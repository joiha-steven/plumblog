//! Sign-in records: sessions, passkeys, API tokens and invites. Only hashes of secrets.

use plumblog_model::{Result, Role, UserId};

use crate::NewUser;

/// A row of `sessions`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    pub id: i64,
    pub token_hash: String,
    pub user_id: UserId,
    pub created_at: i64,
    pub expires_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewSession {
    pub token_hash: String,
    pub user_id: UserId,
    pub created_at: i64,
    pub expires_at: i64,
}

/// A row of `passkeys`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Passkey {
    pub id: i64,
    pub user_id: UserId,
    pub credential_id: String,
    pub public_key: String,
    pub sign_count: i64,
    pub name: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewPasskey {
    pub user_id: UserId,
    pub credential_id: String,
    pub public_key: String,
    pub sign_count: i64,
    pub name: String,
    pub created_at: i64,
}

/// A row of `api_tokens`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiToken {
    pub id: i64,
    pub user_id: UserId,
    pub name: String,
    pub token_hash: String,
    pub scope: Role,
    pub created_at: i64,
    /// `None` = never expires.
    pub expires_at: Option<i64>,
    /// `None` = active.
    pub revoked_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewToken {
    pub user_id: UserId,
    pub name: String,
    pub token_hash: String,
    pub scope: Role,
    pub created_at: i64,
    pub expires_at: Option<i64>,
}

/// A row of `invites`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invite {
    pub id: i64,
    pub code_hash: String,
    pub role: Role,
    pub created_by: UserId,
    pub created_at: i64,
    pub expires_at: i64,
    pub used_at: Option<i64>,
    /// `None` while unused, or once the user who accepted it is deleted.
    pub used_by: Option<UserId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewInvite {
    pub code_hash: String,
    pub role: Role,
    pub created_by: UserId,
    pub created_at: i64,
    pub expires_at: i64,
}

// Workers are single-threaded, so futures need not be `Send` (see plumblog_auth::Crypto).
#[allow(async_fn_in_trait)]
pub trait SessionStore {
    /// `Conflict` on a duplicate hash; `InvalidInput` if the user does not exist.
    async fn create_session(&self, session: NewSession) -> Result<i64>;
    /// `None` if absent, expired (`expires_at <= now`) or its user is disabled.
    async fn find_session(&self, token_hash: &str, now: i64) -> Result<Option<Session>>;
    /// Sets `expires_at` of a session that is still valid at `now` (one write, same token).
    /// Returns whether one was renewed: an absent or already expired session is not revived.
    async fn renew_session(&self, token_hash: &str, expires_at: i64, now: i64) -> Result<bool>;
    /// Absent hash is Ok (signing out twice is harmless).
    async fn delete_session(&self, token_hash: &str) -> Result<()>;
    /// Returns how many were deleted.
    async fn delete_user_sessions(&self, user: UserId) -> Result<u64>;
    /// Deletes sessions with `expires_at <= now`; returns how many.
    async fn delete_expired_sessions(&self, now: i64) -> Result<u64>;
}

#[allow(async_fn_in_trait)]
pub trait PasskeyStore {
    /// `Conflict` on a duplicate credential id; `InvalidInput` if the user does not exist.
    async fn add_passkey(&self, passkey: NewPasskey) -> Result<i64>;
    async fn find_passkey(&self, credential_id: &str) -> Result<Option<Passkey>>;
    /// Ordered by id.
    async fn list_passkeys(&self, user: UserId) -> Result<Vec<Passkey>>;
    /// Returns whether the counter is acceptable: `sign_count` above the stored one, or both
    /// 0 (authenticators without a counter always send 0). If so, records the sign-in:
    /// `sign_count` and `last_used_at = at`. If not, writes nothing (the counter never goes
    /// back); `false` hints at a cloned authenticator and the caller must refuse. `NotFound`.
    async fn touch_passkey(&self, id: i64, sign_count: i64, at: i64) -> Result<bool>;
    /// `NotFound`.
    async fn delete_passkey(&self, id: i64) -> Result<()>;
}

#[allow(async_fn_in_trait)]
pub trait TokenStore {
    /// `Conflict` on a duplicate hash; `InvalidInput` if the user does not exist.
    async fn create_token(&self, token: NewToken) -> Result<i64>;
    /// `None` if absent, revoked, expired (`expires_at <= now`) or its user is disabled.
    /// Disabling does not revoke: re-enabling the user makes their unrevoked, unexpired
    /// tokens resolve again.
    async fn find_token(&self, token_hash: &str, now: i64) -> Result<Option<ApiToken>>;
    /// All of a user's tokens, revoked and expired included, ordered by id.
    async fn list_tokens(&self, user: UserId) -> Result<Vec<ApiToken>>;
    /// `NotFound`. Revoking twice keeps the first time.
    async fn revoke_token(&self, id: i64, at: i64) -> Result<()>;
}

#[allow(async_fn_in_trait)]
pub trait InviteStore {
    /// `Conflict` on a duplicate hash; `InvalidInput` if the creator does not exist.
    async fn create_invite(&self, invite: NewInvite) -> Result<i64>;
    /// Newest first (`created_at` DESC, id DESC), used ones included.
    async fn list_invites(&self) -> Result<Vec<Invite>>;
    /// `NotFound`.
    async fn delete_invite(&self, id: i64) -> Result<()>;
    /// One atomic write: the invite must exist, be unused and have `expires_at > now`, else
    /// `InviteInvalid`. Creates the user with the invite's role (`user.role` is ignored) and
    /// marks the invite `used_at = now`, `used_by = <new id>`. A login or slug `Conflict`
    /// leaves the invite unused.
    async fn accept_invite(&self, code_hash: &str, user: NewUser, now: i64) -> Result<UserId>;
}
