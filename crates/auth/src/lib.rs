//! Passwords, passkeys, sessions and role checks. Crypto sits behind a trait: the Worker
//! implements it with `WebCrypto`, tests with a fake, so this crate never touches the runtime.
//! Everything here is pure: no clock (callers pass `now`), no storage, no network.

pub mod base64url;
pub mod cbor;
pub mod password;
pub mod ratelimit;
pub mod session;
#[cfg(test)]
mod testutil;
pub mod webauthn;

use plumblog_model::{Error, ErrorCode, Permission, Result, Role};
use webauthn::{Es256Key, Rs256Key};

/// What auth needs from the platform. Async because `WebCrypto` is.
// Workers run one request per isolate thread, so futures need not be `Send`; requiring it
// would rule out the WebCrypto implementation.
#[allow(async_fn_in_trait)]
pub trait Crypto {
    async fn pbkdf2_sha256(
        &self,
        password: &[u8],
        salt: &[u8],
        iterations: u32,
    ) -> Result<[u8; 32]>;
    /// Fills `buf` from a cryptographic RNG.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::Internal`] when the platform RNG fails; never fall back to a weak RNG.
    fn random_bytes(&self, buf: &mut [u8]) -> Result<()>;
    async fn sha256(&self, data: &[u8]) -> Result<[u8; 32]>;
    /// ECDSA P-256 with SHA-256 over `message` (the implementation hashes it). `signature` is
    /// the raw `r || s` `WebCrypto` expects (see [`webauthn::der`]); import the key as the raw
    /// [`Es256Key::uncompressed_point`].
    ///
    /// # Errors
    ///
    /// Only for platform failures, including a key the platform refuses to import. A signature
    /// that does not verify is `Ok(false)`.
    async fn verify_es256(
        &self,
        key: &Es256Key,
        message: &[u8],
        signature: &[u8; 64],
    ) -> Result<bool>;
    /// RSASSA-PKCS1-v1_5 with SHA-256 over `message`; import the key as a JWK from `n` and `e`.
    /// `signature` is exactly as long as the modulus.
    ///
    /// # Errors
    ///
    /// As [`Crypto::verify_es256`].
    async fn verify_rs256(&self, key: &Rs256Key, message: &[u8], signature: &[u8]) -> Result<bool>;
    /// Ed25519 over `message`; import the key in raw form.
    ///
    /// # Errors
    ///
    /// As [`Crypto::verify_es256`].
    async fn verify_ed25519(
        &self,
        key: &[u8; 32],
        message: &[u8],
        signature: &[u8; 64],
    ) -> Result<bool>;
}

/// Role check that returns the stable error, so every route denies the same way.
///
/// # Errors
///
/// [`ErrorCode::Forbidden`] when `role` lacks `permission`.
pub fn require(role: Role, permission: Permission) -> Result<()> {
    if role.can(permission) {
        Ok(())
    } else {
        Err(Error::new(
            ErrorCode::Forbidden,
            format!("{role:?} lacks {permission:?}"),
        ))
    }
}

/// Compares secrets without an early exit, so response timing does not leak how many
/// leading bytes matched.
#[must_use]
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::{constant_time_eq, require};
    use plumblog_model::{ErrorCode, Permission, Role};

    #[test]
    fn require_denies_writer_settings_with_stable_code() {
        assert!(require(Role::Owner, Permission::ManageSettings).is_ok());
        let err = require(Role::Writer, Permission::ManageSettings).unwrap_err();
        assert_eq!(err.code, ErrorCode::Forbidden);
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
