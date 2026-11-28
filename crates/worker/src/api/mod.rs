//! The JSON API under `/api/`. Every request goes through the same gate, in order:
//! CSRF (`Origin`, skipped for `Authorization: Bearer`, which a browser never sends on its
//! own), principal (who is asking), then the route, which checks its permission with
//! [`Principal::require`]. `/api/auth/*` is session only. Errors answer `{"error":"<stable code>"}` with a status and
//! log the detail with the request id; no user-facing text is produced here.

mod auth;
pub(crate) mod body;
mod fields;
pub(crate) mod limit;
mod mcp;
mod passkey;
mod principal;
pub(crate) mod site;
mod tokens;
mod v1;

use plumblog_auth::Crypto;
use plumblog_auth::base64url;
use plumblog_model::UserId;
use plumblog_model::{Error, ErrorCode, RequestId, Result};
use plumblog_store::LogStore;
use plumblog_syslog::{Event, EventKind, Scope};
use worker::{Env, Method, Request, Response};

use crate::d1::D1Store;
use crate::webcrypto::WebCrypto;
pub use mcp::PATH as MCP_PATH;
pub use principal::{Principal, resolve_read_only};

pub const PREFIX: &str = "/api/";

/// What every handler gets: storage, crypto and facts about this request.
pub struct Ctx {
    pub store: D1Store,
    pub crypto: WebCrypto,
    pub env: Env,
    pub now: i64,
    pub request_id: RequestId,
    /// `CF-Connecting-IP`, used only for the rate-limit key (hashed), never stored or logged.
    pub ip: Option<String>,
    /// This request's own origin (from the Host header): the CSRF check's reference while the
    /// site has no configured origin. Never used for passkeys or Turnstile (see `site`).
    pub request_origin: String,
}

impl Ctx {
    /// An event of this request, by `actor`. Built through [`Scope`] so the request id is
    /// always set; subject and fields are redacted by the event builder.
    pub fn event(&self, kind: EventKind, actor: Option<UserId>) -> Event {
        Scope::new(self.request_id.clone(), actor).event(kind, u64::try_from(self.now).unwrap_or(0))
    }

    /// Appends one system log entry. A failing log never fails the request; it goes to the
    /// Worker log instead.
    pub async fn syslog(&self, event: Event) {
        let kind = event.kind();
        if let Err(e) = self.store.append(event).await {
            worker::console_error!(
                "[{}] syslog append {} failed: {e}",
                self.request_id.as_str(),
                kind.as_str()
            );
        }
    }
}

/// A successful answer.
pub struct Reply {
    pub status: u16,
    /// JSON object text; `None` for 204.
    pub body: Option<String>,
    /// `Set-Cookie` value, when the session changed.
    pub cookie: Option<String>,
    /// Stored bytes instead of JSON (a media original); `body` is then `None`.
    pub raw: Option<Raw>,
}

/// Bytes handed to the runtime as a stream (an R2 object body), so they never pass through
/// Wasm memory.
pub struct Raw {
    pub body: worker::ResponseBody,
    pub mime: String,
    pub length: u64,
}

impl Reply {
    #[must_use]
    pub fn json(status: u16, body: String) -> Self {
        Self {
            status,
            body: Some(body),
            cookie: None,
            raw: None,
        }
    }

    #[must_use]
    pub fn empty() -> Self {
        Self {
            status: 204,
            body: None,
            cookie: None,
            raw: None,
        }
    }

    /// 200 with `raw` as the body.
    #[must_use]
    pub fn raw(raw: Raw) -> Self {
        Self {
            status: 200,
            body: None,
            cookie: None,
            raw: Some(raw),
        }
    }

    #[must_use]
    pub fn with_cookie(mut self, cookie: String) -> Self {
        self.cookie = Some(cookie);
        self
    }
}

/// HTTP status for each stable code. Exhaustive, so a new code must pick one.
fn status(code: ErrorCode) -> u16 {
    match code {
        ErrorCode::InvalidInput
        | ErrorCode::PostPublishedInFuture
        | ErrorCode::PasskeyInvalid
        | ErrorCode::WebhookUrlBlocked
        | ErrorCode::CommentName
        | ErrorCode::CommentBody
        | ErrorCode::CommentLinks
        | ErrorCode::CommentForm => 400,
        ErrorCode::Unauthenticated | ErrorCode::InvalidCredentials => 401,
        // A signed-in user must prove it again (adding or removing a passkey): not a dead
        // session, so not 401.
        ErrorCode::Forbidden
        | ErrorCode::SetupInvalid
        | ErrorCode::ResetExpired
        | ErrorCode::TurnstileRequired
        | ErrorCode::ReauthRequired
        | ErrorCode::CommentsClosed
        | ErrorCode::CommentCheckFailed => 403,
        ErrorCode::PostNotFound
        | ErrorCode::UserNotFound
        | ErrorCode::NotFound
        | ErrorCode::RouteNotFound => 404,
        ErrorCode::Conflict | ErrorCode::ThemeUnavailable => 409,
        ErrorCode::InviteInvalid => 410,
        ErrorCode::LengthRequired => 411,
        // A size cap like the byte cap: the client must send less, not fix a field.
        ErrorCode::BodyTooLarge | ErrorCode::TooComplex => 413,
        ErrorCode::Locked | ErrorCode::CommentRateLimited => 429,
        // The pages.* codes are only ever logged; 500 if an API answer ever carries one.
        ErrorCode::Storage
        | ErrorCode::Internal
        | ErrorCode::WebhookSecretUnreadable
        | ErrorCode::PageStoreFailed
        | ErrorCode::PagePrerenderFailed
        | ErrorCode::PageCleanupFailed => 500,
        // The receiver failed, not this server.
        ErrorCode::WebhookDeliveryFailed => 502,
        // The site is not set up for this yet (no configured origin, no WEBHOOK_KEY): not the
        // client's fault.
        ErrorCode::OriginMissing
        | ErrorCode::WebhookKeyInvalid
        | ErrorCode::CommentsUnavailable => 503,
    }
}

fn request_id(req: &Request, crypto: Option<&WebCrypto>) -> RequestId {
    if let Some(id) = req
        .headers()
        .get("cf-ray")
        .ok()
        .flatten()
        .and_then(|r| RequestId::parse(&r))
    {
        return id;
    }
    let mut b = [0u8; 8];
    let random = crypto.is_some_and(|c| c.random_bytes(&mut b).is_ok());
    let text = if random {
        base64url::hex(&b)
    } else {
        "no-request-id".to_owned()
    };
    RequestId::parse(&text).unwrap_or_else(|| {
        // Both candidates above are valid ids; this branch only satisfies the type.
        RequestId::parse("x").expect("literal is a valid request id")
    })
}

fn respond(reply: Reply) -> worker::Result<Response> {
    if let Some(raw) = reply.raw {
        let mut res = Response::from_body(raw.body)?.with_status(reply.status);
        let h = res.headers_mut();
        h.set("content-type", &raw.mime)?;
        h.set("content-length", &raw.length.to_string())?;
        // Read with a session or token: no shared cache may keep it, nor the browser's.
        h.set("cache-control", "private, no-store")?;
        h.set("x-content-type-options", "nosniff")?;
        // Defence in depth: even if a browser renders the bytes as a document, it runs
        // sandboxed, with no scripts and an opaque origin.
        h.set("content-security-policy", "sandbox")?;
        if let Some(c) = reply.cookie {
            h.set("set-cookie", &c)?;
        }
        return Ok(res);
    }
    let has_body = reply.body.is_some();
    let mut res = match reply.body {
        Some(b) => Response::ok(b)?.with_status(reply.status),
        None => Response::empty()?.with_status(reply.status),
    };
    let h = res.headers_mut();
    if has_body {
        h.set("content-type", "application/json; charset=utf-8")?;
    }
    // Answers depend on the cookie; never let a cache keep one.
    h.set("cache-control", "no-store")?;
    if let Some(c) = reply.cookie {
        h.set("set-cookie", &c)?;
    }
    Ok(res)
}

fn error_reply(e: &Error) -> Reply {
    let mut out = vec![("error", body::Out::Str(e.code.as_str().to_owned()))];
    if let Some(f) = &e.field {
        out.push(("field", body::Out::Str(f.clone())));
    }
    Reply::json(status(e.code), body::object(&out))
}

/// Non-GET requests must come from this site's own pages: `Origin` equal to the configured site
/// origin, or the request's own one while none is configured (the `SameSite=Strict` cookie is
/// the first wall, this is the second).
fn check_origin(req: &Request, method: &Method, want: &str) -> Result<()> {
    if matches!(method, Method::Get | Method::Head) {
        return Ok(());
    }
    match req.headers().get("origin").ok().flatten() {
        Some(got) if got == want => Ok(()),
        Some(got) => Err(Error::new(
            ErrorCode::Forbidden,
            format!("csrf: Origin {got:?} is not {want:?}"),
        )),
        None => Err(Error::new(ErrorCode::Forbidden, "csrf: Origin missing")),
    }
}

/// Whether a body comes, refusing it unread when `Content-Length` is over `max` (413) or
/// missing on a body (411).
fn declared_length(req: &Request, max: usize) -> Result<bool> {
    match req.headers().get("content-length").ok().flatten() {
        Some(v) => {
            let n: usize = v.trim().parse().map_err(|_| {
                Error::new(ErrorCode::InvalidInput, format!("Content-Length {v:?}"))
            })?;
            if n > max {
                return Err(Error::new(
                    ErrorCode::BodyTooLarge,
                    format!("body of {n} bytes > {max}"),
                ));
            }
            Ok(true)
        }
        None if req.inner().body().is_some() => Err(Error::new(
            ErrorCode::LengthRequired,
            "body without Content-Length",
        )),
        None => Ok(false),
    }
}

/// The request body, refused before it is read when `Content-Length` says it is over `max`
/// (413), or when a body comes without one (chunked: its size is unknown until read, 411).
/// The runtime frames the body by `Content-Length`, so it cannot be longer; the length is
/// checked again after reading all the same.
async fn read_body(req: &mut Request, max: usize) -> Result<Vec<u8>> {
    let too_large = |n: usize| {
        Error::new(
            ErrorCode::BodyTooLarge,
            format!("body of {n} bytes > {max}"),
        )
    };
    if !declared_length(req, max)? {
        return Ok(Vec::new());
    }
    let bytes = req
        .bytes()
        .await
        .map_err(|e| Error::new(ErrorCode::InvalidInput, format!("body: {e}")))?;
    if bytes.len() > max {
        return Err(too_large(bytes.len()));
    }
    Ok(bytes)
}

/// One error line in the Worker log, with the request id: `error` for our failures (5xx),
/// `warn` for the client's.
fn log_error(rid: &RequestId, method: &str, what: &str, e: &Error) {
    let s = status(e.code);
    let line = format!("[{}] {method} {what} -> {s} {e}", rid.as_str());
    if s >= 500 {
        worker::console_error!("{line}");
    } else {
        worker::console_warn!("{line}");
    }
}

/// Handles one `/api/` or `/mcp` request. Never returns a non-JSON error for a failure of ours.
pub async fn handle(mut req: Request, env: &Env) -> worker::Result<Response> {
    let crypto = WebCrypto::new();
    let rid = request_id(&req, crypto.as_ref().ok());
    let method = req.method();
    let path = req.path();
    let is_mcp = path == mcp::PATH;
    if is_mcp && method != Method::Post {
        // Stateless and JSON only: no SSE stream to GET, no session to DELETE. There is no
        // "method not allowed" code; `/api/v1` answers a wrong method `api.not_found` too.
        let e = Error::new(ErrorCode::RouteNotFound, "mcp: POST only");
        log_error(&rid, method.as_ref(), &path, &e);
        let mut reply = mcp::error_reply(&e);
        reply.status = 405;
        let mut res = respond(reply)?;
        res.headers_mut().set("allow", "POST")?;
        return Ok(res);
    }
    // The gate's cookie (renewal, or clearing a dead session) goes out on errors too.
    let mut gate_cookie = None;
    let result = async {
        let crypto = crypto?;
        let request_origin = req
            .url()
            .map_err(|e| Error::new(ErrorCode::Internal, format!("request url: {e}")))?
            .origin()
            .ascii_serialization();
        let ctx = Ctx {
            store: D1Store::new(
                env.d1("DB")
                    .map_err(|e| Error::new(ErrorCode::Internal, format!("D1 binding: {e}")))?,
            ),
            crypto,
            env: env.clone(),
            now: i64::try_from(worker::Date::now().as_millis())
                .map_err(|_| Error::new(ErrorCode::Internal, "clock out of range"))?,
            request_id: rid.clone(),
            ip: req.headers().get("cf-connecting-ip").ok().flatten(),
            request_origin,
        };
        if is_mcp {
            return mcp::handle(&ctx, &mut req).await;
        }
        // A token request carries its credential explicitly, and with no CORS a cross-site
        // page cannot send an `Authorization` header; it also never uses the cookie.
        if !matches!(method, Method::Get | Method::Head) && !principal::has_authorization(&req) {
            let want = site::configured(&ctx)
                .await?
                .map_or_else(|| ctx.request_origin.clone(), |s| s.origin);
            check_origin(&req, &method, &want)?;
        }
        let max = v1::max_body(&method, &path);
        let raw = v1::raw_body(&method, &path);
        let body = if matches!(method, Method::Get | Method::Head) {
            Vec::new()
        } else if raw {
            declared_length(&req, max)?;
            Vec::new()
        } else {
            read_body(&mut req, max).await?
        };
        let (principal, cookie) = principal::resolve(&ctx, &req).await?;
        gate_cookie = cookie;
        if path.starts_with(v1::PREFIX) {
            let body = if raw {
                v1::check_access(&principal, &method, &path)?;
                read_body(&mut req, max).await?
            } else {
                body
            };
            let url = req
                .url()
                .map_err(|e| Error::new(ErrorCode::Internal, format!("request url: {e}")))?;
            let query: Vec<(String, String)> = url.query_pairs().into_owned().collect();
            return v1::dispatch(&ctx, &principal, &method, &path, &query, body).await;
        }
        route(&ctx, &principal, &method, &path, &body).await
    }
    .await;
    let mut reply = match result {
        Ok(r) => r,
        Err(e) => {
            log_error(&rid, method.as_ref(), &path, &e);
            if is_mcp {
                mcp::error_reply(&e)
            } else {
                error_reply(&e)
            }
        }
    };
    // A handler that changed the session (sign-in, sign-out, rotation) decides the cookie.
    if reply.cookie.is_none() {
        reply.cookie = gate_cookie;
    }
    let unauthenticated = reply.status == 401;
    let mut res = respond(reply)?;
    // RFC 6750 names the scheme. No `resource_metadata`: there is no OAuth server, only a
    // token the owner pastes into the client.
    if is_mcp && unauthenticated {
        res.headers_mut().set("www-authenticate", "Bearer")?;
    }
    Ok(res)
}

async fn route(
    ctx: &Ctx,
    p: &Principal,
    method: &Method,
    path: &str,
    body: &[u8],
) -> Result<Reply> {
    if let Principal::Token { name, .. } = p
        && path.starts_with(AUTH_PREFIX)
    {
        return Err(Error::new(
            ErrorCode::Forbidden,
            format!("API token {name:?} on {path}: /api/auth/ is session only"),
        ));
    }
    match (method, path) {
        (Method::Get, "/api/auth/params") => auth::params(ctx).await,
        (Method::Get, "/api/auth/me") => auth::me(ctx, p).await,
        (Method::Patch, "/api/auth/me") => auth::update_me(ctx, p, body).await,
        (Method::Post, "/api/auth/setup") => auth::setup(ctx, p, body).await,
        (Method::Post, "/api/auth/reset") => auth::redeem_reset(ctx, p, body).await,
        (Method::Post, "/api/auth/login") => auth::login(ctx, p, body).await,
        (Method::Post, "/api/auth/logout") => auth::logout(ctx, p).await,
        (Method::Post, "/api/auth/invite/accept") => auth::accept_invite(ctx, p, body).await,
        (Method::Post, "/api/auth/password") => auth::change_password(ctx, p, body).await,
        (Method::Post, "/api/auth/passkey/register/options") => {
            passkey::register_options(ctx, p, body).await
        }
        (Method::Post, "/api/auth/passkey/register/verify") => {
            passkey::register_verify(ctx, p, body).await
        }
        (Method::Post, "/api/auth/passkey/login/options") => {
            passkey::login_options(ctx, body).await
        }
        (Method::Post, "/api/auth/passkey/login/verify") => {
            passkey::login_verify(ctx, p, body).await
        }
        (Method::Get, "/api/auth/passkey") => passkey::list(ctx, p).await,
        (Method::Delete, _) if path.starts_with(passkey::ITEM_PREFIX) => {
            passkey::delete(ctx, p, &path[passkey::ITEM_PREFIX.len()..], body).await
        }
        (Method::Get, tokens::COLLECTION) => tokens::list(ctx, p).await,
        (Method::Post, tokens::COLLECTION) => tokens::create(ctx, p, body).await,
        (Method::Delete, _) if path.starts_with(tokens::ITEM_PREFIX) => {
            tokens::revoke(ctx, p, &path[tokens::ITEM_PREFIX.len()..]).await
        }
        (Method::Get, PROBE_OWNER) if probe_enabled(&ctx.env) => {
            p.require(plumblog_model::Permission::ManageSettings)?;
            Ok(Reply::empty())
        }
        (Method::Get, PROBE_READ) if probe_enabled(&ctx.env) => {
            let user = p.require_read()?;
            Ok(Reply::json(
                200,
                body::object(&[("user", body::Out::Int(user.id.0))]),
            ))
        }
        (Method::Post, PROBE_WRITER) if probe_enabled(&ctx.env) => {
            p.require(plumblog_model::Permission::WritePost)?;
            // Stands in for a content write, so tests can see how writes are attributed.
            ctx.syslog(p.event(ctx, EventKind::ContentChanged).subject("probe"))
                .await;
            Ok(Reply::empty())
        }
        _ => Err(Error::new(
            ErrorCode::RouteNotFound,
            format!("no route {} {path}", method.as_ref()),
        )),
    }
}

/// Session-only routes: account, sign-in and token management.
const AUTH_PREFIX: &str = "/api/auth/";

/// Test builds only: routes for permission tests (owner, any signed-in reader, writer write).
/// Same two locks as `/__contract`: the cargo feature and the variable only
/// tools/wrangler.dev.toml sets.
const PROBE_OWNER: &str = "/api/__probe/owner";
const PROBE_READ: &str = "/api/__probe/read";
const PROBE_WRITER: &str = "/api/__probe/writer";

#[cfg(feature = "contract-tests")]
fn probe_enabled(env: &Env) -> bool {
    env.var("PLUMBLOG_CONTRACT_TESTS")
        .map(|v| v.to_string())
        .ok()
        .as_deref()
        == Some("wipe-db")
}

#[cfg(not(feature = "contract-tests"))]
fn probe_enabled(_env: &Env) -> bool {
    false
}
