//! Tags and series (`tags`, `series`, `post_tags`).

use plumblog_model::{Result, SeriesId, TagId};

/// A row of `tags`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tag {
    pub id: TagId,
    pub slug: String,
    pub name: String,
}

/// A row of `series`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Series {
    pub id: SeriesId,
    pub slug: String,
    pub title: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewSeries {
    pub slug: String,
    pub title: String,
    pub description: String,
}

// Workers are single-threaded, so futures need not be `Send` (see plumblog_auth::Crypto).
#[allow(async_fn_in_trait)]
pub trait TaxonomyStore {
    /// The id of the tag with `slug`, creating it with `name` if absent. An existing tag
    /// keeps its name.
    async fn ensure_tag(&self, slug: &str, name: &str) -> Result<TagId>;
    /// `NotFound`; `Conflict` if another tag has the slug. Bumps the content version if a
    /// published post has the tag.
    async fn update_tag(&self, tag: &Tag) -> Result<()>;
    async fn find_tag(&self, slug: &str) -> Result<Option<Tag>>;
    /// Ordered by name, then id.
    async fn list_tags(&self) -> Result<Vec<Tag>>;
    /// `NotFound`. 1 query.
    async fn get_tag(&self, id: TagId) -> Result<Tag>;
    /// Tags ordered by slug (unique, so a total order), starting strictly after `after`;
    /// at most `limit` (capped at [`crate::MAX_PAGE`]) and whether more follow. The store
    /// looks one row past the page itself, so the cap never hides the answer.
    async fn tags_page(&self, after: Option<&str>, limit: u32) -> Result<(Vec<Tag>, bool)>;
    /// `NotFound`. Removes the tag from every post and bumps their `version` (an editor
    /// holding the old version must reload); bumps the content version if any of those
    /// posts is published.
    async fn delete_tag(&self, id: TagId) -> Result<()>;

    /// `Conflict` on a duplicate slug.
    async fn create_series(&self, series: NewSeries) -> Result<SeriesId>;
    /// `NotFound`; `Conflict` if another series has the slug. Bumps the content version if
    /// a published post is in the series.
    async fn update_series(&self, series: &Series) -> Result<()>;
    async fn find_series(&self, slug: &str) -> Result<Option<Series>>;
    /// Ordered by title, then id.
    async fn list_series(&self) -> Result<Vec<Series>>;
    /// `NotFound`. 1 query.
    async fn get_series(&self, id: SeriesId) -> Result<Series>;
    /// As [`TaxonomyStore::tags_page`], for series.
    async fn series_page(&self, after: Option<&str>, limit: u32) -> Result<(Vec<Series>, bool)>;
    /// `NotFound`. Its posts get `series_id` and `series_position` = `None` and their
    /// `version` + 1 (an editor holding the old version must reload); bumps the content
    /// version if any of them is published.
    async fn delete_series(&self, id: SeriesId) -> Result<()>;
}
