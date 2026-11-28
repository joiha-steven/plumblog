//! MCP at `POST /mcp` (docs/ARCHITECTURE.md, MCP): Streamable HTTP, stateless, one JSON-RPC
//! 2.0 message per POST, answered with plain JSON, never SSE. API token only. Tools are the
//! `/api/v1` routes ([`tools`]), each call run by the same `v1::dispatch` as HTTP; resources
//! are the guides. Every error carries `data: {"error": <stable code>, "field"?}`.

mod tools;

use plumblog_auth::json::{self, Value};
use plumblog_model::api::{self, MCP_MAX_BODY};
use plumblog_model::{Error, ErrorCode, Result};
use worker::{Method, Request};

use super::body::{Out, object, quote};
use super::{Ctx, Principal, Reply, principal, site, v1};

include!(concat!(env!("OUT_DIR"), "/guides.rs"));

pub const PATH: &str = "/mcp";

/// Protocol versions spoken, newest first. Nothing here differs between them: no sessions,
/// no SSE, and fields a version lacks (`structuredContent` before 2025-06-18) are ignored.
const VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26"];

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;

const GUIDE_URI: &str = "plumblog://guides/";

/// A JSON-RPC error: its code and our error, whose stable code becomes `data.error`.
#[derive(Debug)]
struct RpcError {
    code: i64,
    error: Error,
}

impl RpcError {
    fn new(code: i64, error: Error) -> Self {
        Self { code, error }
    }

    fn params(field: &str, why: &str) -> Self {
        Self::new(
            INVALID_PARAMS,
            Error::new(ErrorCode::InvalidInput, format!("{field}: {why}")).at(field),
        )
    }
}

/// The client's requested version when we speak it, else our newest.
fn negotiate(requested: &str) -> &'static str {
    VERSIONS
        .iter()
        .find(|v| **v == requested)
        .copied()
        .unwrap_or(VERSIONS[0])
}

/// What one POST carries.
#[derive(Debug, PartialEq, Eq)]
enum Message {
    /// A request: its id as JSON text, method and params.
    Call {
        id: String,
        method: String,
        params: Vec<(String, Value)>,
    },
    /// A notification or a response: accepted, nothing to answer.
    Accepted,
}

/// One JSON-RPC message, strictly: refused before any method runs, so the answer has no id.
fn parse(body: &[u8]) -> std::result::Result<Message, RpcError> {
    let request = |field: &str, why: &str| {
        RpcError::new(
            INVALID_REQUEST,
            Error::new(ErrorCode::InvalidInput, format!("{field}: {why}")).at(field),
        )
    };
    // A batch is not one message; JSON-RPC batching was removed from MCP in 2025-06-18.
    if body.iter().find(|b| !b.is_ascii_whitespace()) == Some(&b'[') {
        return Err(RpcError::new(
            INVALID_REQUEST,
            Error::new(ErrorCode::InvalidInput, "batch: one message per POST"),
        ));
    }
    let members =
        json::parse_document(body, MCP_MAX_BODY).map_err(|e| RpcError::new(PARSE_ERROR, e))?;
    let (mut id, mut method, mut params, mut version, mut reply) = (None, None, None, None, false);
    for (k, v) in members {
        match k.as_str() {
            "jsonrpc" => version = Some(v),
            "id" => id = Some(v),
            "method" => method = Some(v),
            "params" => params = Some(v),
            "result" | "error" => reply = true,
            _ => return Err(request(&k, "unknown member")),
        }
    }
    if version != Some(Value::Str("2.0".to_owned())) {
        return Err(request("jsonrpc", "must be \"2.0\""));
    }
    let method = match method {
        Some(Value::Str(m)) => m,
        Some(_) => return Err(request("method", "string")),
        None if reply => return Ok(Message::Accepted),
        None => return Err(request("method", "required")),
    };
    let id = match id {
        // MCP ids are a string or an integer, never null.
        Some(Value::Int(n)) => n.to_string(),
        Some(Value::Str(s)) => quote(&s),
        Some(_) => return Err(request("id", "string or integer")),
        None => return Ok(Message::Accepted),
    };
    let params = match params {
        None => Vec::new(),
        Some(Value::Obj(p)) => p,
        Some(_) => return Err(request("params", "object")),
    };
    Ok(Message::Call { id, method, params })
}

fn data(e: &Error) -> String {
    let mut out = vec![("error", Out::Str(e.code.as_str().to_owned()))];
    if let Some(f) = &e.field {
        out.push(("field", Out::Str(f.clone())));
    }
    object(&out)
}

fn error_json(id: &str, e: &RpcError) -> String {
    let error = object(&[
        ("code", Out::Int(e.code)),
        // The stable code, not prose: user-facing text lives in locales.
        ("message", Out::Str(e.error.code.as_str().to_owned())),
        ("data", Out::Raw(data(&e.error))),
    ]);
    object(&[
        ("jsonrpc", Out::Str("2.0".to_owned())),
        ("id", Out::Raw(id.to_owned())),
        ("error", Out::Raw(error)),
    ])
}

fn result_json(id: &str, result: String) -> String {
    object(&[
        ("jsonrpc", Out::Str("2.0".to_owned())),
        ("id", Out::Raw(id.to_owned())),
        ("result", Out::Raw(result)),
    ])
}

/// The answer to a failure before any method ran (auth, Origin, size, version, parse): the
/// HTTP status of the code, and a JSON-RPC error with no id.
pub fn error_reply(e: &Error) -> Reply {
    let status = super::status(e.code);
    let code = if status >= 500 {
        INTERNAL_ERROR
    } else {
        INVALID_REQUEST
    };
    let rpc = RpcError::new(code, e.clone());
    Reply::json(status, error_json("null", &rpc))
}

/// Handles one `POST /mcp`. `Err` is a transport failure for [`error_reply`]; JSON-RPC
/// errors of a well-formed request are answered with 200, as the protocol wants.
///
/// # Errors
///
/// `Unauthenticated` without a valid API token (a session cookie never counts),
/// `Forbidden` for a foreign `Origin`, `InvalidInput` for an unsupported
/// `MCP-Protocol-Version`, `LengthRequired`/`BodyTooLarge` for the body; storage errors.
pub async fn handle(ctx: &Ctx, req: &mut Request) -> Result<Reply> {
    let header = |name: &str| req.headers().get(name).ok().flatten();
    // Token only: with no cookie there is no ambient credential, so nothing for a cross-site
    // page to ride on (no CSRF check needed, as for any Bearer request).
    if !principal::has_authorization(req) {
        return Err(Error::new(
            ErrorCode::Unauthenticated,
            "mcp: no Authorization header (API token only)",
        ));
    }
    // DNS rebinding: a page on another origin that reaches a local or proxied server.
    if let Some(got) = header("origin") {
        let want = site::configured(ctx)
            .await?
            .map_or_else(|| ctx.request_origin.clone(), |s| s.origin);
        if got != want {
            return Err(Error::new(
                ErrorCode::Forbidden,
                format!("mcp: Origin {got:?} is not {want:?}"),
            ));
        }
    }
    if let Some(v) = header("mcp-protocol-version")
        && !VERSIONS.contains(&v.as_str())
    {
        return Err(Error::new(
            ErrorCode::InvalidInput,
            format!("mcp: protocol version {v:?} not supported"),
        )
        .at("mcp-protocol-version"));
    }
    // Refuse an oversized body unread, before the token costs a hash and a query.
    super::declared_length(req, MCP_MAX_BODY)?;
    let (p, _) = principal::resolve(ctx, req).await?;
    let body = super::read_body(req, MCP_MAX_BODY).await?;
    let (id, method, params) = match parse(&body) {
        Ok(Message::Call { id, method, params }) => (id, method, params),
        Ok(Message::Accepted) => {
            return Ok(Reply {
                status: 202,
                body: None,
                cookie: None,
            });
        }
        Err(e) => {
            log(ctx, "message", &e.error);
            return Ok(Reply::json(
                super::status(e.error.code),
                error_json("null", &e),
            ));
        }
    };
    match call(ctx, &p, &method, &params).await {
        Ok(result) => Ok(Reply::json(200, result_json(&id, result))),
        Err(e) => {
            log(ctx, &method, &e.error);
            Ok(Reply::json(200, error_json(&id, &e)))
        }
    }
}

fn log(ctx: &Ctx, what: &str, e: &Error) {
    super::log_error(&ctx.request_id, "POST", &format!("{PATH} {what}"), e);
}

fn member<'a>(params: &'a [(String, Value)], key: &str) -> Option<&'a Value> {
    params.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn str_param<'a>(
    params: &'a [(String, Value)],
    key: &str,
) -> std::result::Result<&'a str, RpcError> {
    match member(params, key) {
        Some(Value::Str(s)) => Ok(s),
        Some(_) => Err(RpcError::params(key, "string")),
        None => Err(RpcError::params(key, "required")),
    }
}

/// Runs one method. Unknown `params` members are ignored: the spec adds optional ones
/// (`_meta`, `cursor`) that change nothing here.
async fn call(
    ctx: &Ctx,
    p: &Principal,
    method: &str,
    params: &[(String, Value)],
) -> std::result::Result<String, RpcError> {
    match method {
        "initialize" => {
            let version = negotiate(str_param(params, "protocolVersion")?);
            Ok(initialize(version))
        }
        "ping" => Ok("{}".to_owned()),
        "tools/list" => Ok(tools::list(|r| v1::access(p, r).is_ok())),
        "tools/call" => tool_call(ctx, p, params).await,
        "resources/list" => Ok(resources()),
        "resources/read" => read_resource(str_param(params, "uri")?),
        _ => Err(RpcError::new(
            METHOD_NOT_FOUND,
            Error::new(ErrorCode::RouteNotFound, format!("method {method:?}")).at("method"),
        )),
    }
}

fn initialize(version: &str) -> String {
    let unchanging = || Out::Raw(object(&[("listChanged", Out::Bool(false))]));
    object(&[
        ("protocolVersion", Out::Str(version.to_owned())),
        (
            "capabilities",
            Out::Raw(object(&[
                ("tools", unchanging()),
                ("resources", unchanging()),
            ])),
        ),
        (
            "serverInfo",
            Out::Raw(object(&[
                ("name", Out::Str("plumblog".to_owned())),
                ("version", Out::Str(env!("CARGO_PKG_VERSION").to_owned())),
            ])),
        ),
    ])
}

fn worker_method(m: api::Method) -> Method {
    match m {
        api::Method::Get => Method::Get,
        api::Method::Post => Method::Post,
        api::Method::Put => Method::Put,
        api::Method::Patch => Method::Patch,
        api::Method::Delete => Method::Delete,
    }
}

/// A tool's result. A refusal of the call itself (forbidden, invalid, conflict) is a result
/// with `isError`, not a JSON-RPC error, so the model sees it and can fix its call.
fn tool_result(json: &str, is_error: bool) -> String {
    let text = object(&[
        ("type", Out::Str("text".to_owned())),
        ("text", Out::Str(json.to_owned())),
    ]);
    let mut out = vec![("content", Out::Raw(format!("[{text}]")))];
    // `structuredContent` must be an object; every API answer is one.
    if json.starts_with('{') {
        out.push(("structuredContent", Out::Raw(json.to_owned())));
    }
    out.push(("isError", Out::Bool(is_error)));
    object(&out)
}

async fn tool_call(
    ctx: &Ctx,
    p: &Principal,
    params: &[(String, Value)],
) -> std::result::Result<String, RpcError> {
    let name = str_param(params, "name")?;
    let route = tools::route(name).ok_or_else(|| RpcError::params("name", "unknown tool"))?;
    let args = match member(params, "arguments") {
        None => &[][..],
        Some(Value::Obj(a)) => a.as_slice(),
        Some(_) => return Err(RpcError::params("arguments", "object")),
    };
    let what = format!("tools/call {name}");
    let outcome = match tools::split(route, args) {
        Ok(r) => {
            let m = worker_method(route.method);
            v1::dispatch(ctx, p, &m, &r.path, &r.query, r.body).await
        }
        Err(e) => Err(e),
    };
    Ok(match outcome {
        Ok(reply) => {
            let json = reply.body.unwrap_or_else(|| "{}".to_owned());
            if reply.status >= 400 {
                worker::console_warn!(
                    "[{}] {PATH} {what} -> {} {json}",
                    ctx.request_id.as_str(),
                    reply.status
                );
            }
            tool_result(&json, reply.status >= 400)
        }
        Err(e) => {
            log(ctx, &what, &e);
            tool_result(&data(&e), true)
        }
    })
}

fn guide_uri(name: &str) -> String {
    format!("{GUIDE_URI}{name}")
}

fn guide_title(text: &str) -> &str {
    // The build script checked that every guide starts with `# <title>`.
    text.lines()
        .next()
        .and_then(|l| l.strip_prefix("# "))
        .unwrap_or_default()
        .trim()
}

fn resources() -> String {
    let items: Vec<String> = GUIDES
        .iter()
        .map(|(name, summary, text)| {
            object(&[
                ("uri", Out::Str(guide_uri(name))),
                ("name", Out::Str((*name).to_owned())),
                ("title", Out::Str(guide_title(text).to_owned())),
                ("description", Out::Str((*summary).to_owned())),
                ("mimeType", Out::Str("text/markdown".to_owned())),
            ])
        })
        .collect();
    object(&[("resources", Out::Raw(format!("[{}]", items.join(","))))])
}

fn read_resource(uri: &str) -> std::result::Result<String, RpcError> {
    let (_, _, text) = uri
        .strip_prefix(GUIDE_URI)
        .and_then(|name| GUIDES.iter().find(|(n, _, _)| *n == name))
        .ok_or_else(|| {
            RpcError::new(
                INVALID_PARAMS,
                Error::new(ErrorCode::NotFound, format!("resource {uri:?}")).at("uri"),
            )
        })?;
    let content = object(&[
        ("uri", Out::Str(uri.to_owned())),
        ("mimeType", Out::Str("text/markdown".to_owned())),
        ("text", Out::Str((*text).to_owned())),
    ]);
    Ok(object(&[("contents", Out::Raw(format!("[{content}]")))]))
}

#[cfg(test)]
mod tests {
    use super::{
        GUIDES, INVALID_PARAMS, INVALID_REQUEST, Message, PARSE_ERROR, negotiate, parse,
        read_resource, resources,
    };
    use plumblog_auth::json::Value;
    use plumblog_model::ErrorCode;
    use plumblog_model::api::MCP_MAX_BODY;

    #[test]
    fn negotiates_versions() {
        assert_eq!(negotiate("2025-06-18"), "2025-06-18");
        assert_eq!(negotiate("2025-03-26"), "2025-03-26");
        assert_eq!(negotiate("2025-11-25"), "2025-11-25");
        assert_eq!(negotiate("2024-11-05"), "2025-11-25");
        assert_eq!(negotiate(""), "2025-11-25");
    }

    fn refused(body: &str) -> (i64, ErrorCode, Option<String>) {
        let e = parse(body.as_bytes()).unwrap_err();
        (e.code, e.error.code, e.error.field)
    }

    #[test]
    fn parses_one_message() {
        assert_eq!(
            parse(br#"{"jsonrpc":"2.0","id":"a\"b","method":"ping","params":{"_meta":{}}}"#)
                .unwrap(),
            Message::Call {
                id: r#""a\"b""#.to_owned(),
                method: "ping".to_owned(),
                params: vec![("_meta".to_owned(), Value::Obj(Vec::new()))],
            }
        );
        assert_eq!(
            parse(br#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#).unwrap(),
            Message::Call {
                id: "7".to_owned(),
                method: "tools/list".to_owned(),
                params: Vec::new(),
            }
        );
        // Notifications and responses need no answer.
        assert_eq!(
            parse(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).unwrap(),
            Message::Accepted
        );
        assert_eq!(
            parse(br#"{"jsonrpc":"2.0","id":1,"result":{}}"#).unwrap(),
            Message::Accepted
        );

        let invalid = |f: &str| (INVALID_REQUEST, ErrorCode::InvalidInput, Some(f.to_owned()));
        assert_eq!(
            refused(r#" [{"jsonrpc":"2.0","id":1,"method":"ping"}]"#).0,
            INVALID_REQUEST
        );
        assert_eq!(
            refused(r#"{"jsonrpc":"2.0","id":1,"method":"ping""#).0,
            PARSE_ERROR
        );
        assert_eq!(
            refused(r#"{"jsonrpc":"2.0","id":1.5,"method":"ping"}"#).0,
            PARSE_ERROR
        );
        assert_eq!(
            refused(r#"{"jsonrpc":"1.0","id":1,"method":"ping"}"#),
            invalid("jsonrpc")
        );
        assert_eq!(refused(r#"{"id":1,"method":"ping"}"#), invalid("jsonrpc"));
        assert_eq!(
            refused(r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#),
            invalid("id")
        );
        assert_eq!(
            refused(r#"{"jsonrpc":"2.0","id":1,"method":2}"#),
            invalid("method")
        );
        assert_eq!(refused(r#"{"jsonrpc":"2.0","id":1}"#), invalid("method"));
        assert_eq!(
            refused(r#"{"jsonrpc":"2.0","id":1,"method":"ping","params":[]}"#),
            invalid("params")
        );
        assert_eq!(
            refused(r#"{"jsonrpc":"2.0","id":1,"method":"ping","x":1}"#),
            invalid("x")
        );
        // Size and value caps are the strict parser's.
        let many = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"ping","params":{{"a":[{}]}}}}"#,
            vec!["1"; 5000].join(",")
        );
        assert!(many.len() < MCP_MAX_BODY);
        assert_eq!(refused(&many).0, PARSE_ERROR);
    }

    #[test]
    fn serves_guides() {
        assert!(!GUIDES.is_empty());
        let list = resources();
        for (name, summary, _) in GUIDES {
            assert!(list.contains(&format!("\"uri\":\"plumblog://guides/{name}\"")));
            assert!(list.contains(&super::quote(summary)));
        }
        let (name, _, _) = GUIDES[0];
        assert!(read_resource(&format!("plumblog://guides/{name}")).is_ok());
        let e = read_resource("plumblog://guides/nope").unwrap_err();
        assert_eq!(e.code, INVALID_PARAMS);
        assert_eq!(e.error.code, ErrorCode::NotFound);
    }
}
