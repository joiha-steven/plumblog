//! Sign-in protection (`login_failures`, `auth_challenges`) and first-owner setup.
//!
//! The store only ever sees hashes: the caller hashes logins, IPs, challenges and setup
//! codes before calling, so a database dump reveals none of them.

use plumblog_model::{Result, UserId, stable_codes};

use crate::NewUser;

stable_codes! {
    /// Stored in `auth_challenges.purpose`.
    pub enum ChallengePurpose {
        Register => "register",
        Login => "login",
    }
}

/// Most keys one [`LoginLimitStore::failures`] call may read (one attempt checks a login and
/// an IP; the cap keeps the query within D1's parameter limit).
pub const MAX_FAILURE_KEYS: usize = 16;

/// Settings key holding the setup code hash, as a JSON string.
pub const SETUP_CODE_HASH_KEY: &str = "setup_code_hash";
/// Settings key holding when the setup code expires (ms), as a JSON integer.
pub const SETUP_CODE_EXPIRES_KEY: &str = "setup_code_expires_at";

/// Settings key holding the site's random id (a JSON string), part of the browser's password
/// stretch salt. Public: the sign-in page needs it before anyone is signed in.
pub const SITE_ID_KEY: &str = "site_id";
/// Settings key holding the Turnstile site key (a JSON string); absent = Turnstile off.
pub const TURNSTILE_SITE_KEY: &str = "turnstile_site_key";

/// Settings key holding the site's public origin (a JSON string, `https://blog.example`):
/// the passkey relying party and the host Turnstile answers must name. `blog setup` writes it;
/// absent, the API falls back to the request's own origin and logs `settings.origin_missing`.
pub const ORIGIN_KEY: &str = "origin";

/// Settings that must never leave the Worker (API responses, theme data, exports).
pub const INTERNAL_SETTING_KEYS: &[&str] = &[SETUP_CODE_HASH_KEY, SETUP_CODE_EXPIRES_KEY];

/// Whether `key` is in [`INTERNAL_SETTING_KEYS`].
#[must_use]
pub fn is_internal_setting(key: &str) -> bool {
    INTERNAL_SETTING_KEYS.contains(&key)
}

/// `settings` without internal keys: the one filter every path that hands settings to a
/// client or a theme goes through.
#[must_use]
pub fn public_settings(settings: Vec<(String, String)>) -> Vec<(String, String)> {
    settings
        .into_iter()
        .filter(|(k, _)| !is_internal_setting(k))
        .collect()
}

/// `value_json` of [`SETUP_CODE_HASH_KEY`] for `code_hash`. Both stores compare this exact
/// text, so a hash matches only itself.
#[must_use]
pub fn setup_hash_json(code_hash: &str) -> String {
    format!("\"{code_hash}\"")
}

/// What [`LoginLimitStore::record_failure`] found or wrote.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recorded {
    /// The count after this attempt, or the stored one when it was not counted.
    pub count: i64,
    pub window_start: i64,
    /// Whether this attempt was counted (a write happened).
    pub counted: bool,
}

impl Recorded {
    /// Attempts before this one: what the rate-limit decision reads.
    #[must_use]
    pub fn before(&self) -> i64 {
        if self.counted {
            self.count.saturating_sub(1)
        } else {
            self.count
        }
    }
}

// Workers are single-threaded, so futures need not be `Send` (see plumblog_auth::Crypto).
#[allow(async_fn_in_trait)]
pub trait LoginLimitStore {
    /// Counts one attempt for `key_hash` in one atomic upsert. A window covers
    /// `[window_start, window_start + window_ms)`: an attempt at or after its end restarts it
    /// at count 1 with `window_start = now`; otherwise the count goes up by one, unless it
    /// already reached `cap` in the open window: then nothing is written (a locked key costs
    /// no write) and the row comes back as it is, with `counted: false`.
    async fn record_failure(
        &self,
        key_hash: &str,
        now: i64,
        window_ms: i64,
        cap: i64,
    ) -> Result<Recorded>;
    /// Deletes at most `limit` rows whose window ended (`window_start + window_ms <= now`)
    /// and returns how many went. Rows for random logins and credential ids would otherwise
    /// stay forever; the caller sweeps a bounded batch now and then.
    async fn delete_ended_failures(&self, now: i64, window_ms: i64, limit: u32) -> Result<u64>;
    /// `(count, window_start)` per key, in the order given, in one query. A missing key or
    /// one whose window ended (`window_start + window_ms <= now`) reads as `(0, 0)`.
    /// `InvalidInput` for more than [`MAX_FAILURE_KEYS`] keys. Never writes.
    async fn failures(
        &self,
        key_hashes: &[&str],
        now: i64,
        window_ms: i64,
    ) -> Result<Vec<(i64, i64)>>;
    /// Forgets `key_hash` (after a successful sign-in). Absent is Ok.
    async fn clear_failures(&self, key_hash: &str) -> Result<()>;
    /// Takes one back from `key_hash`'s count in one atomic write: an attempt counted up front
    /// that then succeeded. A count of 1 removes the row (rows hold at least one); absent is Ok;
    /// otherwise the window is left as it is.
    async fn forgive_failure(&self, key_hash: &str) -> Result<()>;
}

#[allow(async_fn_in_trait)]
pub trait ChallengeStore {
    /// `Conflict` if the hash exists; `InvalidInput` if `user` does not exist. A user's
    /// challenges go when the user is deleted.
    async fn put_challenge(
        &self,
        challenge_hash: &str,
        purpose: ChallengePurpose,
        user: Option<UserId>,
        expires_at: i64,
    ) -> Result<()>;
    /// Single use: atomically deletes the challenge with this hash and purpose and returns
    /// `Some(user)` if it was still valid (`expires_at > now`). An expired one is deleted too
    /// and gives `None`; one with another purpose is left alone and gives `None`; a second
    /// take of the same hash gives `None`.
    async fn take_challenge(
        &self,
        challenge_hash: &str,
        purpose: ChallengePurpose,
        now: i64,
    ) -> Result<Option<Option<UserId>>>;
    /// Deletes challenges with `expires_at <= now`; returns how many. Lazy cleanup: callers
    /// may run it before each put.
    async fn delete_expired_challenges(&self, now: i64) -> Result<u64>;
}

#[allow(async_fn_in_trait)]
pub trait SetupStore {
    /// Stores `value_json` under [`SITE_ID_KEY`] unless a value is already there, and returns
    /// the stored value, in one atomic write: two first visitors racing both get the same
    /// id, so a password stretched with it stays valid. Never overwrites. No version bump.
    async fn ensure_site_id(&self, value_json: &str, now: i64) -> Result<String>;
    /// Writes [`SETUP_CODE_HASH_KEY`] and [`SETUP_CODE_EXPIRES_KEY`] together, replacing any
    /// earlier code. No version bump (readers never see them).
    async fn put_setup_code(&self, code_hash: &str, expires_at: i64, now: i64) -> Result<()>;
    /// Creates the first user, always as owner (`user.role` is ignored), and deletes both
    /// setup keys, in one atomic write, only if the stored hash equals `code_hash` exactly,
    /// the code has not expired (`expires_at > now`) and no user exists. Otherwise
    /// `SetupInvalid` and nothing changes. Two setups racing: at most one succeeds.
    /// Comparing in constant time is the caller's job. No version bump.
    async fn create_first_owner(&self, code_hash: &str, user: NewUser, now: i64) -> Result<UserId>;
}
