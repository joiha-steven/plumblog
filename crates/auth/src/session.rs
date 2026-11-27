//! Session tokens and the session cookie. Only the SHA-256 of a token is stored, so a leaked
//! database cannot be replayed as cookies. Times are UTC milliseconds like the store.

use crate::{Crypto, base64url};
use plumblog_model::Result;
use std::fmt;

/// `__Host-` makes browsers refuse the cookie unless it is Secure, has `Path=/` and no
/// `Domain`, so no subdomain or plain-HTTP page can set or overwrite it.
pub const COOKIE_NAME: &str = "__Host-plumblog_session";
pub const TOKEN_BYTES: usize = 32;
/// 30 days.
pub const LIFETIME_MS: i64 = 30 * 24 * 60 * 60 * 1000;

/// A syntactically valid session token (base64url of 32 bytes). `Debug` never prints it.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionToken(String);

impl SessionToken {
    /// `None` unless `text` is strict base64url of exactly [`TOKEN_BYTES`] bytes, which also
    /// guarantees it is safe inside a `Set-Cookie` header.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let bytes = base64url::decode(text)?;
        (bytes.len() == TOKEN_BYTES).then(|| Self(text.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SessionToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionToken(<redacted>)")
    }
}

/// A fresh random token and the hash to store for it.
///
/// # Errors
///
/// A [`Crypto`] error (RNG or digest failure) passes through.
pub async fn new_token(crypto: &impl Crypto) -> Result<(SessionToken, String)> {
    let mut bytes = [0u8; TOKEN_BYTES];
    crypto.random_bytes(&mut bytes)?;
    let token = SessionToken(base64url::encode(&bytes));
    let hash = hash_token(crypto, &token).await?;
    Ok((token, hash))
}

/// Lowercase hex SHA-256 of the token text, the `sessions.token_hash` value.
///
/// # Errors
///
/// A [`Crypto`] error passes through.
pub async fn hash_token(crypto: &impl Crypto, token: &SessionToken) -> Result<String> {
    Ok(base64url::hex(&crypto.sha256(token.0.as_bytes()).await?))
}

/// Expiry of a session created or renewed at `now`.
#[must_use]
pub fn expires_at(now: i64) -> i64 {
    now.saturating_add(LIFETIME_MS)
}

/// Renew (one write) only once less than half the lifetime remains, so an active user costs
/// about one write per 15 days, not one per request. An expired session is never renewed.
#[must_use]
pub fn needs_renewal(expires_at: i64, now: i64) -> bool {
    now < expires_at && expires_at.saturating_sub(now) < LIFETIME_MS / 2
}

/// `Set-Cookie` value carrying `token` until `expires_at`. `Max-Age` is the whole seconds left,
/// rounded down so the browser never keeps the cookie past the server's expiry.
#[must_use]
pub fn set_cookie(token: &SessionToken, expires_at: i64, now: i64) -> String {
    let max_age = expires_at.saturating_sub(now).max(0) / 1000;
    format!(
        "{COOKIE_NAME}={}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age={max_age}",
        token.0
    )
}

/// `Set-Cookie` value that deletes the session cookie (sign-out, refused session).
#[must_use]
pub fn clear_cookie() -> String {
    format!("{COOKIE_NAME}=; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=0")
}

/// The session token from a `Cookie` request header. Pairs without `=` are skipped; `None`
/// when the cookie is absent, malformed, or present more than once (ambiguous: never guess
/// which one the user meant).
#[must_use]
pub fn token_from_cookie_header(header: &str) -> Option<SessionToken> {
    let mut found = None;
    for pair in header.split(';') {
        let Some((name, value)) = pair.trim_matches([' ', '\t']).split_once('=') else {
            continue;
        };
        if name == COOKIE_NAME {
            if found.is_some() {
                return None;
            }
            found = Some(value);
        }
    }
    SessionToken::parse(found?)
}

#[cfg(test)]
mod tests {
    use super::{
        LIFETIME_MS, SessionToken, clear_cookie, expires_at, hash_token, needs_renewal, new_token,
        set_cookie, token_from_cookie_header,
    };
    use crate::base64url::{encode, hex};
    use crate::testutil::{FakeCrypto, block_on, sha256};
    use plumblog_model::ErrorCode;

    #[test]
    fn new_tokens_are_random_and_hashed() {
        let crypto = FakeCrypto::new();
        let (a, ha) = block_on(new_token(&crypto)).unwrap();
        let (b, hb) = block_on(new_token(&crypto)).unwrap();
        assert_ne!(a, b);
        assert_eq!(a.as_str().len(), 43);
        assert_eq!(ha, hex(&sha256(a.as_str().as_bytes())));
        assert_eq!(ha.len(), 64);
        assert_ne!(ha, hb);
        assert_eq!(block_on(hash_token(&crypto, &a)).unwrap(), ha);
        assert_eq!(format!("{a:?}"), "SessionToken(<redacted>)");
        crypto.rng_fails.set(true);
        assert_eq!(
            block_on(new_token(&crypto)).unwrap_err().code,
            ErrorCode::Internal
        );
    }

    #[test]
    fn lifetime_and_renewal() {
        let now = 1_700_000_000_000;
        let exp = expires_at(now);
        assert_eq!(exp - now, 30 * 86_400_000);
        assert!(!needs_renewal(exp, now));
        assert!(!needs_renewal(exp, now + LIFETIME_MS / 2)); // exactly half left
        assert!(needs_renewal(exp, now + LIFETIME_MS / 2 + 1));
        assert!(needs_renewal(exp, exp - 1));
        assert!(!needs_renewal(exp, exp)); // expired
        assert!(!needs_renewal(exp, exp + 1));
        assert_eq!(expires_at(i64::MAX), i64::MAX);
        assert!(!needs_renewal(i64::MIN, i64::MAX));
    }

    #[test]
    fn cookie_headers() {
        let t = SessionToken::parse(&encode(&[1; 32])).unwrap();
        assert_eq!(
            set_cookie(&t, 10_999, 1_000),
            format!(
                "__Host-plumblog_session={}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=9",
                t.as_str()
            )
        );
        assert!(set_cookie(&t, 0, 5_000).ends_with("Max-Age=0"));
        assert!(set_cookie(&t, expires_at(0), 0).ends_with("Max-Age=2592000"));
        assert_eq!(
            clear_cookie(),
            "__Host-plumblog_session=; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=0"
        );
    }

    #[test]
    fn cookie_parsing_is_strict() {
        let v = encode(&[2; 32]);
        let want = SessionToken::parse(&v);
        assert!(want.is_some());
        for header in [
            format!("__Host-plumblog_session={v}"),
            format!("a=b; __Host-plumblog_session={v}; c=d"),
            format!("garbage;  __Host-plumblog_session={v};;=x; ; theme=dark"),
            format!("\t__Host-plumblog_session={v}\t"),
            format!("x=__Host-plumblog_session={v}; __Host-plumblog_session={v}"),
        ] {
            assert_eq!(token_from_cookie_header(&header), want, "{header:?}");
        }
        for header in [
            String::new(),
            "a=b".into(),
            format!("__host-plumblog_session={v}"),
            format!("__Host-plumblog_session ={v}"),
            format!("plumblog_session={v}"),
            format!("__Host-plumblog_session=\"{v}\""),
            format!("__Host-plumblog_session={v}="),
            format!("__Host-plumblog_session={v} x"),
            format!("__Host-plumblog_session={}", encode(&[2; 31])),
            format!("__Host-plumblog_session={}", encode(&[2; 33])),
            "__Host-plumblog_session=".into(),
            "__Host-plumblog_session".into(),
            format!("__Host-plumblog_session={v}; __Host-plumblog_session={v}"),
            format!("__Host-plumblog_session=bad; __Host-plumblog_session={v}"),
        ] {
            assert_eq!(token_from_cookie_header(&header), None, "{header:?}");
        }
    }
}
