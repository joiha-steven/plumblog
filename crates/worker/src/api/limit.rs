//! Sign-in rate limiting (docs/ARCHITECTURE.md, Users and auth): the policy is
//! `plumblog_auth::ratelimit`; this file reads and writes `login_failures` and asks Turnstile.

use std::fmt::Write as _;

use plumblog_auth::Crypto;
use plumblog_auth::base64url;
use plumblog_auth::ratelimit::{self, Decision, Failures, LOCK_AFTER, TURNSTILE_AFTER, WINDOW_MS};
use plumblog_model::{Error, ErrorCode, Result, UserId};
use plumblog_store::{LoginLimitStore, SettingsStore, TURNSTILE_SITE_KEY};
use plumblog_syslog::EventKind;
use worker::wasm_bindgen::JsValue;
use worker::{Fetch, Headers, Method, RequestInit};

use super::Ctx;
use super::body::json_string;

/// Cloudflare's endpoint; tests point `TURNSTILE_VERIFY_URL` at a local stub.
const DEFAULT_VERIFY_URL: &str = "https://challenges.cloudflare.com/turnstile/v0/siteverify";

/// Most ended `login_failures` rows one sweep deletes: bounds the writes of any one request.
const SWEEP_LIMIT: u32 = 50;

/// The hashed rate-limit keys of one attempt: the client address when known, and the subject
/// (a login, or a passkey credential id) when there is one.
pub struct Keys {
    ip: Option<String>,
    subject: Option<String>,
}

async fn hash_key(ctx: &Ctx, text: &str) -> Result<String> {
    Ok(base64url::hex(&ctx.crypto.sha256(text.as_bytes()).await?))
}

impl Keys {
    /// Password sign-in and re-checks: the login and the address.
    ///
    /// # Errors
    ///
    /// Crypto errors pass through.
    pub async fn new(ctx: &Ctx, login: &str) -> Result<Self> {
        Self::build(ctx, Some(ratelimit::login_key(login))).await
    }

    /// Passkey sign-in: the credential id the browser named, and the address.
    ///
    /// # Errors
    ///
    /// Crypto errors pass through.
    pub async fn passkey(ctx: &Ctx, credential_id: &str) -> Result<Self> {
        Self::build(ctx, Some(format!("passkey:{credential_id}"))).await
    }

    /// One-time codes (setup, invites): the address only; a code has no stable subject.
    ///
    /// # Errors
    ///
    /// Crypto errors pass through.
    pub async fn address(ctx: &Ctx) -> Result<Self> {
        Self::build(ctx, None).await
    }

    async fn build(ctx: &Ctx, subject: Option<String>) -> Result<Self> {
        let subject = match subject {
            Some(s) => Some(hash_key(ctx, &s).await?),
            None => None,
        };
        let mut ip = None;
        match ctx.ip.as_deref().map(ratelimit::ip_key) {
            Some(Ok(k)) => ip = Some(hash_key(ctx, &k).await?),
            // Cloudflare always sets the header; without it, limit by the subject alone (or
            // not at all, for codes) and say so.
            Some(Err(e)) => worker::console_error!(
                "[{}] rate limit without the address: {e}",
                ctx.request_id.as_str()
            ),
            None => worker::console_error!(
                "[{}] rate limit without the address: no CF-Connecting-IP",
                ctx.request_id.as_str()
            ),
        }
        Ok(Self { ip, subject })
    }
}

/// The Turnstile secret when Turnstile is set up: a site key in settings and the
/// `TURNSTILE_SECRET` secret. Half a setup is logged and treated as off (so 5 failures lock).
async fn turnstile_secret(ctx: &Ctx) -> Result<Option<String>> {
    let site_key = ctx
        .store
        .get_setting(TURNSTILE_SITE_KEY)
        .await?
        .and_then(|v| json_string(&v))
        .filter(|k| !k.is_empty());
    let secret = ctx
        .env
        .secret("TURNSTILE_SECRET")
        .map(|s| s.to_string())
        .ok()
        .filter(|s| !s.is_empty());
    match (site_key, secret) {
        (Some(_), Some(s)) => Ok(Some(s)),
        (Some(_), None) => {
            worker::console_error!(
                "[{}] turnstile_site_key is set but the TURNSTILE_SECRET secret is missing; \
                 treating Turnstile as off",
                ctx.request_id.as_str()
            );
            Ok(None)
        }
        (None, _) => Ok(None),
    }
}

/// Public site key for the sign-in page, only when the server can verify its tokens.
///
/// # Errors
///
/// Storage errors pass through.
pub async fn turnstile_site_key(ctx: &Ctx) -> Result<Option<String>> {
    if turnstile_secret(ctx).await?.is_none() {
        return Ok(None);
    }
    Ok(ctx
        .store
        .get_setting(TURNSTILE_SITE_KEY)
        .await?
        .and_then(|v| json_string(&v)))
}

/// `application/x-www-form-urlencoded` value.
fn form_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

fn internal(_ctx: &Ctx, what: &str, e: impl std::fmt::Display) -> Error {
    Error::new(ErrorCode::Internal, format!("turnstile {what}: {e}"))
}

/// Asks Turnstile whether `token` is valid and was solved on this site: `success` and a
/// `hostname` equal to the site host (a token solved on another site that uses the same
/// widget keys must not count here). The client IP is not sent (privacy; optional).
async fn turnstile_ok(ctx: &Ctx, secret: &str, token: &str) -> Result<bool> {
    // Before the network call: without a site origin no answer could be checked.
    let site = super::site::site(ctx).await?;
    let url = ctx
        .env
        .var("TURNSTILE_VERIFY_URL")
        .map_or_else(|_| DEFAULT_VERIFY_URL.to_owned(), |v| v.to_string());
    let body = format!(
        "secret={}&response={}",
        form_value(secret),
        form_value(token)
    );
    let headers = Headers::new();
    headers
        .set("content-type", "application/x-www-form-urlencoded")
        .map_err(|e| internal(ctx, "headers", e))?;
    let mut init = RequestInit::new();
    init.with_method(Method::Post)
        .with_headers(headers)
        .with_body(Some(JsValue::from_str(&body)));
    let req =
        worker::Request::new_with_init(&url, &init).map_err(|e| internal(ctx, "request", e))?;
    let mut res = Fetch::Request(req)
        .send()
        .await
        .map_err(|e| internal(ctx, "fetch", e))?;
    if res.status_code() != 200 {
        return Err(internal(ctx, "status", res.status_code()));
    }
    let text = res.text().await.map_err(|e| internal(ctx, "body", e))?;
    // The answer holds arrays, which the strict flat reader refuses; it comes from a fixed
    // trusted URL, so the runtime's JSON parser reads it.
    let parsed =
        worker::js_sys::JSON::parse(&text).map_err(|e| internal(ctx, "json", format!("{e:?}")))?;
    let field = |name: &str| {
        worker::js_sys::Reflect::get(&parsed, &JsValue::from_str(name))
            .map_err(|e| internal(ctx, "json", format!("{e:?}")))
    };
    if field("success")?.as_bool() != Some(true) {
        return Ok(false);
    }
    match field("hostname")?.as_string() {
        Some(h) if h.eq_ignore_ascii_case(&site.host) => Ok(true),
        other => {
            worker::console_warn!(
                "[{}] turnstile token solved on {other:?}, not {:?}",
                ctx.request_id.as_str(),
                site.host
            );
            Ok(false)
        }
    }
}

/// One counted attempt.
pub struct Attempt {
    /// This attempt's failure brought a key to its lock (logged once, by [`failed`]).
    locks: bool,
}

/// Sweeps a bounded batch of ended rows. A failure only leaves rows for the next sweep.
async fn sweep(ctx: &Ctx) {
    if let Err(e) = ctx
        .store
        .delete_ended_failures(ctx.now, WINDOW_MS, SWEEP_LIMIT)
        .await
    {
        worker::console_error!(
            "[{}] sweeping ended login_failures failed: {e}",
            ctx.request_id.as_str()
        );
    }
}

/// Gate before checking a secret (password, passkey, one-time code). Counts the attempt first,
/// in one atomic upsert per key, so concurrent attempts cannot all read a count under the
/// threshold: each gets its own count. The address goes first: once it is locked nothing more
/// is written for the attempt (no subject key, no challenge), so a flood of random logins
/// from one address costs no writes. A key that reached its lock is not written either. Then
/// decides from the attempts before this one: refuses locked keys, and asks for Turnstile once
/// enough piled up. A refused attempt below the lock stays counted; that never extends a
/// lock, which ends with its window (the upsert moves `window_start` only when it is over).
/// The first attempt of an address's window also sweeps a bounded batch of ended rows.
///
/// # Errors
///
/// `Locked`, `TurnstileRequired` (missing or rejected token), `OriginMissing` (Turnstile on
/// without a site origin), storage or fetch errors.
pub async fn check(ctx: &Ctx, keys: &Keys, turnstile_token: Option<&str>) -> Result<Attempt> {
    // The lock threshold depends on Turnstile, and the store needs it to stop writing.
    let secret = turnstile_secret(ctx).await?;
    let cap = if secret.is_some() {
        LOCK_AFTER
    } else {
        TURNSTILE_AFTER
    };
    let mut before = Vec::with_capacity(2);
    let mut locks = false;
    let mut new_window = false;
    for (i, key) in [&keys.ip, &keys.subject].into_iter().flatten().enumerate() {
        let r = ctx
            .store
            .record_failure(key, ctx.now, WINDOW_MS, cap)
            .await?;
        locks |= r.counted && r.count == cap;
        new_window |= i == 0 && r.counted && r.count == 1;
        before.push(Failures {
            count: r.before(),
            window_start: r.window_start,
        });
        if i == 0
            && keys.ip.is_some()
            && let d @ Decision::Locked { .. } =
                ratelimit::decide(&before, ctx.now, secret.is_some())
        {
            return Err(locked(d));
        }
    }
    if new_window {
        sweep(ctx).await;
    }
    match ratelimit::decide(&before, ctx.now, secret.is_some()) {
        Decision::Allow => Ok(Attempt { locks }),
        d @ Decision::Locked { .. } => Err(locked(d)),
        d @ Decision::NeedTurnstile => {
            let refuse = || {
                d.error()
                    .unwrap_or_else(|| Error::new(ErrorCode::TurnstileRequired, "turnstile"))
            };
            if secret.is_some() {
                // Turnstile is on but cannot be verified: say so, not "solve the challenge".
                super::site::site(ctx).await?;
            }
            let (Some(secret), Some(token)) = (secret, turnstile_token.filter(|t| !t.is_empty()))
            else {
                return Err(refuse());
            };
            if turnstile_ok(ctx, &secret, token).await? {
                Ok(Attempt { locks })
            } else {
                let mut e = refuse();
                e.detail.push_str("; token rejected");
                Err(e)
            }
        }
    }
}

fn locked(d: Decision) -> Error {
    d.error()
        .unwrap_or_else(|| Error::new(ErrorCode::Locked, "locked"))
}

/// After a failed check of the secret: the attempt is already counted; logs a lock once, from
/// the attempt whose failure made a key reach its threshold.
pub async fn failed(ctx: &Ctx, attempt: &Attempt, actor: Option<UserId>, how: &str) {
    if attempt.locks {
        ctx.syslog(ctx.event(EventKind::SignInLocked, actor).subject(how))
            .await;
    }
}

/// After a success: forget the subject's count (login or credential), and take this attempt
/// back from the address's (its earlier failures stay), one write per key. Without this, a
/// person signing in a few times from one address would lock themselves out.
///
/// # Errors
///
/// Storage errors pass through.
pub async fn succeeded(ctx: &Ctx, keys: &Keys) -> Result<()> {
    if let Some(key) = &keys.subject {
        ctx.store.clear_failures(key).await?;
    }
    if let Some(key) = &keys.ip {
        ctx.store.forgive_failure(key).await?;
    }
    Ok(())
}

/// Most anonymous passkey sign-in options per address in a window: each writes a challenge
/// row, so the count bounds the table; far above what a person needs.
pub const OPTIONS_PER_WINDOW: i64 = 60;

/// Counts one anonymous options request for this address, before anything is written for it.
///
/// # Errors
///
/// `Locked` above [`OPTIONS_PER_WINDOW`] in the window; storage and crypto errors pass through.
pub async fn options(ctx: &Ctx) -> Result<()> {
    let Some(ip) = ctx.ip.as_deref() else {
        worker::console_error!(
            "[{}] passkey options not limited: no CF-Connecting-IP",
            ctx.request_id.as_str()
        );
        return Ok(());
    };
    let ip = match ratelimit::ip_key(ip) {
        Ok(k) => k,
        Err(e) => {
            worker::console_error!(
                "[{}] passkey options not limited: {e}",
                ctx.request_id.as_str()
            );
            return Ok(());
        }
    };
    let key = hash_key(ctx, &format!("options:{ip}")).await?;
    let r = ctx
        .store
        .record_failure(&key, ctx.now, WINDOW_MS, OPTIONS_PER_WINDOW)
        .await?;
    if !r.counted {
        return Err(Error::new(
            ErrorCode::Locked,
            format!(
                "passkey options: {} from this address since {}, limit {OPTIONS_PER_WINDOW}",
                r.count, r.window_start
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::form_value;

    #[test]
    fn form_encoding() {
        assert_eq!(form_value("a b&c=d+e/~"), "a%20b%26c%3Dd%2Be%2F~");
    }
}
