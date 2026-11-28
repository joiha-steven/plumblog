//! Sign-in protection (`login_failures`, `auth_challenges`) and first-owner setup.
//!
//! The store only ever sees hashes: the caller hashes logins, IPs, challenges and setup
//! codes before calling, so a database dump reveals none of them.

use plumblog_model::{Result, UserId, stable_codes};

use crate::{NewSession, NewUser, ProfileUpdate};

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

/// Settings keys of the owner reset code `blog admin reset` stores: its hash (a JSON string,
/// as [`setup_hash_json`]), the owner it is bound to (JSON integer id) and when it expires
/// (ms, JSON integer). Internal, like the setup code.
pub const OWNER_RESET_HASH_KEY: &str = "owner_reset_hash";
pub const OWNER_RESET_USER_KEY: &str = "owner_reset_user";
pub const OWNER_RESET_EXPIRES_KEY: &str = "owner_reset_expires_at";
/// When the reset ran (ms, JSON integer) and whether it kept the owner's API tokens (JSON
/// boolean): the redeem revokes tokens and owner invites created since, which a request in
/// flight across the reset may have left. Absent (a code from an older `blog`): the redeem
/// revokes every token and owner invite of the user.
pub const OWNER_RESET_AT_KEY: &str = "owner_reset_at";
/// How far before the recorded reset time the redeem still treats tokens and owner invites as
/// made since the reset: a request that began before the reset stamps `created_at` with its own
/// start, and the CLI's clock may run ahead of the Worker's, so a strict comparison could keep a
/// token an attacker minted during the reset.
pub const OWNER_RESET_SINCE_MARGIN_MS: i64 = 10 * 60 * 1000;
pub const OWNER_RESET_KEEP_TOKENS_KEY: &str = "owner_reset_keep_tokens";
/// Every owner reset key, deleted together when the code is taken.
pub const OWNER_RESET_KEYS: &[&str] = &[
    OWNER_RESET_HASH_KEY,
    OWNER_RESET_USER_KEY,
    OWNER_RESET_EXPIRES_KEY,
    OWNER_RESET_AT_KEY,
    OWNER_RESET_KEEP_TOKENS_KEY,
];

/// Settings key holding the site's random id (a JSON string), part of the browser's password
/// stretch salt. Public: the sign-in page needs it before anyone is signed in.
pub const SITE_ID_KEY: &str = "site_id";
/// Settings key `blog backup` writes (JSON `{at, database, media, complete}`): when the data
/// was last copied off Cloudflare. Internal: never handed to the API or themes, and not
/// writable through it; only `blog` says what it backed up.
pub const LAST_BACKUP_KEY: &str = "last_backup";
/// Settings key holding the Turnstile site key (a JSON string); absent = Turnstile off.
pub const TURNSTILE_SITE_KEY: &str = "turnstile_site_key";

/// Settings key holding the site's public origin (a JSON string, `https://blog.example`):
/// the passkey relying party and the host Turnstile answers must name. `blog setup` writes it;
/// absent, the API falls back to the request's own origin and logs `settings.origin_missing`.
pub const ORIGIN_KEY: &str = "origin";

/// Settings key holding the site's default language (a JSON string, `vi`): the language of the
/// reader site and the auth pages. `blog setup` writes it once, never over an existing value.
/// The reader's own constant (`SITE_LANG_KEY`) must name the same key; a worker test checks it.
pub const SITE_LANG_KEY: &str = "site_lang";

/// Settings that must never leave the Worker (API responses, theme data, exports).
pub const INTERNAL_SETTING_KEYS: &[&str] = &[
    SETUP_CODE_HASH_KEY,
    SETUP_CODE_EXPIRES_KEY,
    LAST_BACKUP_KEY,
    OWNER_RESET_HASH_KEY,
    OWNER_RESET_USER_KEY,
    OWNER_RESET_EXPIRES_KEY,
    OWNER_RESET_AT_KEY,
    OWNER_RESET_KEEP_TOKENS_KEY,
];

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
    /// Counts one attempt on a shared key (the site-wide limit) in one statement, without
    /// reading the row back: an ended window restarts at 1 with `window_start = now`; an open
    /// one goes up by one while below `cap`. Returns the count after this attempt, or `None`
    /// when the key is at `cap` in its open window: then the statement writes no row.
    async fn count_capped(
        &self,
        key_hash: &str,
        now: i64,
        window_ms: i64,
        cap: i64,
    ) -> Result<Option<i64>>;
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
    /// Whether a setup code could still create the first owner as far as users go: true only
    /// while no user row exists (disabled or demoted ones count, exactly as in
    /// [`Self::create_first_owner`]). One read, no code involved; never writes.
    async fn setup_open(&self) -> Result<bool>;
    /// Writes [`SETUP_CODE_HASH_KEY`] and [`SETUP_CODE_EXPIRES_KEY`] together, replacing any
    /// earlier code. No version bump (readers never see them).
    async fn put_setup_code(&self, code_hash: &str, expires_at: i64, now: i64) -> Result<()>;
    /// Creates the first user, always as owner (`user.role` is ignored), and deletes both
    /// setup keys, in one atomic write, only if the stored hash equals `code_hash` exactly,
    /// the code has not expired (`expires_at > now`) and no user exists. Otherwise
    /// `SetupInvalid` and nothing changes. Two setups racing: at most one succeeds.
    /// Comparing in constant time is the caller's job. No version bump.
    async fn create_first_owner(&self, code_hash: &str, user: NewUser, now: i64) -> Result<UserId>;
    /// Writes the owner reset code (`blog admin reset` writes the same rows over D1's REST
    /// API): hash first, then user, expiry, reset time (`now`) and `keep_tokens`, replacing
    /// any earlier code. No version bump.
    async fn put_owner_reset(
        &self,
        code_hash: &str,
        user: UserId,
        expires_at: i64,
        keep_tokens: bool,
        now: i64,
    ) -> Result<()>;
    /// Single use: if the stored hash equals `code_hash` exactly, deletes every reset key
    /// ([`OWNER_RESET_KEYS`]) in one atomic write and returns the bound user when the code
    /// has not expired (`expires_at > now`, both values exactly JSON integers). An expired
    /// code is deleted too and gives `None`; a wrong code changes nothing and gives `None`; of
    /// two takes racing, at most one gets the user. Comparing in constant time is the caller's
    /// job.
    async fn take_owner_reset(&self, code_hash: &str, now: i64) -> Result<Option<UserId>>;
    /// Redeems the owner reset code in one atomic write, only if `session.proof` is
    /// [`crate::Proof::ResetCode`] and holds for `id` (else `SetupInvalid`) and `id` is still
    /// an active owner (else `SetupInvalid`; [`Self::take_owner_reset`] then spends the code).
    /// Sets `password_hash = hash` and `profile` if given (bumping the content version),
    /// deletes every session and passkey of `id`, revokes its API tokens (with
    /// [`OWNER_RESET_KEEP_TOKENS_KEY`] true, only those created at or after
    /// [`OWNER_RESET_AT_KEY`]), deletes its unused invites (any role) created since, deletes the
    /// reset keys and creates `session`. Returns the new session id. `Conflict` on a login or
    /// slug another user holds; then nothing changed and the code still works.
    async fn redeem_owner_reset(
        &self,
        id: UserId,
        hash: String,
        profile: Option<ProfileUpdate>,
        session: NewSession,
        now: i64,
    ) -> Result<i64>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_backup_record_is_internal_and_the_site_id_is_not() {
        let all = vec![
            (LAST_BACKUP_KEY.to_string(), "{}".to_string()),
            (SETUP_CODE_HASH_KEY.to_string(), "\"h\"".to_string()),
            (SITE_ID_KEY.to_string(), "\"s\"".to_string()),
            ("title".to_string(), "\"t\"".to_string()),
        ];
        let shown: Vec<String> = public_settings(all).into_iter().map(|(k, _)| k).collect();
        assert_eq!(shown, [SITE_ID_KEY, "title"]);
        assert!(is_internal_setting(LAST_BACKUP_KEY));
        for &k in OWNER_RESET_KEYS {
            assert!(is_internal_setting(k), "{k}");
            let shown = public_settings(vec![(k.to_string(), "1".to_string())]);
            assert!(shown.is_empty(), "{k}");
        }
    }
}
