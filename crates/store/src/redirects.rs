//! Old URL -> new URL (`redirects`).

use plumblog_model::Result;

/// A row of `redirects`. Paths start with `/`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Redirect {
    pub from_path: String,
    pub to_path: String,
    pub created_at: i64,
}

// Workers are single-threaded, so futures need not be `Send` (see plumblog_auth::Crypto).
#[allow(async_fn_in_trait)]
pub trait RedirectStore {
    /// The target of `path`, if redirected.
    async fn find_redirect(&self, path: &str) -> Result<Option<String>>;
    /// `InvalidInput` if `from == to`. Upserts `from -> to` (`created_at = now`), rewrites
    /// every redirect that pointed to `from` to point to `to` (no chains) and deletes the
    /// redirect from `to`, if any (no loops). One atomic write.
    async fn put_redirect(&self, from: &str, to: &str, now: i64) -> Result<()>;
    /// Absent path is Ok.
    async fn delete_redirect(&self, from: &str) -> Result<()>;
    /// Ordered by `from_path`.
    async fn list_redirects(&self) -> Result<Vec<Redirect>>;
}
