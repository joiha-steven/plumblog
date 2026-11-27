//! API tokens (`Authorization: Bearer plb_...`). Like sessions, only the SHA-256 of the full
//! token text is stored, so a leaked database cannot be replayed; the token is shown once.

use crate::{Crypto, base64url};
use plumblog_model::Result;
use std::fmt;

/// Marks the text as a plumblog token, so secret scanners and people can recognise a leak.
pub const PREFIX: &str = "plb_";
pub const TOKEN_BYTES: usize = 32;

/// A syntactically valid API token: [`PREFIX`] + strict base64url of [`TOKEN_BYTES`] bytes
/// (43 characters). `Debug` never prints it.
#[derive(Clone, PartialEq, Eq)]
pub struct ApiTokenSecret(String);

impl ApiTokenSecret {
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let rest = text.strip_prefix(PREFIX)?;
        let bytes = base64url::decode(rest)?;
        (bytes.len() == TOKEN_BYTES).then(|| Self(text.to_owned()))
    }

    /// The token from an `Authorization` header value: scheme `Bearer` (any case, as HTTP
    /// schemes are case-insensitive), one space, the token, nothing else. Anything else is
    /// `None`; the caller answers 401 and never falls back to another credential.
    #[must_use]
    pub fn from_authorization(header: &str) -> Option<Self> {
        let (scheme, token) = header.split_once(' ')?;
        if !scheme.eq_ignore_ascii_case("bearer") {
            return None;
        }
        Self::parse(token)
    }

    /// The full token text, to show the user once.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiTokenSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiTokenSecret(<redacted>)")
    }
}

/// A fresh random token and the hash to store for it.
///
/// # Errors
///
/// A [`Crypto`] error (RNG or digest failure) passes through.
pub async fn new_token(crypto: &impl Crypto) -> Result<(ApiTokenSecret, String)> {
    let mut bytes = [0u8; TOKEN_BYTES];
    crypto.random_bytes(&mut bytes)?;
    let token = ApiTokenSecret(format!("{PREFIX}{}", base64url::encode(&bytes)));
    let hash = hash_token(crypto, &token).await?;
    Ok((token, hash))
}

/// Lowercase hex SHA-256 of the full token text (prefix included), the
/// `api_tokens.token_hash` value.
///
/// # Errors
///
/// A [`Crypto`] error passes through.
pub async fn hash_token(crypto: &impl Crypto, token: &ApiTokenSecret) -> Result<String> {
    Ok(base64url::hex(&crypto.sha256(token.0.as_bytes()).await?))
}

#[cfg(test)]
mod tests {
    use super::{ApiTokenSecret, hash_token, new_token};
    use crate::base64url::{encode, hex};
    use crate::testutil::{FakeCrypto, block_on, sha256};
    use plumblog_model::ErrorCode;

    #[test]
    fn new_tokens_are_random_prefixed_and_hashed() {
        let crypto = FakeCrypto::new();
        let (a, ha) = block_on(new_token(&crypto)).unwrap();
        let (b, hb) = block_on(new_token(&crypto)).unwrap();
        assert_ne!(a, b);
        assert_ne!(ha, hb);
        assert!(a.as_str().starts_with("plb_"));
        assert_eq!(a.as_str().len(), 4 + 43);
        assert_eq!(ApiTokenSecret::parse(a.as_str()), Some(a.clone()));
        assert_eq!(ha, hex(&sha256(a.as_str().as_bytes())));
        assert_eq!(block_on(hash_token(&crypto, &a)).unwrap(), ha);
        assert_eq!(format!("{a:?}"), "ApiTokenSecret(<redacted>)");
        crypto.rng_fails.set(true);
        assert_eq!(
            block_on(new_token(&crypto)).unwrap_err().code,
            ErrorCode::Internal
        );
    }

    #[test]
    fn authorization_header_is_strict() {
        let v = format!("plb_{}", encode(&[7; 32]));
        let want = ApiTokenSecret::parse(&v);
        assert!(want.is_some());
        for header in [
            format!("Bearer {v}"),
            format!("bearer {v}"),
            format!("BEARER {v}"),
        ] {
            assert_eq!(
                ApiTokenSecret::from_authorization(&header),
                want,
                "{header:?}"
            );
        }
        let body = encode(&[7; 32]);
        for header in [
            String::new(),
            "Bearer".into(),
            "Bearer ".into(),
            v.clone(),
            format!("Bearer  {v}"),
            format!("Bearer {v} "),
            format!(" Bearer {v}"),
            format!("Bearer\t{v}"),
            format!("Basic {v}"),
            format!("Token {v}"),
            format!("Bearer {body}"),
            format!("Bearer PLB_{body}"),
            format!("Bearer plb-{body}"),
            format!("Bearer plb_{}", encode(&[7; 31])),
            format!("Bearer plb_{}", encode(&[7; 33])),
            format!("Bearer plb_{body}="),
            format!("Bearer plb_+{}", &body[1..]),
            format!("Bearer {v}, Bearer {v}"),
            format!("Bearer {v},{v}"),
            // Non-zero unused trailing bits: a second spelling of the same bytes.
            format!("Bearer plb_{}d", &body[..42]),
        ] {
            assert_eq!(
                ApiTokenSecret::from_authorization(&header),
                None,
                "{header:?}"
            );
        }
    }
}
