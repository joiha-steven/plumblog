//! The JSON API under `/api/`. Every request goes through the same gate, in order:
//! CSRF (`Origin`), principal (who is asking), then the route, which checks its permission
//! with [`Principal::require`]. Errors answer `{"error":"<stable code>"}` with a status and
//! log the detail with the request id; no user-facing text is produced here.

mod auth;
mod body;
mod fields;
mod limit;
mod passkey;
mod principal;
mod site;

use plumblog_auth::Crypto;
use plumblog_auth::base64url;
use plumblog_model::UserId;
use plumblog_model::{Error, ErrorCode, RequestId, Result};
use plumblog_store::LogStore;
use plumblog_syslog::{Event, EventKind};
use worker::{Env, Method, Request, Response};

use crate::d1::D1Store;
use crate::webcrypto::WebCrypto;
pub use principal::Principal;

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
    /// Appends one system log entry. A failing log never fails the request; it goes to the
    /// Worker log instead.
    pub async fn syslog(&self, kind: EventKind, actor: Option<UserId>, detail: &str) {
        let event = Event {
            at_ms: u64::try_from(self.now).unwrap_or(0),
            request_id: self.request_id.clone(),
            kind,
            actor,
            error: None,
            detail: detail.to_owned(),
        };
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
}

impl Reply {
    #[must_use]
    pub fn json(status: u16, body: String) -> Self {
        Self {
            status,
            body: Some(body),
            cookie: None,
        }
    }

    #[must_use]
    pub fn empty() -> Self {
        Self {
            status: 204,
            body: None,
            cookie: None,
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
        ErrorCode::InvalidInput | ErrorCode::PasskeyInvalid => 400,
        ErrorCode::Unauthenticated | ErrorCode::InvalidCredentials => 401,
        ErrorCode::Forbidden | ErrorCode::SetupInvalid | ErrorCode::TurnstileRequired => 403,
        ErrorCode::PostNotFound
        | ErrorCode::UserNotFound
        | ErrorCode::NotFound
        | ErrorCode::RouteNotFound => 404,
        ErrorCode::Conflict => 409,
        ErrorCode::InviteInvalid => 410,
        ErrorCode::Locked => 429,
        ErrorCode::Storage | ErrorCode::Internal => 500,
        // The site is not set up for this yet (no configured origin): not the client's fault.
        ErrorCode::OriginMissing => 503,
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
    let mut res = match reply.body {
        Some(b) => Response::ok(b)?.with_status(reply.status),
        None => Response::empty()?.with_status(reply.status),
    };
    let h = res.headers_mut();
    if reply.status != 204 {
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
    Reply::json(
        status(e.code),
        body::object(&[("error", body::Out::Str(e.code.as_str().to_owned()))]),
    )
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

/// Handles one `/api/` request. Never returns a non-JSON error for a failure of ours.
pub async fn handle(mut req: Request, env: &Env) -> worker::Result<Response> {
    let crypto = WebCrypto::new();
    let rid = request_id(&req, crypto.as_ref().ok());
    let method = req.method();
    let path = req.path();
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
        if !matches!(method, Method::Get | Method::Head) {
            let want = site::configured(&ctx)
                .await?
                .map_or_else(|| ctx.request_origin.clone(), |s| s.origin);
            check_origin(&req, &method, &want)?;
        }
        let body = if matches!(method, Method::Get | Method::Head) {
            Vec::new()
        } else {
            req.bytes()
                .await
                .map_err(|e| Error::new(ErrorCode::InvalidInput, format!("body: {e}")))?
        };
        let (principal, cookie) = principal::resolve(&ctx, &req).await?;
        gate_cookie = cookie;
        route(&ctx, &principal, &method, &path, &body).await
    }
    .await;
    let mut reply = match result {
        Ok(r) => r,
        Err(e) => {
            let s = status(e.code);
            let line = format!("[{}] {} {path} -> {s} {}", rid.as_str(), method.as_ref(), e);
            if s >= 500 {
                worker::console_error!("{line}");
            } else {
                worker::console_warn!("{line}");
            }
            error_reply(&e)
        }
    };
    // A handler that changed the session (sign-in, sign-out, rotation) decides the cookie.
    if reply.cookie.is_none() {
        reply.cookie = gate_cookie;
    }
    respond(reply)
}

async fn route(
    ctx: &Ctx,
    p: &Principal,
    method: &Method,
    path: &str,
    body: &[u8],
) -> Result<Reply> {
    match (method, path) {
        (Method::Get, "/api/auth/params") => auth::params(ctx).await,
        (Method::Get, "/api/auth/me") => auth::me(p),
        (Method::Patch, "/api/auth/me") => auth::update_me(ctx, p, body).await,
        (Method::Post, "/api/auth/setup") => auth::setup(ctx, p, body).await,
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
            passkey::delete(ctx, p, &path[passkey::ITEM_PREFIX.len()..]).await
        }
        (Method::Get, PROBE) if probe_enabled(&ctx.env) => {
            p.require(plumblog_model::Permission::ManageSettings)?;
            Ok(Reply::empty())
        }
        _ => Err(Error::new(
            ErrorCode::RouteNotFound,
            format!("no route {} {path}", method.as_ref()),
        )),
    }
}

/// Test builds only: an owner-only route for permission tests. Same two locks as
/// `/__contract`: the cargo feature and the variable only tools/wrangler.dev.toml sets.
const PROBE: &str = "/api/__probe/owner";

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
