//! MCP tools are the `/api/v1` routes: names from [`api::tool_name`], schemas from the
//! route's fields. A call is split back into the path, query and body an HTTP client would
//! send, so it runs through the same `v1::dispatch` and its checks. No I/O here.

use plumblog_auth::json::Value;
use plumblog_model::api::{self, Field, In, Method, ROUTES, Route, Type};
use plumblog_model::{Error, ErrorCode, Result};

use crate::api::body::{Out, object, quote};
use crate::api::v1::{access_text, write_json};

/// The route a tool name calls.
pub fn route(name: &str) -> Option<&'static Route> {
    ROUTES.iter().find(|r| api::tool_name(r.id) == Some(name))
}

fn list_json(items: &[String]) -> Out {
    Out::Raw(format!("[{}]", items.join(",")))
}

/// Generated from the table, no prose: `PATCH /api/v1/posts/{id}; needs post.write; may need
/// post.publish, post.edit_any`.
fn description(r: &Route) -> String {
    let mut d = format!(
        "{} {}; needs {}",
        r.method.as_str(),
        r.path,
        access_text(r.access)
    );
    if !r.also.is_empty() {
        let also: Vec<&str> = r.also.iter().map(|p| p.as_str()).collect();
        d.push_str("; may need ");
        d.push_str(&also.join(", "));
    }
    d
}

fn input_schema(fields: &[Field]) -> String {
    let props: Vec<(&str, Out)> = fields
        .iter()
        .map(|f| (f.name, Out::Raw(f.ty.json_schema().to_owned())))
        .collect();
    let required: Vec<String> = fields
        .iter()
        .filter(|f| f.required)
        .map(|f| quote(f.name))
        .collect();
    object(&[
        ("type", Out::Str("object".to_owned())),
        ("properties", Out::Raw(object(&props))),
        ("required", list_json(&required)),
        ("additionalProperties", Out::Bool(false)),
    ])
}

/// `readOnlyHint` for reads, `destructiveHint` for deletes. Other writes leave
/// `destructiveHint` at the spec's default (true): an update overwrites what was there.
fn annotations(r: &Route) -> String {
    let mut a = vec![("readOnlyHint", Out::Bool(r.method == Method::Get))];
    if r.method == Method::Delete {
        a.push(("destructiveHint", Out::Bool(true)));
    }
    object(&a)
}

fn tool_json(name: &str, r: &Route) -> String {
    object(&[
        ("name", Out::Str(name.to_owned())),
        ("description", Out::Str(description(r))),
        ("inputSchema", Out::Raw(input_schema(r.fields))),
        ("annotations", Out::Raw(annotations(r))),
    ])
}

/// `tools/list` result: the tools whose route `allowed` says the caller may call (its
/// `access`; `also` depends on the input and is checked at call time).
pub fn list(allowed: impl Fn(&Route) -> bool) -> String {
    let tools: Vec<String> = ROUTES
        .iter()
        .filter(|r| allowed(r))
        .filter_map(|r| api::tool_name(r.id).map(|name| tool_json(name, r)))
        .collect();
    object(&[("tools", list_json(&tools))])
}

/// A tool call as the HTTP request it stands for.
#[derive(Debug, PartialEq, Eq)]
pub struct Request {
    pub path: String,
    pub query: Vec<(String, String)>,
    pub body: Vec<u8>,
}

fn invalid(field: &str, why: &str) -> Error {
    Error::new(ErrorCode::InvalidInput, format!("{field}: {why}")).at(field)
}

/// A path value: one non-empty segment. Ids must be JSON integers, as in a body.
fn path_value(f: &Field, v: &Value) -> Result<String> {
    match (f.ty, v) {
        (Type::Int, Value::Int(n)) => Ok(n.to_string()),
        (Type::Str, Value::Str(s)) if !s.is_empty() && !s.contains('/') => Ok(s.clone()),
        (Type::Str, Value::Str(_)) => Err(invalid(f.name, "one non-empty path segment")),
        _ => Err(invalid(f.name, f.ty.as_str())),
    }
}

/// A query value as the text an HTTP client would send; `Input::parse` reads it back.
fn query_value(f: &Field, v: &Value) -> Result<String> {
    match (f.ty, v) {
        (Type::Str, Value::Str(s)) => Ok(s.clone()),
        (Type::Int, Value::Int(n)) => Ok(n.to_string()),
        (Type::Bool, Value::Bool(b)) => Ok(b.to_string()),
        _ => Err(invalid(f.name, f.ty.as_str())),
    }
}

/// Splits a tool's `arguments` by where each field goes.
///
/// # Errors
///
/// `InvalidInput` naming the argument for one the route does not have, a path or query value
/// of the wrong type, or a missing path value; `BodyTooLarge` when the body is over the
/// route's cap. Body values are type-checked later by `Input::parse`, as for HTTP.
pub fn split(r: &'static Route, args: &[(String, Value)]) -> Result<Request> {
    let mut path_values: Vec<(&str, String)> = Vec::new();
    let mut query = Vec::new();
    let mut body: Vec<(&str, &Value)> = Vec::new();
    for (name, v) in args {
        let f = r
            .fields
            .iter()
            .find(|f| f.name == name)
            .ok_or_else(|| invalid(name, "unknown field"))?;
        match f.at {
            In::Path => path_values.push((f.name, path_value(f, v)?)),
            In::Query => query.push((f.name.to_owned(), query_value(f, v)?)),
            In::Body => body.push((f.name, v)),
        }
    }
    let mut path = String::new();
    for (i, seg) in r.path.split('/').enumerate() {
        if i > 0 {
            path.push('/');
        }
        match seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
            Some(name) => {
                let (_, v) = path_values
                    .iter()
                    .find(|(n, _)| *n == name)
                    .ok_or_else(|| invalid(name, "required"))?;
                path.push_str(v);
            }
            None => path.push_str(seg),
        }
    }
    // The built path must lead back to this route and no other.
    if !api::find(r.method, &path).is_ok_and(|(found, _)| found.id == r.id) {
        return Err(Error::new(
            ErrorCode::InvalidInput,
            format!("tool path {path:?} does not match {}", r.path),
        ));
    }
    let has_body_fields = r.fields.iter().any(|f| f.at == In::Body);
    let body = if has_body_fields {
        let mut out = String::from("{");
        for (i, (name, v)) in body.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push_str(&quote(name));
            out.push(':');
            write_json(&mut out, v);
        }
        out.push('}');
        out.into_bytes()
    } else {
        Vec::new()
    };
    if body.len() > r.max_body {
        return Err(Error::new(
            ErrorCode::BodyTooLarge,
            format!("tool body of {} bytes > {}", body.len(), r.max_body),
        ));
    }
    Ok(Request { path, query, body })
}

#[cfg(test)]
mod tests {
    use super::{Request, list, route, split};
    use plumblog_auth::json::{Value, parse_document};
    use plumblog_model::ErrorCode;
    use plumblog_model::api::{MAX_POST_BODY, Method, ROUTES, tool_name};

    fn args(json: &str) -> Vec<(String, Value)> {
        parse_document(json.as_bytes(), usize::MAX).unwrap()
    }

    fn call(tool: &str, json: &str) -> Result<Request, (ErrorCode, Option<String>)> {
        split(route(tool).unwrap(), &args(json)).map_err(|e| (e.code, e.field))
    }

    #[test]
    fn splits_by_place() {
        let r = call(
            "post_update",
            r#"{"id":7,"expected_version":3,"title":"T","tags":[1,2]}"#,
        )
        .unwrap();
        assert_eq!(r.path, "/api/v1/posts/7");
        assert!(r.query.is_empty());
        assert_eq!(
            String::from_utf8(r.body).unwrap(),
            r#"{"expected_version":3,"title":"T","tags":[1,2]}"#
        );

        let r = call("post_list", r#"{"limit":5,"status":"draft","tag":2}"#).unwrap();
        assert_eq!(r.path, "/api/v1/posts");
        assert_eq!(
            r.query,
            [("limit", "5"), ("status", "draft"), ("tag", "2")]
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
        );
        assert!(r.body.is_empty());

        let r = call("media_list", r#"{"missing_alt":true}"#).unwrap();
        assert_eq!(r.query, [("missing_alt".to_owned(), "true".to_owned())]);

        let r = call(
            "plugin_option_put",
            r#"{"name":"toc","key":"depth","value":{"a":[1,null,"x"]}}"#,
        )
        .unwrap();
        assert_eq!(r.path, "/api/v1/plugins/toc/options/depth");
        assert_eq!(
            String::from_utf8(r.body).unwrap(),
            r#"{"value":{"a":[1,null,"x"]}}"#
        );

        // A write without body fields sends no body; one with body fields always sends an
        // object, so a missing required field is reported by name.
        assert!(call("post_trash", r#"{"id":1}"#).unwrap().body.is_empty());
        assert_eq!(call("tag_create", "{}").unwrap().body, b"{}");
        assert!(call("me", "{}").unwrap().body.is_empty());
    }

    #[test]
    fn refuses_bad_arguments() {
        let bad = |tool: &str, json: &str, field: &str| {
            assert_eq!(
                call(tool, json).unwrap_err(),
                (ErrorCode::InvalidInput, Some(field.to_owned())),
                "{tool} {json}"
            );
        };
        bad("post_get", r#"{"id":7,"nope":1}"#, "nope");
        bad("post_get", r#"{"id":"7"}"#, "id");
        bad("post_get", "{}", "id");
        bad("setting_put", r#"{"key":"a/b","value":1}"#, "key");
        bad("setting_put", r#"{"key":"","value":1}"#, "key");
        bad("post_list", r#"{"limit":"5"}"#, "limit");
        bad("post_list", r#"{"status":null}"#, "status");
        bad("media_list", r#"{"unused":"true"}"#, "unused");

        let big = format!(r#"{{"value":"{}","key":"k"}}"#, "x".repeat(9000));
        assert_eq!(
            call("setting_put", &big).unwrap_err().0,
            ErrorCode::BodyTooLarge
        );
        let post = format!(
            r#"{{"lang":"en","slug":"s","title":"t","markdown":"{}"}}"#,
            "x".repeat(MAX_POST_BODY)
        );
        assert_eq!(
            call("post_create", &post).unwrap_err().0,
            ErrorCode::BodyTooLarge
        );
    }

    #[test]
    fn lists_tools_with_schemas() {
        let all = list(|_| true);
        assert_eq!(
            all.matches(r#"{"name":""#).count(),
            ROUTES.iter().filter(|r| tool_name(r.id).is_some()).count()
        );
        assert!(all.contains(
            r#"{"name":"post_update","description":"PATCH /api/v1/posts/{id}; needs post.write; may need post.publish, post.edit_any","inputSchema":{"type":"object","properties":{"id":{"type":"integer"},"expected_version":{"type":"integer"},"#
        ));
        assert!(all.contains(r#""cover_media_id":{"type":["integer","null"]}"#));
        assert!(all.contains(
            r#""tags":{"type":"array","items":{"type":"integer"}}},"required":["id","expected_version"],"additionalProperties":false},"annotations":{"readOnlyHint":false}}"#
        ));
        assert!(all.contains(r#""annotations":{"readOnlyHint":false,"destructiveHint":true}"#));
        assert!(!all.contains("\"reference\"") && !all.contains("media_upload"));
        let reads = list(|r| r.method == Method::Get);
        assert!(reads.contains("\"post_get\"") && !reads.contains("\"post_create\""));
    }
}
