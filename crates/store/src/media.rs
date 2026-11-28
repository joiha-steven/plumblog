//! The media catalogue (`media`). Bytes live in R2.

use plumblog_model::{MediaId, PostId, Result, UserId};

/// A row of `media`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Media {
    pub id: MediaId,
    pub r2_key: String,
    pub mime: String,
    pub bytes: i64,
    pub width: i64,
    pub height: i64,
    /// `""` = missing description.
    pub alt: String,
    /// JSON the store never parses.
    pub variants_json: String,
    /// `None` once the uploader is deleted.
    pub uploaded_by: Option<UserId>,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewMedia {
    pub r2_key: String,
    pub mime: String,
    /// `>= 0`, else `InvalidInput`.
    pub bytes: i64,
    /// `> 0`, else `InvalidInput`.
    pub width: i64,
    /// `> 0`, else `InvalidInput`.
    pub height: i64,
    pub alt: String,
    pub variants_json: String,
    pub uploaded_by: Option<UserId>,
    pub created_at: i64,
}

/// One page of the media library, newest first (`created_at` DESC, id DESC).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaQuery {
    /// Keyset cursor: `(created_at, id)` of the last item of the previous page; the page
    /// starts strictly after it.
    pub before: Option<(i64, MediaId)>,
    /// Only items with `alt == ""`.
    pub missing_alt_only: bool,
    /// Only items no post uses, in its body or as cover (see `MediaStore::media_usage`).
    pub unused_only: bool,
    /// Capped at [`crate::MAX_PAGE`] + 1: a page plus the probe row for "is there more".
    pub limit: u32,
}

// Workers are single-threaded, so futures need not be `Send` (see plumblog_auth::Crypto).
#[allow(async_fn_in_trait)]
pub trait MediaStore {
    /// `Conflict` on a duplicate `r2_key`; `InvalidInput` on bad sizes or a missing uploader.
    async fn create_media(&self, media: NewMedia) -> Result<MediaId>;
    /// `NotFound`.
    async fn get_media(&self, id: MediaId) -> Result<Media>;
    /// The item when a reader can see it: a `published` post or page uses it in its body
    /// (`SavePost::media_ids`) or as cover. `None` for unknown or unused media and for media
    /// used only by drafts, scheduled or trashed posts, alike, so a caller cannot tell them
    /// apart. "Published" is the reader's own test (status alone). One read.
    async fn public_media(&self, id: MediaId) -> Result<Option<Media>>;
    async fn list_media(&self, query: MediaQuery) -> Result<Vec<Media>>;
    /// `NotFound`. Bumps the content version (alt text shows on reader pages).
    async fn set_media_alt(&self, id: MediaId, alt: &str) -> Result<()>;
    /// Posts that use the item in their body (`SavePost::media_ids`) or as cover, any
    /// status including trashed; ascending, each once. Empty for unknown media.
    async fn media_usage(&self, id: MediaId) -> Result<Vec<PostId>>;
    /// `NotFound`. `Conflict` while any post uses it (see `media_usage`); the detail lists
    /// those post ids and nothing is deleted. Never bumps the content version: unused media
    /// is on no reader page.
    async fn delete_media(&self, id: MediaId) -> Result<()>;
}
