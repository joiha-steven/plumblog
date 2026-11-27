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

/// The hashed rate-limit keys of one attempt: the login, and the client address when known.
pub struct Keys(Vec<String>);

async fn hash_key(ctx: &Ctx, text: &str) -> Result<String> {
    Ok(base64url::hex(&ctx.crypto.sha256(text.as_bytes()).await?))
}

impl Keys {
    /// # Errors
    ///
    /// Crypto errors pass through.
    pub async fn new(ctx: &Ctx, login: &str) -> Result<Self> {
        let mut keys = vec![hash_key(ctx, &ratelimit::login_key(login)).await?];
        match ctx.ip.as_deref().map(ratelimit::ip_key) {
            Some(Ok(ip)) => keys.push(hash_key(ctx, &ip).await?),
            // Cloudflare always sets the header; without it, limit by login alone and say so.
            Some(Err(e)) => worker::console_error!(
                "[{}] rate limit by login only: {e}",
                ctx.request_id.as_str()
            ),
            None => worker::console_error!(
                "[{}] rate limit by login only: no CF-Connecting-IP",
                ctx.request_id.as_str()
            ),
        }
        Ok(Self(keys))
    }

    fn refs(&self) -> Vec<&str> {
        self.0.iter().map(String::as_str).collect()
    }

    /// The login key, the first one.
    fn login(&self) -> &str {
        self.0.first().map_or("", String::as_str)
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

/// Asks Turnstile whether `token` is valid. The client IP is not sent (privacy; optional).
async fn turnstile_ok(ctx: &Ctx, secret: &str, token: &str) -> Result<bool> {
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
    let success = worker::js_sys::Reflect::get(&parsed, &JsValue::from_str("success"))
        .map_err(|e| internal(ctx, "json", format!("{e:?}")))?;
    Ok(success.as_bool() == Some(true))
}

/// Gate before checking a password: refuses locked keys, and asks for Turnstile once enough
/// failures piled up. Reads one row per key; writes nothing.
///
/// # Errors
///
/// `Locked`, `TurnstileRequired` (missing or rejected token), storage or fetch errors.
pub async fn check(ctx: &Ctx, keys: &Keys, turnstile_token: Option<&str>) -> Result<Vec<Failures>> {
    let rows: Vec<Failures> = ctx
        .store
        .failures(&keys.refs(), ctx.now, WINDOW_MS)
        .await?
        .into_iter()
        .map(|(count, window_start)| Failures {
            count,
            window_start,
        })
        .collect();
    // Turnstile settings matter only once a key reached its threshold: skip the read before.
    let secret = if rows.iter().any(|r| r.count >= TURNSTILE_AFTER) {
        turnstile_secret(ctx).await?
    } else {
        None
    };
    match ratelimit::decide(&rows, ctx.now, secret.is_some()) {
        Decision::Allow => Ok(rows),
        d @ Decision::Locked { .. } => Err(d
            .error()
            .unwrap_or_else(|| Error::new(ErrorCode::Locked, "locked"))),
        d @ Decision::NeedTurnstile => {
            let refuse = || {
                d.error()
                    .unwrap_or_else(|| Error::new(ErrorCode::TurnstileRequired, "turnstile"))
            };
            let (Some(secret), Some(token)) = (secret, turnstile_token.filter(|t| !t.is_empty()))
            else {
                return Err(refuse());
            };
            if turnstile_ok(ctx, &secret, token).await? {
                Ok(rows)
            } else {
                let mut e = refuse();
                e.detail.push_str("; token rejected");
                Err(e)
            }
        }
    }
}

/// Counts a failed attempt on every key (one upsert each) and logs a lock once, when a key
/// crosses its threshold.
///
/// # Errors
///
/// Storage errors pass through.
pub async fn failed(ctx: &Ctx, keys: &Keys, actor: Option<UserId>) -> Result<()> {
    let mut locked = false;
    let mut turnstile_on: Option<bool> = None;
    for key in &keys.0 {
        let (count, _) = ctx.store.record_failure(key, ctx.now, WINDOW_MS).await?;
        if count == LOCK_AFTER {
            locked = true;
        } else if count == TURNSTILE_AFTER {
            if turnstile_on.is_none() {
                turnstile_on = Some(turnstile_secret(ctx).await?.is_some());
            }
            locked |= turnstile_on == Some(false);
        }
    }
    if locked {
        ctx.syslog(EventKind::SignInLocked, actor, "password").await;
    }
    Ok(())
}

/// After a successful sign-in: forget the login's failures (the address keeps its count).
/// Writes only when there was something to forget.
///
/// # Errors
///
/// Storage errors pass through.
pub async fn succeeded(ctx: &Ctx, keys: &Keys, rows: &[Failures]) -> Result<()> {
    if rows.first().is_some_and(|r| r.count > 0) {
        ctx.store.clear_failures(keys.login()).await?;
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
