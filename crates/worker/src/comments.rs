//! `POST /<slug>/comments`: a reader's comment form (docs/ARCHITECTURE.md, Reader site).
//!
//! Order, cheapest refusal first, and nothing written before the gate has passed: method and
//! form shape (64 KB cap) -> post exists, published, comments open -> name and body limits ->
//! [`CommentGate::verify`] (Turnstile) -> [`CommentGate::allow`] (per-IP limit) -> insert as
//! pending -> one `comment.created` system log entry (ids only) -> 303 to
//! `<post>?comment=pending#comments`. Every refusal is a 303 to
//! `<post>?comment=error&code=<stable code>#comments`: the reader's input is never echoed.

use crate::d1::D1Store;
use crate::pages::{comment_target, routes};
use plumblog_model::{Error, ErrorCode, Result as CoreResult};
use plumblog_store::comment;
use plumblog_syslog::{Scope, request_id};
use worker::{Env, Request, Response, Result, console_error, console_log, js_sys};

/// Largest form body read, in bytes. A full comment of 5,000 non-ASCII characters
/// percent-encodes to about 45 KB (3 UTF-8 bytes, 9 encoded each), plus the name and token;
/// anything bigger is refused before it is read.
pub const MAX_FORM_BYTES: usize = 64 * 1024;
/// The form field Turnstile adds inside the form.
const TOKEN_FIELD: &str = "cf-turnstile-response";

/// What stands between a form post and the database. `verify` proves a person sent it
/// (Turnstile); `allow` counts the attempt against a per-IP limit. Both run before any
/// write, so spam that fails costs no D1 write. Errors carry `comments.unavailable`,
/// `comments.check_failed` or `comments.rate_limited`.
// Workers are single-threaded, so futures need not be `Send` (see auth::Crypto).
#[allow(async_fn_in_trait)]
pub trait CommentGate {
    async fn verify(&self, token: &str, remote_ip: Option<&str>) -> CoreResult<()>;
    async fn allow(&self, remote_ip: Option<&str>) -> CoreResult<()>;
}

/// The production gate until Turnstile and the limiter are wired (issue #17 phase 2):
/// refuses everything, so no comment can be stored on a live site before spam checks exist.
pub struct Unconfigured;

#[expect(
    clippy::unused_async_trait_impl,
    reason = "stands in for the async Turnstile and limiter gate"
)]
impl CommentGate for Unconfigured {
    async fn verify(&self, _token: &str, _remote_ip: Option<&str>) -> CoreResult<()> {
        Err(unavailable())
    }
    async fn allow(&self, _remote_ip: Option<&str>) -> CoreResult<()> {
        Err(unavailable())
    }
}

fn unavailable() -> Error {
    Error::new(
        ErrorCode::CommentsUnavailable,
        "no spam check configured (Turnstile, rate limit)",
    )
}

/// The gate a request gets. Test builds may swap in [`crate::contract::TestGate`].
enum Gate {
    Unconfigured(Unconfigured),
    #[cfg(feature = "contract-tests")]
    Test(crate::contract::TestGate),
}

impl CommentGate for Gate {
    async fn verify(&self, token: &str, remote_ip: Option<&str>) -> CoreResult<()> {
        match self {
            Self::Unconfigured(g) => g.verify(token, remote_ip).await,
            #[cfg(feature = "contract-tests")]
            Self::Test(g) => g.verify(token, remote_ip).await,
        }
    }
    async fn allow(&self, remote_ip: Option<&str>) -> CoreResult<()> {
        match self {
            Self::Unconfigured(g) => g.allow(remote_ip).await,
            #[cfg(feature = "contract-tests")]
            Self::Test(g) => g.allow(remote_ip).await,
        }
    }
}

#[cfg_attr(
    not(feature = "contract-tests"),
    expect(unused_variables, reason = "test builds read them")
)]
fn gate(env: &Env, req: &Request) -> Gate {
    #[cfg(feature = "contract-tests")]
    if let Some(g) = crate::contract::TestGate::for_request(env, req) {
        return Gate::Test(g);
    }
    Gate::Unconfigured(Unconfigured)
}

/// Handles a form post to `/<slug>/comments`.
pub async fn handle(req: &mut Request, env: &Env, slug: &str) -> Result<Response> {
    let scope = Scope::new(
        request_id(req.headers().get("cf-ray")?.as_deref(), random_id),
        None,
    );
    let origin = req.url()?.origin().ascii_serialization();
    let post_url = format!("{origin}{}", routes::post(slug));
    let store = match env.d1("DB") {
        Ok(d) => D1Store::new(d),
        Err(e) => {
            console_error!("{} comments: no D1 binding DB: {e}", scope.log_prefix());
            return redirect(&post_url, Err(unavailable().code));
        }
    };
    let gate = gate(env, req);
    match accept(req, &store, &gate, &scope, slug).await {
        Accept::Stored => redirect(&post_url, Ok(())),
        Accept::NoPost => Ok(Response::empty()?.with_status(404)),
        Accept::Refused(e) => {
            // Worker log only: refusals are not system log events (one write per event,
            // never per spam attempt).
            console_log!(
                "{} comments.refused slug={slug} code={} detail={}",
                scope.log_prefix(),
                e.code.as_str(),
                e.detail
            );
            redirect(&post_url, Err(e.code))
        }
    }
}

enum Accept {
    Stored,
    NoPost,
    Refused(Error),
}

async fn accept(
    req: &mut Request,
    store: &D1Store,
    gate: &impl CommentGate,
    scope: &Scope,
    slug: &str,
) -> Accept {
    let form = match read_form(req).await {
        Ok(f) => f,
        Err(e) => return Accept::Refused(e),
    };
    let post = match comment_target(store, slug).await {
        Ok(Some((id, true))) => id,
        Ok(Some((_, false))) => {
            return Accept::Refused(Error::new(ErrorCode::CommentsClosed, "closed"));
        }
        Ok(None) => return Accept::NoPost,
        Err(e) => {
            console_error!("{} comments: post lookup failed: {e}", scope.log_prefix());
            return Accept::Refused(Error::new(ErrorCode::CommentsUnavailable, e));
        }
    };
    let checked = match comment::check(&form.name, &form.body) {
        Ok(c) => c,
        Err(e) => return Accept::Refused(e),
    };
    let ip = form.remote_ip.as_deref();
    if let Err(e) = gate.verify(&form.token, ip).await {
        return Accept::Refused(e);
    }
    if let Err(e) = gate.allow(ip).await {
        return Accept::Refused(e);
    }
    let now = worker::Date::now().as_millis();
    let report = |m: &str| console_error!("{m}");
    match comment::submit(store, scope, post, checked, now, report).await {
        Ok(_) => Accept::Stored,
        Err(e) => {
            console_error!("{} comments: insert failed: {e}", scope.log_prefix());
            Accept::Refused(Error::new(ErrorCode::CommentsUnavailable, e.to_string()))
        }
    }
}

struct Form {
    name: String,
    body: String,
    token: String,
    remote_ip: Option<String>,
}

async fn read_form(req: &mut Request) -> CoreResult<Form> {
    let bad = |detail: &str| Error::new(ErrorCode::CommentForm, detail);
    let header = |name: &str| req.headers().get(name).ok().flatten();
    let is_form = header("content-type").is_some_and(|t| {
        t.split(';').next().is_some_and(|m| {
            m.trim()
                .eq_ignore_ascii_case("application/x-www-form-urlencoded")
        })
    });
    if !is_form {
        return Err(bad("content type"));
    }
    // Browsers always send a length for a urlencoded form; requiring it means an oversized
    // body is refused before a byte of it is read.
    let len = header("content-length").and_then(|l| l.trim().parse::<usize>().ok());
    if len.is_none_or(|l| l > MAX_FORM_BYTES) {
        return Err(bad("content length missing or over the cap"));
    }
    let remote_ip = header("cf-connecting-ip");
    let bytes = req.bytes().await.map_err(|e| bad(&e.to_string()))?;
    if bytes.len() > MAX_FORM_BYTES {
        return Err(bad("body over the cap"));
    }
    let mut form = Form {
        name: String::new(),
        body: String::new(),
        token: String::new(),
        remote_ip,
    };
    let (mut name, mut body, mut token) = (false, false, false);
    // The first value of each field wins; repeated fields cannot smuggle a second value.
    for (k, v) in worker::url::form_urlencoded::parse(&bytes) {
        match k.as_ref() {
            "name" if !name => (form.name, name) = (v.into_owned(), true),
            "body" if !body => (form.body, body) = (v.into_owned(), true),
            TOKEN_FIELD if !token => (form.token, token) = (v.into_owned(), true),
            _ => {}
        }
    }
    Ok(form)
}

/// 303 to the post: `?comment=pending`, or `?comment=error&code=<code>`. Only fixed strings
/// go into the URL.
fn redirect(post_url: &str, outcome: std::result::Result<(), ErrorCode>) -> Result<Response> {
    let to = match outcome {
        Ok(()) => format!("{post_url}?comment=pending#comments"),
        Err(code) => format!("{post_url}?comment=error&code={}#comments", code.as_str()),
    };
    Response::redirect_with_status(worker::Url::parse(&to)?, 303)
}

/// A request id when there is no `cf-ray` (local runs). It only links log lines to the
/// admin entry, so `Math.random` is enough.
fn random_id() -> String {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a random float in [0, 1) scaled into u64 range"
    )]
    let n = (js_sys::Math::random() * 1e15) as u64;
    format!("req-{n:x}")
}
