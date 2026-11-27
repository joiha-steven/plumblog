//! Site counters and user accounts (`site`, `users`).

use plumblog_model::{Result, Role, UserId};

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
    /// Writes every field except `id` and `created_at`. `UserNotFound`; `Conflict` on a
    /// login or slug held by another user.
    async fn update_user(&self, user: &User) -> Result<()>;
    /// `UserNotFound`; `Conflict` while they author any post (reassign first). Deletes their
    /// sessions, passkeys, tokens and the invites they created; clears `invites.used_by`,
    /// `media.uploaded_by`, `revisions.author_id` and the log actor that pointed at them.
    async fn delete_user(&self, id: UserId) -> Result<()>;
}
