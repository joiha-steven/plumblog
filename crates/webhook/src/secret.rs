//! Signing secrets: made from 32 random bytes, shown once as `whsec_<base64>`, stored sealed
//! with AES-256-GCM under the Worker secret `WEBHOOK_KEY` as `v1:<nonce>:<ciphertext+tag>`
//! (both base64). A row in any other format is refused, never used as a plaintext key.

use std::fmt;

use plumblog_model::{Error, ErrorCode, Result};

use crate::{WebhookCrypto, b64};

/// What users paste into their receiver.
pub const SECRET_PREFIX: &str = "whsec_";
/// Secret and `WEBHOOK_KEY` length in bytes.
pub const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;
const TAG_LEN: usize = 16;
const SEALED_PREFIX: &str = "v1:";

/// The decoded `WEBHOOK_KEY`. `Debug` never prints it.
#[derive(Clone, PartialEq, Eq)]
pub struct SealKey([u8; KEY_LEN]);

impl fmt::Debug for SealKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SealKey(..)")
    }
}

impl SealKey {
    /// Reads `WEBHOOK_KEY` (base64 of 32 bytes; surrounding whitespace ignored).
    ///
    /// # Errors
    /// [`ErrorCode::WebhookKeyInvalid`] when it is missing, not base64, or not 32 bytes.
    pub fn parse(raw: Option<&str>) -> Result<Self> {
        let raw = raw
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Error::new(ErrorCode::WebhookKeyInvalid, "WEBHOOK_KEY is not set"))?;
        let bytes = b64::decode(raw)
            .ok_or_else(|| Error::new(ErrorCode::WebhookKeyInvalid, "WEBHOOK_KEY is not base64"))?;
        let len = bytes.len();
        <[u8; KEY_LEN]>::try_from(bytes).map(Self).map_err(|_| {
            Error::new(
                ErrorCode::WebhookKeyInvalid,
                format!("WEBHOOK_KEY is {len} bytes, wanted {KEY_LEN}"),
            )
        })
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }
}

/// A signing secret in the clear. `Debug` never prints it.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret([u8; KEY_LEN]);

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(..)")
    }
}

impl Secret {
    /// 32 bytes from the platform RNG.
    ///
    /// # Errors
    /// The RNG's error; there is no weaker fallback.
    pub fn generate<C: WebhookCrypto>(crypto: &C) -> Result<Self> {
        let mut b = [0u8; KEY_LEN];
        crypto.random_bytes(&mut b)?;
        Ok(Self(b))
    }

    /// The HMAC key.
    #[must_use]
    pub fn bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    /// `whsec_<base64>`, the form shown to the user once.
    #[must_use]
    pub fn display(&self) -> String {
        format!("{SECRET_PREFIX}{}", b64::encode(&self.0))
    }

    /// Reads the `whsec_` form back (receivers and tests).
    #[must_use]
    pub fn parse_display(s: &str) -> Option<Self> {
        let bytes = b64::decode(s.strip_prefix(SECRET_PREFIX)?)?;
        <[u8; KEY_LEN]>::try_from(bytes).ok().map(Self)
    }
}

/// Seals `secret` for `webhooks.secret` with a fresh random nonce.
///
/// # Errors
/// The crypto backend's error (RNG or encryption).
pub async fn seal<C: WebhookCrypto>(crypto: &C, key: &SealKey, secret: &Secret) -> Result<String> {
    let mut nonce = [0u8; NONCE_LEN];
    crypto.random_bytes(&mut nonce)?;
    let ct = crypto
        .aes_gcm_encrypt(key.bytes(), &nonce, secret.bytes())
        .await?;
    Ok(format!(
        "{SEALED_PREFIX}{}:{}",
        b64::encode(&nonce),
        b64::encode(&ct)
    ))
}

fn unreadable(why: &str) -> Error {
    // Never the stored value: a plaintext row would leak its secret into the log.
    Error::new(ErrorCode::WebhookSecretUnreadable, why)
}

/// Opens a stored secret.
///
/// # Errors
/// [`ErrorCode::WebhookSecretUnreadable`] when `stored` is not `v1:` sealed (e.g. a
/// plaintext secret), is malformed, or does not decrypt under `key` (wrong key, tampered).
pub async fn open<C: WebhookCrypto>(crypto: &C, key: &SealKey, stored: &str) -> Result<Secret> {
    let rest = stored
        .strip_prefix(SEALED_PREFIX)
        .ok_or_else(|| unreadable("stored secret is not sealed (no v1: prefix); refusing it"))?;
    let (nonce, ct) = rest
        .split_once(':')
        .ok_or_else(|| unreadable("sealed secret has no nonce separator"))?;
    let nonce = b64::decode(nonce)
        .and_then(|n| <[u8; NONCE_LEN]>::try_from(n).ok())
        .ok_or_else(|| unreadable("sealed secret nonce is not 12 base64 bytes"))?;
    let ct = b64::decode(ct)
        .filter(|c| c.len() == KEY_LEN + TAG_LEN)
        .ok_or_else(|| unreadable("sealed secret ciphertext is not 48 base64 bytes"))?;
    let plain = crypto
        .aes_gcm_decrypt(key.bytes(), &nonce, &ct)
        .await
        .map_err(|e| unreadable(&format!("sealed secret does not decrypt: {}", e.detail)))?;
    <[u8; KEY_LEN]>::try_from(plain)
        .map(Secret)
        .map_err(|_| unreadable("sealed secret decrypts to the wrong length"))
}

#[cfg(test)]
mod tests {
    use super::{SealKey, Secret, open, seal};
    use crate::b64;
    use crate::tests::{FakeCrypto, run};
    use plumblog_model::ErrorCode;

    fn key() -> SealKey {
        SealKey::parse(Some(&b64::encode(&[7u8; 32]))).unwrap()
    }

    #[test]
    fn missing_or_malformed_key_is_a_stable_error() {
        for raw in [
            None,
            Some(""),
            Some("  \n"),
            Some("not base64!"),
            Some(b64::encode(&[1u8; 16]).as_str()),
            Some(b64::encode(&[1u8; 33]).as_str()),
        ] {
            let e = SealKey::parse(raw).unwrap_err();
            assert_eq!(e.code, ErrorCode::WebhookKeyInvalid, "{raw:?}");
        }
        let padded = format!(" {}\n", b64::encode(&[9u8; 32]));
        assert_eq!(SealKey::parse(Some(&padded)).unwrap().bytes(), &[9u8; 32]);
    }

    #[test]
    fn debug_never_prints_key_material() {
        let s = Secret([0xab; 32]);
        assert_eq!(format!("{s:?} {:?}", key()), "Secret(..) SealKey(..)");
    }

    #[test]
    fn display_form_round_trips() {
        let c = FakeCrypto::default();
        let s = Secret::generate(&c).unwrap();
        let shown = s.display();
        assert!(shown.starts_with("whsec_"), "{shown}");
        assert_eq!(shown.len(), "whsec_".len() + 44);
        assert_eq!(Secret::parse_display(&shown), Some(s));
        assert_eq!(Secret::parse_display("whsec_AAAA"), None);
        assert_eq!(Secret::parse_display(&shown["whsec_".len()..]), None);
    }

    #[test]
    fn seal_open_round_trip_with_fresh_nonces() {
        let c = FakeCrypto::default();
        let s = Secret::generate(&c).unwrap();
        let a = run(seal(&c, &key(), &s)).unwrap();
        let b = run(seal(&c, &key(), &s)).unwrap();
        assert!(a.starts_with("v1:"), "{a}");
        assert_eq!(a.split(':').count(), 3);
        assert_ne!(a, b, "each seal uses a new nonce");
        assert!(
            !a.contains(&b64::encode(s.bytes())),
            "sealed form hides the secret"
        );
        assert_eq!(run(open(&c, &key(), &a)).unwrap(), s);
        assert_eq!(run(open(&c, &key(), &b)).unwrap(), s);
    }

    #[test]
    fn plaintext_wrong_key_and_tampering_are_refused() {
        let c = FakeCrypto::default();
        let s = Secret::generate(&c).unwrap();
        let sealed = run(seal(&c, &key(), &s)).unwrap();
        let other = SealKey::parse(Some(&b64::encode(&[8u8; 32]))).unwrap();
        let mut tampered = sealed.clone();
        let last = tampered.pop().unwrap();
        let swap = if last == 'A' { 'Q' } else { 'A' };
        // Keeps the length, changes the tag's last bits or padding: either way unreadable.
        tampered.push(swap);
        for stored in [
            s.display(),
            b64::encode(s.bytes()),
            "s3cret".to_owned(),
            String::new(),
            "v1:".to_owned(),
            "v1:AAAA:AAAA".to_owned(),
            "v2:".to_owned() + &sealed[3..],
            tampered,
        ] {
            let e = run(open(&c, &key(), &stored)).unwrap_err();
            assert_eq!(e.code, ErrorCode::WebhookSecretUnreadable, "{stored:?}");
            assert!(!e.detail.contains(&b64::encode(s.bytes())), "{}", e.detail);
        }
        let e = run(open(&c, &other, &sealed)).unwrap_err();
        assert_eq!(e.code, ErrorCode::WebhookSecretUnreadable);
    }
}
