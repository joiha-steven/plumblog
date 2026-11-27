//! Reader comments (`comments`).

use plumblog_model::{CommentId, PostId, Result, stable_codes};

stable_codes! {
    /// Stored in `comments.status`.
    pub enum CommentStatus {
        Pending => "pending",
        Approved => "approved",
        Spam => "spam",
        Trashed => "trashed",
    }
}

/// A row of `comments`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comment {
    pub id: CommentId,
    pub post_id: PostId,
    pub parent_id: Option<CommentId>,
    pub author_name: String,
    pub body: String,
    pub status: CommentStatus,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewComment {
    pub post_id: PostId,
    /// Must be a comment of the same post.
    pub parent_id: Option<CommentId>,
    pub author_name: String,
    pub body: String,
    pub status: CommentStatus,
    pub created_at: i64,
}

// Workers are single-threaded, so futures need not be `Send` (see plumblog_auth::Crypto).
#[allow(async_fn_in_trait)]
pub trait CommentStore {
    /// `InvalidInput` if the post is missing, or the parent is missing or on another post.
    /// Bumps the content version if the status is approved.
    async fn add_comment(&self, comment: NewComment) -> Result<CommentId>;
    /// Oldest first (`created_at`, id).
    async fn list_comments(&self, post: PostId, status: CommentStatus) -> Result<Vec<Comment>>;
    /// Moderation queue, oldest first (`created_at`, id). `after` is the `(created_at, id)`
    /// of the last comment of the previous page; the page starts strictly after it.
    async fn list_comments_by_status(
        &self,
        status: CommentStatus,
        after: Option<(i64, CommentId)>,
        limit: u32,
    ) -> Result<Vec<Comment>>;
    /// `NotFound`. Bumps if the old or new status is approved.
    async fn set_comment_status(&self, id: CommentId, status: CommentStatus) -> Result<()>;
    /// `NotFound`. Replies (at any depth) go too; bumps if any deleted comment was approved.
    async fn delete_comment(&self, id: CommentId) -> Result<()>;
}
