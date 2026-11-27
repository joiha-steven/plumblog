//! What reader pages read. Shaped as queries so a D1 implementation fetches only one page of
//! rows. The Worker serves `db::Db` (the store, `reader_db`); `SampleSource` serves the embedded sample
//! posts to unit tests of rendering and to nothing else.

use plumblog_markdown::Rendered;
use plumblog_model::{PostId, Result, TagId};

#[derive(Clone, Debug)]
pub struct StoredPost {
    /// 0 for the embedded samples.
    pub id: PostId,
    pub slug: String,
    pub title: String,
    pub lang: String,
    pub published_at: i64,
    pub pinned: bool,
    /// A static page (kind `page`): no comments, no neighbours.
    pub is_page: bool,
    /// The post's own comment switch; the site switch is read separately.
    pub comments_enabled: bool,
    pub excerpt: Option<String>,
    pub body: Rendered,
    pub tags: Vec<Tag>,
    /// Other published language versions: `(lang, slug)`.
    pub translations: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tag {
    /// 0 for the embedded samples.
    pub id: TagId,
    pub slug: String,
    pub name: String,
}

#[derive(Clone, Debug)]
pub struct Settings {
    pub title: String,
    pub description: String,
    pub footer: String,
    pub lang: String,
    pub per_page: usize,
    /// Languages the site offers, the default first. Stored and parsed for the language
    /// switcher (a theme feature, not built yet); no route is gated on it.
    #[allow(dead_code, reason = "read by the language switcher, not built yet")]
    pub languages: Vec<String>,
    /// Public origin (`https://example.com`) for canonical URLs, feeds and the sitemap.
    /// Production must set it: otherwise the request's Host header decides public URLs.
    pub origin: Option<String>,
    /// Feed and sitemap date for a site with no posts. The store has no site clock, so a
    /// stored site leaves it 0 and feeds use their newest post instead.
    pub updated_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter<'a> {
    All,
    Tag(&'a Tag),
    Year(i32),
    /// One language, newest first (feeds).
    Lang(&'a str),
}

/// One page of posts and how many the filter matches in all.
pub struct Slice {
    pub posts: Vec<StoredPost>,
    pub total: usize,
}

/// One sitemap URL: a published post or page.
pub struct SitemapPost {
    pub lang: String,
    pub slug: String,
    /// Unix seconds.
    pub updated_at: i64,
    /// Posts sharing it are translations of each other.
    pub group: i64,
}

/// A post with its chronological neighbours.
pub struct Neighbours {
    pub post: StoredPost,
    pub older: Option<StoredPost>,
    pub newer: Option<StoredPost>,
}

// Workers are single-threaded, so futures need not be `Send` (see auth::Crypto).
#[allow(async_fn_in_trait)]
/// The content version comes from the store (`db::ExtSource`), not from here.
pub trait ReaderSource {
    async fn settings(&self) -> Result<Settings>;
    /// Published posts, pinned first, then newest first.
    async fn posts(&self, filter: Filter<'_>, offset: usize, limit: usize) -> Result<Slice>;
    /// The published post at `/<lang>/<slug>` with its neighbours and translations.
    async fn post(&self, lang: &str, slug: &str) -> Result<Option<Neighbours>>;
    /// The newest `limit` posts in one language, with bodies (feeds).
    async fn feed(&self, lang: &str, limit: usize) -> Result<Vec<StoredPost>>;
    /// Newest `limit` published posts and pages, for the sitemap.
    async fn sitemap_posts(&self, limit: usize) -> Result<Vec<SitemapPost>>;
    async fn tag(&self, slug: &str) -> Result<Option<Tag>>;
    /// Every tag with at least one published post (sitemap).
    async fn tags(&self) -> Result<Vec<Tag>>;
    /// Years that have posts, newest first.
    async fn years(&self) -> Result<Vec<i32>>;
}

include!(concat!(env!("OUT_DIR"), "/embedded.rs"));
