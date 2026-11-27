//! Sign-in rate limiting (docs/ARCHITECTURE.md, Users and auth): the policy is
//! `plumblog_auth::ratelimit`; this file reads and writes `login_failures` and asks Turnstile.

use std::fmt::Write as _;

use plumblog_auth::Crypto;
use plumblog_auth::base64url;
use plumblog_auth::ratelimit::{self, Decision, Failures, LOCK_AFTER, TURNSTILE_AFTER, WINDOW_MS};
use plumblog_model::{Error, ErrorCode, Result, UserId};
use plumblog_store::{
    ChallengePurpose, ChallengeStore, LoginLimitStore, SettingsStore, TURNSTILE_SITE_KEY,
};
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
pub async fn turnstile_secret(ctx: &Ctx) -> Result<Option<String>> {
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
/// widget keys must not count here). `remote_ip` is optional: sign-in leaves it out
/// (privacy); comments send it, since Turnstile uses it to judge the solve.
pub async fn turnstile_ok(
    ctx: &Ctx,
    secret: &str,
    token: &str,
    remote_ip: Option<&str>,
) -> Result<bool> {
    // Before the network call: without a site origin no answer could be checked.
    let site = super::site::site(ctx).await?;
    let url = ctx
        .env
        .var("TURNSTILE_VERIFY_URL")
        .map_or_else(|_| DEFAULT_VERIFY_URL.to_owned(), |v| v.to_string());
    let mut body = format!(
        "secret={}&response={}",
        form_value(secret),
        form_value(token)
    );
    if let Some(ip) = remote_ip {
        body.push_str("&remoteip=");
        body.push_str(&form_value(ip));
    }
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

/// Turnstile, looked up once per request and only when an attempt needs it (a key at its
/// Turnstile threshold, or the site-wide cap reached).
#[derive(Default)]
struct Gate {
    setup: Setup,
    /// A token was verified for this request: tokens are single use, so it is not sent twice.
    passed: bool,
    /// The site-wide cap was passed by consuming a gated passkey challenge (see [`global`]).
    took_gated: bool,
}

#[derive(Clone, Default)]
enum Setup {
    #[default]
    Unread,
    Off,
    On(String),
}

impl Gate {
    async fn secret(&mut self, ctx: &Ctx) -> Result<Option<String>> {
        if matches!(self.setup, Setup::Unread) {
            self.setup = match turnstile_secret(ctx).await? {
                Some(s) => Setup::On(s),
                None => Setup::Off,
            };
        }
        Ok(match &self.setup {
            Setup::On(s) => Some(s.clone()),
            Setup::Unread | Setup::Off => None,
        })
    }

    /// Whether Turnstile is on, as far as this request looked; `false` when it never needed to.
    fn on(&self) -> bool {
        matches!(self.setup, Setup::On(_))
    }
}

/// The site-wide cap. Test builds read `PLUMBLOG_TEST_GLOBAL_CAP`, behind the same two locks
/// as `/__contract` (the cargo feature and the variable only tools/wrangler.dev.toml sets).
#[cfg(feature = "contract-tests")]
fn global_cap(ctx: &Ctx) -> i64 {
    let var = |k: &str| ctx.env.var(k).map(|v| v.to_string()).ok();
    if var("PLUMBLOG_CONTRACT_TESTS").as_deref() != Some("wipe-db") {
        return ratelimit::GLOBAL_CAP;
    }
    var("PLUMBLOG_TEST_GLOBAL_CAP")
        .and_then(|v| v.parse().ok())
        .unwrap_or(ratelimit::GLOBAL_CAP)
}

#[cfg(not(feature = "contract-tests"))]
fn global_cap(_ctx: &Ctx) -> i64 {
    ratelimit::GLOBAL_CAP
}

/// Counts the attempt on the site-wide key before anything else is written. Below the cap it
/// costs one write, and the attempt that reaches the cap logs it once per window. At the cap
/// nothing is written: with Turnstile fully set up (site key, secret and origin) a valid token
/// lets the attempt go on to the per-address and per-key limits; without one it is
/// `TurnstileRequired`, and without Turnstile `Locked` until the window ends. Returns whether
/// this attempt opened a new window.
///
/// `gated`: the hash a passkey sign-in challenge has when its options request passed this
/// gate at the cap (issue #63: tokens are single use, so options and sign-in cannot each
/// verify one). At the cap with Turnstile set up, consuming that challenge, single use and
/// atomic, passes the gate instead of a token; it is tried first, as a client may send the
/// spent token again. Without Turnstile it is never taken.
async fn global(
    ctx: &Ctx,
    gate: &mut Gate,
    token: Option<&str>,
    gated: Option<&str>,
) -> Result<bool> {
    let cap = global_cap(ctx);
    if let Some(n) = ctx
        .store
        .count_capped(ratelimit::GLOBAL_KEY, ctx.now, WINDOW_MS, cap)
        .await?
    {
        if n == cap {
            ctx.syslog(
                ctx.event(EventKind::GlobalCap, None)
                    .subject(&cap.to_string()),
            )
            .await;
        }
        return Ok(n == 1);
    }
    let configured =
        gate.secret(ctx).await?.is_some() && super::site::configured(ctx).await?.is_some();
    if !configured {
        return Err(Error::new(
            ErrorCode::Locked,
            format!("site-wide sign-in cap {cap} reached in this window; Turnstile is off"),
        ));
    }
    let refuse = |why: &str| {
        Error::new(
            ErrorCode::TurnstileRequired,
            format!("site-wide sign-in cap {cap} reached in this window; {why}"),
        )
    };
    if let Some(hash) = gated
        && ctx
            .store
            .take_challenge(hash, ChallengePurpose::Login, ctx.now)
            .await?
            .is_some()
    {
        gate.passed = true;
        gate.took_gated = true;
        return Ok(false);
    }
    let (Some(secret), Some(token)) = (gate.secret(ctx).await?, token.filter(|t| !t.is_empty()))
    else {
        return Err(refuse("no token"));
    };
    if !turnstile_ok(ctx, &secret, token, None).await? {
        return Err(refuse("token rejected"));
    }
    gate.passed = true;
    Ok(false)
}

/// Refuses an address already locked in this window, from one read and no write. Runs before
/// the site-wide count, so a locked address never adds to it: one address adds at most its own
/// threshold per window, and cannot keep sign-in locked site-wide on its own.
async fn refuse_locked_address(ctx: &Ctx, gate: &mut Gate, ip: &str) -> Result<()> {
    let (count, window_start) = ctx
        .store
        .failures(&[ip], ctx.now, WINDOW_MS)
        .await?
        .first()
        .copied()
        .unwrap_or((0, 0));
    // Between the thresholds the lock depends on Turnstile; read its setting only then.
    if (TURNSTILE_AFTER..LOCK_AFTER).contains(&count) {
        gate.secret(ctx).await?;
    }
    let row = Failures {
        count,
        window_start,
    };
    match ratelimit::decide(&[row], ctx.now, gate.on()) {
        d @ Decision::Locked { .. } => Err(locked(d)),
        Decision::Allow | Decision::NeedTurnstile => Ok(()),
    }
}

/// Gate before checking a secret (password, passkey, one-time code) for a caller without a
/// session. An address already locked is refused first, with no write. Then the site-wide
/// count ([`global`]): at its cap the attempt writes nothing unless it carries a valid Turnstile
/// token. Then counts the attempt per key, in one atomic upsert per key, so concurrent attempts
/// cannot all read a count under the threshold: each gets its own count. The address goes
/// first: once it is locked nothing more is written for the attempt (no subject key, no
/// challenge), so a flood of random logins from one address costs no writes. A key that
/// reached its lock is not written either. Then decides from the attempts before this one:
/// refuses locked keys, and asks for Turnstile once enough piled up. A refused attempt below
/// the lock stays counted; that never extends a lock, which ends with its window (the upsert
/// moves `window_start` only when it is over). The first attempt of an address's window, and
/// of the site-wide window (the only sweep for attempts without an address), also sweeps a
/// bounded batch of ended rows.
///
/// # Errors
///
/// `Locked`, `TurnstileRequired` (missing or rejected token), `OriginMissing` (Turnstile on
/// without a site origin), storage or fetch errors.
pub async fn check(ctx: &Ctx, keys: &Keys, turnstile_token: Option<&str>) -> Result<Attempt> {
    check_passkey(ctx, keys, turnstile_token, None)
        .await
        .map(|(attempt, _)| attempt)
}

/// As [`check`], for anonymous passkey sign-in: `gated` is the gated hash of the presented
/// challenge (see [`global`]). Also returns whether that challenge was consumed here, in which
/// case the caller must not take it again.
///
/// # Errors
///
/// As [`check`].
pub async fn check_passkey(
    ctx: &Ctx,
    keys: &Keys,
    turnstile_token: Option<&str>,
    gated: Option<&str>,
) -> Result<(Attempt, bool)> {
    let mut gate = Gate::default();
    if let Some(ip) = &keys.ip {
        refuse_locked_address(ctx, &mut gate, ip).await?;
    }
    let new_window = global(ctx, &mut gate, turnstile_token, gated).await?;
    let took_gated = gate.took_gated;
    let attempt = count_keys(ctx, keys, turnstile_token, gate, new_window).await?;
    Ok((attempt, took_gated))
}

/// As [`check`], for re-authentication by a caller who already holds a session (current
/// password, passkey re-auth): only the per-address and per-key limits, never the site-wide
/// cap, so a flood of anonymous attempts cannot stop a signed-in user from changing a password
/// or a passkey. A stolen session stays limited per login and per address.
///
/// # Errors
///
/// As [`check`].
pub async fn check_signed_in(
    ctx: &Ctx,
    keys: &Keys,
    turnstile_token: Option<&str>,
) -> Result<Attempt> {
    count_keys(ctx, keys, turnstile_token, Gate::default(), false).await
}

async fn count_keys(
    ctx: &Ctx,
    keys: &Keys,
    turnstile_token: Option<&str>,
    mut gate: Gate,
    mut new_window: bool,
) -> Result<Attempt> {
    let mut before = Vec::with_capacity(2);
    let mut locks = false;
    for (i, key) in [&keys.ip, &keys.subject].into_iter().flatten().enumerate() {
        let is_ip = i == 0 && keys.ip.is_some();
        // Counted with the lower threshold as the cap first: below it the lock does not depend
        // on Turnstile, so most attempts never read its settings.
        let mut cap = TURNSTILE_AFTER;
        let mut r = ctx
            .store
            .record_failure(key, ctx.now, WINDOW_MS, cap)
            .await?;
        if (!r.counted || r.count == cap) && gate.secret(ctx).await?.is_some() {
            cap = LOCK_AFTER;
            if !r.counted {
                r = ctx
                    .store
                    .record_failure(key, ctx.now, WINDOW_MS, cap)
                    .await?;
            }
        }
        locks |= r.counted && r.count == cap;
        new_window |= is_ip && r.counted && r.count == 1;
        before.push(Failures {
            count: r.before(),
            window_start: r.window_start,
        });
        if is_ip && let d @ Decision::Locked { .. } = ratelimit::decide(&before, ctx.now, gate.on())
        {
            return Err(locked(d));
        }
    }
    if new_window {
        sweep(ctx).await;
    }
    match ratelimit::decide(&before, ctx.now, gate.on()) {
        Decision::Allow => Ok(Attempt { locks }),
        d @ Decision::Locked { .. } => Err(locked(d)),
        // Already verified at the site-wide cap: the same token cannot be verified twice, and
        // a gated challenge stands for the token its options request verified.
        Decision::NeedTurnstile if gate.passed => Ok(Attempt { locks }),
        d @ Decision::NeedTurnstile => {
            let refuse = || {
                d.error()
                    .unwrap_or_else(|| Error::new(ErrorCode::TurnstileRequired, "turnstile"))
            };
            let secret = gate.secret(ctx).await?;
            if secret.is_some() {
                // Turnstile is on but cannot be verified: say so, not "solve the challenge".
                super::site::site(ctx).await?;
            }
            let (Some(secret), Some(token)) = (secret, turnstile_token.filter(|t| !t.is_empty()))
            else {
                return Err(refuse());
            };
            if turnstile_ok(ctx, &secret, token, None).await? {
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

/// The hashed limiter key of this address under `scope`, or `None` (logged as `what`) when
/// the address is missing or unusable: limiting everyone together would let one client lock
/// the rest out.
async fn address_key(ctx: &Ctx, scope: &str, what: &str) -> Result<Option<String>> {
    let Some(ip) = ctx.ip.as_deref() else {
        worker::console_error!(
            "[{}] {what} not limited: no CF-Connecting-IP",
            ctx.request_id.as_str()
        );
        return Ok(None);
    };
    let ip = match ratelimit::ip_key(ip) {
        Ok(k) => k,
        Err(e) => {
            worker::console_error!("[{}] {what} not limited: {e}", ctx.request_id.as_str());
            return Ok(None);
        }
    };
    Ok(Some(hash_key(ctx, &format!("{scope}:{ip}")).await?))
}

/// Most anonymous passkey sign-in options per address in a window: each writes a challenge
/// row, so the count bounds the table; far above what a person needs (one per sign-in). Kept
/// below `GLOBAL_CAP` so one address alone cannot fill the site-wide window.
pub const OPTIONS_PER_WINDOW: i64 = 20;

/// Counts one anonymous options request. An address at [`OPTIONS_PER_WINDOW`] is refused from
/// one read, before the site-wide count (as in [`check`], so it never adds to it). Then the
/// site-wide count (at the cap it needs a valid Turnstile token or is refused, writing nothing),
/// then this address's, before anything is written for it. Returns whether a Turnstile token
/// passed the site-wide cap: the challenge is then stored gated (see [`global`]).
///
/// # Errors
///
/// `Locked` above [`OPTIONS_PER_WINDOW`] in the window or at the site-wide cap without
/// Turnstile; `TurnstileRequired`; storage, fetch and crypto errors pass through.
pub async fn options(ctx: &Ctx, turnstile_token: Option<&str>) -> Result<bool> {
    let key = options_key(ctx).await?;
    if let Some(key) = &key {
        let (count, window_start) = ctx
            .store
            .failures(&[key], ctx.now, WINDOW_MS)
            .await?
            .first()
            .copied()
            .unwrap_or((0, 0));
        if count >= OPTIONS_PER_WINDOW {
            return Err(options_locked(count, window_start));
        }
    }
    let mut gate = Gate::default();
    let new_window = global(ctx, &mut gate, turnstile_token, None).await?;
    if new_window {
        sweep(ctx).await;
    }
    let Some(key) = key else {
        return Ok(gate.passed);
    };
    let r = ctx
        .store
        .record_failure(&key, ctx.now, WINDOW_MS, OPTIONS_PER_WINDOW)
        .await?;
    if !r.counted {
        return Err(options_locked(r.count, r.window_start));
    }
    Ok(gate.passed)
}

/// Comments an address may send per [`COMMENTS_WINDOW_MS`]. A person writes a comment or
/// two and retries a typo once or twice; 5 covers that while a bot stays well under a flood.
/// Only attempts with a valid Turnstile token count (see [`comment_precheck`]), so each
/// limiter row costs an attacker one solved challenge.
pub const COMMENTS_PER_WINDOW: i64 = 5;
/// The comment window: 10 minutes, long enough to blunt a script, short enough that an
/// honest reader who hit the cap soon writes again.
pub const COMMENTS_WINDOW_MS: i64 = 10 * 60 * 1000;

fn comments_limited(count: i64, since: i64) -> Error {
    Error::new(
        ErrorCode::CommentRateLimited,
        format!("{count} from this address since {since}, limit {COMMENTS_PER_WINDOW}"),
    )
}

/// Before Turnstile is asked: refuses an address already at [`COMMENTS_PER_WINDOW`] from one
/// read, so it costs no write and no siteverify call.
///
/// # Errors
///
/// `CommentRateLimited` at the cap; storage and crypto errors pass through.
pub async fn comment_precheck(ctx: &Ctx) -> Result<()> {
    let Some(key) = address_key(ctx, "comments", "comment attempt").await? else {
        return Ok(());
    };
    let (count, since) = ctx
        .store
        .failures(&[&key], ctx.now, COMMENTS_WINDOW_MS)
        .await?
        .first()
        .copied()
        .unwrap_or((0, 0));
    if count >= COMMENTS_PER_WINDOW {
        return Err(comments_limited(count, since));
    }
    Ok(())
}

/// After Turnstile passed: counts the attempt in one atomic upsert, so concurrent attempts
/// cannot all slip under the cap that [`comment_precheck`] read.
///
/// # Errors
///
/// `CommentRateLimited` when the count would pass [`COMMENTS_PER_WINDOW`]; storage and crypto
/// errors pass through.
pub async fn comment_record(ctx: &Ctx) -> Result<()> {
    let Some(key) = address_key(ctx, "comments", "comment attempt").await? else {
        return Ok(());
    };
    let r = ctx
        .store
        .record_failure(&key, ctx.now, COMMENTS_WINDOW_MS, COMMENTS_PER_WINDOW)
        .await?;
    if r.counted {
        Ok(())
    } else {
        Err(comments_limited(r.count, r.window_start))
    }
}

fn options_locked(count: i64, window_start: i64) -> Error {
    Error::new(
        ErrorCode::Locked,
        format!(
            "passkey options: {count} from this address since {window_start}, limit {OPTIONS_PER_WINDOW}"
        ),
    )
}

/// The per-address options key, or `None` (logged) when the address is missing or unusable.
async fn options_key(ctx: &Ctx) -> Result<Option<String>> {
    let Some(ip) = ctx.ip.as_deref() else {
        worker::console_error!(
            "[{}] passkey options not limited per address: no CF-Connecting-IP",
            ctx.request_id.as_str()
        );
        return Ok(None);
    };
    let ip = match ratelimit::ip_key(ip) {
        Ok(k) => k,
        Err(e) => {
            worker::console_error!(
                "[{}] passkey options not limited per address: {e}",
                ctx.request_id.as_str()
            );
            return Ok(None);
        }
    };
    Ok(Some(hash_key(ctx, &format!("options:{ip}")).await?))
}

#[cfg(test)]
mod tests {
    use super::form_value;

    #[test]
    fn form_encoding() {
        assert_eq!(form_value("a b&c=d+e/~"), "a%20b%26c%3Dd%2Be%2F~");
    }
}
