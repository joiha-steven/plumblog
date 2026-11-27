//! Passwords. The browser stretches the password (PBKDF2-SHA256, [`STRETCH_ITERATIONS`], salt
//! from [`stretch_salt`]) so the Worker never sees it and stays inside its CPU budget; the server
//! then hashes the 32-byte stretched value once more with a per-user random salt, so a leaked
//! database row cannot be replayed as a sign-in.
//!
//! Stored form: `p1$<iterations>$<salt b64url>$<hash b64url>` (16-byte salt, 32-byte hash).

use crate::{Crypto, base64url, constant_time_eq};
use plumblog_model::{Error, ErrorCode, Result};

/// Browser-side PBKDF2-SHA256 iterations. `web/admin/js/stretch.js` must use the same value.
pub const STRETCH_ITERATIONS: u32 = 600_000;
/// Length of the stretched value the browser sends (base64url, see [`parse_stretched`]).
pub const STRETCHED_LEN: usize = 32;
/// Server-side PBKDF2-SHA256 iterations over the stretched value.
pub const SERVER_ITERATIONS: u32 = 1_000;
/// Highest iteration count accepted from a stored hash: a corrupt row must not burn the
/// request's CPU budget.
pub const MAX_SERVER_ITERATIONS: u32 = 10_000;
pub const SALT_LEN: usize = 16;
const SCHEME: &str = "p1";

/// The browser's PBKDF2 salt: `"plumblog:" + site_id + ":" + lowercase(login)`. Deterministic,
/// so the sign-in page needs no per-user salt lookup (which would reveal who has an account).
/// Lowercasing is Unicode default (`toLowerCase()` in the browser).
#[must_use]
pub fn stretch_salt(site_id: &str, login: &str) -> String {
    format!("plumblog:{site_id}:{}", login.to_lowercase())
}

/// Decodes the stretched value sent by the browser.
///
/// # Errors
///
/// [`ErrorCode::InvalidInput`] unless it is strict base64url of exactly 32 bytes.
pub fn parse_stretched(text: &str) -> Result<[u8; STRETCHED_LEN]> {
    base64url::decode(text)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| {
            Error::new(
                ErrorCode::InvalidInput,
                "stretched password is not 32 bytes of base64url",
            )
        })
}

/// A new stored hash with a fresh random salt.
///
/// # Errors
///
/// A [`Crypto`] error (RNG or PBKDF2 failure) passes through.
pub async fn hash_password(
    crypto: &impl Crypto,
    stretched: &[u8; STRETCHED_LEN],
) -> Result<String> {
    let mut salt = [0u8; SALT_LEN];
    crypto.random_bytes(&mut salt)?;
    let hash = crypto
        .pbkdf2_sha256(stretched, &salt, SERVER_ITERATIONS)
        .await?;
    Ok(format!(
        "{SCHEME}${SERVER_ITERATIONS}${}${}",
        base64url::encode(&salt),
        base64url::encode(&hash)
    ))
}

struct Stored {
    iterations: u32,
    salt: [u8; SALT_LEN],
    hash: [u8; 32],
}

fn parse_stored(stored: &str) -> Result<Stored> {
    let corrupt = |why: &str| {
        Error::new(
            ErrorCode::Internal,
            format!("stored password hash is unreadable: {why}"),
        )
    };
    let mut parts = stored.split('$');
    let (Some(scheme), Some(iters), Some(salt), Some(hash), None) = (
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
        parts.next(),
    ) else {
        return Err(corrupt("expected 4 '$'-separated fields"));
    };
    if scheme != SCHEME {
        return Err(corrupt("unknown scheme"));
    }
    // Digits only, no sign or leading zero, so each count has one spelling.
    if iters.is_empty() || iters.starts_with('0') || !iters.bytes().all(|b| b.is_ascii_digit()) {
        return Err(corrupt("bad iteration count"));
    }
    let iterations: u32 = iters.parse().map_err(|_| corrupt("bad iteration count"))?;
    if !(SERVER_ITERATIONS..=MAX_SERVER_ITERATIONS).contains(&iterations) {
        return Err(corrupt("iteration count out of range"));
    }
    let salt = base64url::decode(salt)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| corrupt("salt is not 16 bytes of base64url"))?;
    let hash = base64url::decode(hash)
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| corrupt("hash is not 32 bytes of base64url"))?;
    Ok(Stored {
        iterations,
        salt,
        hash,
    })
}

/// Checks a stretched value against a stored hash, comparing in constant time.
///
/// # Errors
///
/// [`ErrorCode::Internal`] when `stored` is not a hash this version can read: refuse the
/// sign-in and log it (the detail never contains the hash). A [`Crypto`] error passes through.
pub async fn verify_password(
    crypto: &impl Crypto,
    stored: &str,
    stretched: &[u8; STRETCHED_LEN],
) -> Result<bool> {
    let s = parse_stored(stored)?;
    let got = crypto
        .pbkdf2_sha256(stretched, &s.salt, s.iterations)
        .await?;
    Ok(constant_time_eq(&got, &s.hash))
}

/// Same work as [`verify_password`] against a fixed hash, for unknown logins, so a sign-in
/// takes as long whether the account exists or not. Always `Ok(false)`.
///
/// # Errors
///
/// A [`Crypto`] error passes through.
pub async fn dummy_verify(crypto: &impl Crypto, stretched: &[u8; STRETCHED_LEN]) -> Result<bool> {
    // A well-formed hash of nothing anyone can send: parsed like a real row, same iterations.
    const DUMMY: &str =
        "p1$1000$AAAAAAAAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    let matched = verify_password(crypto, DUMMY, stretched).await?;
    // Consume the result so the comparison is not optimised away, but never let it through.
    std::hint::black_box(matched);
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::{
        SERVER_ITERATIONS, dummy_verify, hash_password, parse_stretched, stretch_salt,
        verify_password,
    };
    use crate::base64url;
    use crate::testutil::{FakeCrypto, block_on};
    use plumblog_model::ErrorCode;

    #[test]
    fn salt_builder_lowercases_login_only() {
        assert_eq!(stretch_salt("S1te", "Alice"), "plumblog:S1te:alice");
        assert_eq!(stretch_salt("x", "\u{c9}MILE"), "plumblog:x:\u{e9}mile");
    }

    #[test]
    fn hash_then_verify() {
        let crypto = FakeCrypto::new();
        let pw = [7u8; 32];
        let stored = block_on(hash_password(&crypto, &pw)).unwrap();
        let parts: Vec<&str> = stored.split('$').collect();
        assert_eq!(parts[0], "p1");
        assert_eq!(parts[1], "1000");
        assert_eq!(base64url::decode(parts[2]).unwrap().len(), 16);
        assert_eq!(base64url::decode(parts[3]).unwrap().len(), 32);
        assert!(block_on(verify_password(&crypto, &stored, &pw)).unwrap());
        assert!(!block_on(verify_password(&crypto, &stored, &[8u8; 32])).unwrap());
        // Fresh salt each time.
        let again = block_on(hash_password(&crypto, &pw)).unwrap();
        assert_ne!(stored, again);
        assert!(block_on(verify_password(&crypto, &again, &pw)).unwrap());
    }

    #[test]
    fn rng_failure_is_an_error_not_a_weak_salt() {
        let crypto = FakeCrypto::new();
        crypto.rng_fails.set(true);
        let err = block_on(hash_password(&crypto, &[1; 32])).unwrap_err();
        assert_eq!(err.code, ErrorCode::Internal);
        assert!(crypto.pbkdf2_calls.borrow().is_empty());
    }

    #[test]
    fn garbled_hashes_are_refused_without_running_pbkdf2() {
        let crypto = FakeCrypto::new();
        let good = block_on(hash_password(&crypto, &[1; 32])).unwrap();
        let (salt, hash) = {
            let p: Vec<&str> = good.split('$').collect();
            (p[2].to_string(), p[3].to_string())
        };
        crypto.pbkdf2_calls.borrow_mut().clear();
        let bad = [
            String::new(),
            "p1".into(),
            "plain-text-password".into(),
            format!("p2$1000${salt}${hash}"),
            format!("P1$1000${salt}${hash}"),
            format!("p1$01000${salt}${hash}"),
            format!("p1$+1000${salt}${hash}"),
            format!("p1$-1000${salt}${hash}"),
            format!("p1$999${salt}${hash}"),
            format!("p1$10001${salt}${hash}"),
            format!("p1$99999999999${salt}${hash}"),
            format!("p1$${salt}${hash}"),
            format!("p1$1000$${hash}"),
            format!("p1$1000${salt}$"),
            format!("p1$1000${salt}=${hash}"),
            format!("p1$1000${salt}A${hash}"),
            format!("p1$1000${salt}${hash}AAAA"),
            format!("p1$1000${salt}${hash}$"),
            format!("p1$1000${salt}"),
            format!("$p1$1000${salt}${hash}"),
            format!("p1 $1000${salt}${hash}"),
            "$pbkdf2-sha256$29000$abc$def".into(),
        ];
        for stored in bad {
            let err = block_on(verify_password(&crypto, &stored, &[1; 32])).unwrap_err();
            assert_eq!(err.code, ErrorCode::Internal, "{stored:?}");
            assert!(!err.detail.contains(&hash), "detail leaks the hash");
        }
        assert!(crypto.pbkdf2_calls.borrow().is_empty());
        // Higher counts within the cap still verify (room to raise SERVER_ITERATIONS later).
        assert!(
            !block_on(verify_password(
                &crypto,
                &format!("p1$5000${salt}${hash}"),
                &[1; 32]
            ))
            .unwrap()
        );
    }

    #[test]
    fn dummy_verify_costs_the_same_and_never_matches() {
        let crypto = FakeCrypto::new();
        let stored = block_on(hash_password(&crypto, &[0; 32])).unwrap();
        crypto.pbkdf2_calls.borrow_mut().clear();
        assert!(!block_on(dummy_verify(&crypto, &[0; 32])).unwrap());
        assert!(!block_on(verify_password(&crypto, &stored, &[9; 32])).unwrap());
        let calls = crypto.pbkdf2_calls.borrow();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].2, SERVER_ITERATIONS);
        assert_eq!(calls[0].2, calls[1].2);
        assert_eq!(calls[0].1.len(), calls[1].1.len());
    }

    #[test]
    fn stretched_value_is_strict() {
        let ok = base64url::encode(&[3; 32]);
        assert_eq!(parse_stretched(&ok).unwrap(), [3; 32]);
        for bad in [
            String::new(),
            base64url::encode(&[3; 31]),
            base64url::encode(&[3; 33]),
            format!("{ok}="),
            format!(" {ok}"),
            "not base64!".into(),
        ] {
            assert_eq!(
                parse_stretched(&bad).unwrap_err().code,
                ErrorCode::InvalidInput
            );
        }
    }
}
