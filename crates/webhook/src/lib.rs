//! Outgoing webhooks (issue #18), all pure: events and payloads, Standard Webhooks signing,
//! secrets sealed at rest, the URL guard, the retry schedule, and the flows that queue and
//! deliver. Crypto and HTTP are traits the Worker implements (`WebCrypto`, `fetch`); storage is
//! `plumblog_store`. Nothing here reads a clock or does I/O on its own.

mod b64;
mod deliver;
mod event;
mod payload;
mod retry;
mod secret;
mod sign;
pub mod ssrf;
#[cfg(test)]
mod tests;

pub use deliver::{
    Created, DELIVERY_JOB, PRUNE_JOB, Run, Sender, create_webhook, enqueue, prune_job,
    rotate_secret,
};
pub use event::{WebhookEvent, events_column, wants};
pub use payload::{PostInfo, build as build_payload, rfc3339};
pub use retry::{
    Failure, HttpError, KEEP_MS, LEASE_MS, MAX_ATTEMPTS, MAX_PER_RUN, Outcome, PRUNE_PER_RUN,
    RETRY_DELAYS_MS, classify, next_attempt_at,
};
pub use secret::{KEY_LEN, SECRET_PREFIX, SealKey, Secret, open, seal};
pub use sign::{headers, signature_header, signed_content};
pub use ssrf::{Blocked, Policy, Target};

use plumblog_model::Result;

/// The platform crypto webhooks need (`WebCrypto` in the Worker). Separate from
/// `plumblog_auth::Crypto` so the two can change independently.
// Workers are single-threaded, so futures need not be `Send` (see plumblog_auth::Crypto).
#[allow(async_fn_in_trait)]
pub trait WebhookCrypto {
    async fn hmac_sha256(&self, key: &[u8], msg: &[u8]) -> Result<[u8; 32]>;
    /// AES-256-GCM, no associated data. Returns ciphertext followed by the 16-byte tag
    /// (the `WebCrypto` layout).
    async fn aes_gcm_encrypt(
        &self,
        key: &[u8; 32],
        nonce: &[u8; 12],
        plaintext: &[u8],
    ) -> Result<Vec<u8>>;
    /// Inverse of [`Self::aes_gcm_encrypt`].
    ///
    /// # Errors
    /// When the tag does not verify (wrong key, tampered data) or the platform fails.
    async fn aes_gcm_decrypt(
        &self,
        key: &[u8; 32],
        nonce: &[u8; 12],
        sealed: &[u8],
    ) -> Result<Vec<u8>>;
    /// Fills `buf` from a cryptographic RNG.
    ///
    /// # Errors
    /// When the platform RNG fails; never fall back to a weak one.
    fn random_bytes(&self, buf: &mut [u8]) -> Result<()>;
}

/// Longest one delivery may take. A full run of [`retry::MAX_PER_RUN`] slow receivers must
/// end before its lease does, or the next run would claim and send the same rows again.
pub const HTTP_TIMEOUT_MS: i64 = 10_000;
const _: () = assert!(
    (retry::MAX_PER_RUN as i64) * HTTP_TIMEOUT_MS < retry::LEASE_MS,
    "a run of slow receivers must end before its lease"
);

/// Sends one delivery (`fetch` in the Worker). The implementation must not follow redirects
/// (`redirect: "manual"`, so a 3xx comes back as its status) and must give up after
/// [`HTTP_TIMEOUT_MS`], so one slow receiver cannot hold the whole run.
#[allow(async_fn_in_trait)]
pub trait WebhookHttp {
    async fn post(
        &self,
        url: &str,
        headers: &[(&'static str, String)],
        body: &str,
    ) -> std::result::Result<u16, HttpError>;
}
