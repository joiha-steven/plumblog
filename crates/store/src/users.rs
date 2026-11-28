//! Site counters and user accounts (`site`, `users`).

use plumblog_model::{Result, Role, UserId};

use crate::NewSession;

/// A row of `users`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    pub id: UserId,
    /// Unique ignoring ASCII case (SQLite `NOCASE`).
    pub login: String,
    pub slug: String,
    pub display_name: String,
    pub bio: String,
    pub role: Role,
    /// `None` for passkey-only accounts.
    pub password_hash: Option<String>,
    /// `None` = active.
    pub disabled_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A user to insert: active, `updated_at = created_at`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewUser {
    pub login: String,
    pub slug: String,
    pub display_name: String,
    pub bio: String,
    pub role: Role,
    pub password_hash: Option<String>,
    pub created_at: i64,
}

/// The reader-visible fields of a user, written by [`UserStore::update_profile`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProfileUpdate {
    pub login: String,
    pub slug: String,
    pub display_name: String,
    pub bio: String,
}

// Workers are single-threaded, so futures need not be `Send` (see plumblog_auth::Crypto).
#[allow(async_fn_in_trait)]
pub trait SiteStore {
    /// Part of every page cache key; starts at 1.
    async fn content_version(&self) -> Result<i64>;
}

#[allow(async_fn_in_trait)]
pub trait UserStore {
    /// `Conflict` if the login (ASCII case-insensitive) or slug is taken.
    async fn create_user(&self, user: NewUser) -> Result<UserId>;
    /// `UserNotFound` if absent.
    async fn get_user(&self, id: UserId) -> Result<User>;
    /// ASCII case-insensitive, like SQLite `NOCASE`.
    async fn find_user_by_login(&self, login: &str) -> Result<Option<User>>;
    async fn find_user_by_slug(&self, slug: &str) -> Result<Option<User>>;
    /// Ordered by id.
    async fn list_users(&self) -> Result<Vec<User>>;
    /// By id ascending, strictly after `after`: at most `limit` (capped at
    /// [`crate::MAX_PAGE`]) users, and whether more follow.
    async fn users_by_id(&self, after: Option<UserId>, limit: u32) -> Result<(Vec<User>, bool)>;
    // Each write below touches only its own columns plus `updated_at = now`, so two admins
    // changing different things (or a profile edit racing a password change) never undo
    // each other. An "active owner" is `role = Owner` with `disabled_at = None`; a write
    // that would leave none fails with `Conflict`, atomically even under concurrent writes.

    /// Writes login, slug, display name and bio. `UserNotFound`; `Conflict` on a login
    /// (ASCII case-insensitive) or slug held by another user. Bumps the content version
    /// (author pages show these).
    async fn update_profile(&self, id: UserId, profile: ProfileUpdate, now: i64) -> Result<()>;
    /// `None` makes the account passkey-only. `UserNotFound`. No version bump.
    async fn set_password(&self, id: UserId, hash: Option<String>, now: i64) -> Result<()>;
    /// A password change, and with `profile` a login change (the browser's stretch salt
    /// holds the login, so the two must land together or the user is locked out). In one
    /// atomic write: sets `password_hash = hash`, writes `profile` if given (bumping the
    /// content version), deletes every session of the user and creates `session` (the
    /// caller's rotated one). Returns the new session id. `hash = None` is a passkey-only
    /// account's login change: the account must have no password at the time of the write
    /// (else `Conflict`: a password stretched with the old login would stop working), and
    /// keeps none. `UserNotFound`; `Conflict` on a login or slug held by another user, or a
    /// duplicate session hash; `InvalidInput` if `session.user_id` is not `id`. On error
    /// nothing changed.
    async fn change_password(
        &self,
        id: UserId,
        hash: Option<String>,
        profile: Option<ProfileUpdate>,
        session: NewSession,
        now: i64,
    ) -> Result<i64>;
    /// `UserNotFound`; `Conflict` when demoting the last active owner. No version bump.
    async fn set_role(&self, id: UserId, role: Role, now: i64) -> Result<()>;
    /// `Some(at)` disables the account and, in the same atomic write, deletes all its
    /// sessions (a disabled account must not stay signed in); `None` re-enables it.
    /// `UserNotFound`; `Conflict` when disabling the last active owner. No version bump.
    async fn set_disabled(&self, id: UserId, disabled_at: Option<i64>, now: i64) -> Result<()>;
    /// `UserNotFound`; `Conflict` while they author any post (reassign first) or when they
    /// are the last active owner. Deletes their
    /// sessions, passkeys, tokens and the invites they created; clears `invites.used_by`,
    /// `media.uploaded_by`, `revisions.author_id` and the log actor that pointed at them.
    async fn delete_user(&self, id: UserId) -> Result<()>;
}
