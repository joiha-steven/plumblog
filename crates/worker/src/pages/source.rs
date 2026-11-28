//! What reader pages read. Shaped as queries so a D1 implementation fetches only one page of
//! rows. The Worker serves `db::Db` (the store, `reader_db`); `SampleSource` serves the embedded sample
//! posts to unit tests of rendering and to nothing else.

use plumblog_markdown::Rendered;
use plumblog_model::{PostId, Result, SeriesId, TagId};

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
    /// The post page only; sized for a social card (see `pages/cover.rs`).
    pub cover: Option<plumblog_render::Image>,
    pub body: Rendered,
    pub tags: Vec<Tag>,
    /// Other published language versions: `(lang, slug)`.
    pub translations: Vec<(String, String)>,
    /// The series it belongs to; read for the post page only.
    pub series: Option<PostSeries>,
}

/// A post's series as its page shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostSeries {
    pub slug: String,
    pub title: String,
    pub position: Option<i64>,
}

/// A series page's subject.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeriesInfo {
    pub id: SeriesId,
    pub slug: String,
    pub title: String,
    pub description: String,
}

/// A tag or series page that exists in one language (sitemap).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaxonPage {
    pub slug: String,
    pub name: String,
    pub lang: String,
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
    /// Whether `lang` was stored (and valid) rather than the default; the admin's sign-in
    /// pages trust it over the browser's language only then.
    pub lang_set: bool,
    pub per_page: usize,
    /// Languages the site offers, the default first: `/<lang>/` is a home only for these.
    pub languages: Vec<String>,
    /// Public origin (`https://example.com`) for canonical URLs, feeds and the sitemap.
    /// Production must set it: otherwise the request's Host header decides public URLs.
    pub origin: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter<'a> {
    /// One language, pinned first, then by date: the home page.
    Home(&'a str),
    /// One language's posts with the tag, newest first.
    Tag(&'a Tag, &'a str),
    /// One language's posts of the series in reading order.
    Series(&'a SeriesInfo, &'a str),
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
    /// A static page, not a post: it makes no language home.
    pub is_page: bool,
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
    /// Unix seconds the site last changed (its newest setting write), for a feed with no
    /// posts; `None` when it has no settings at all. 1 query.
    async fn settings_updated(&self) -> Result<Option<i64>>;
    /// Published posts, pinned first, then newest first.
    async fn posts(&self, filter: Filter<'_>, offset: usize, limit: usize) -> Result<Slice>;
    /// The published post at `/<lang>/<slug>` with its neighbours and translations.
    async fn post(&self, lang: &str, slug: &str) -> Result<Option<Neighbours>>;
    /// The newest `limit` posts in one language, with bodies (feeds).
    async fn feed(&self, lang: &str, limit: usize) -> Result<Vec<StoredPost>>;
    /// Newest `limit` published posts and pages, for the sitemap.
    async fn sitemap_posts(&self, limit: usize) -> Result<Vec<SitemapPost>>;
    async fn tag(&self, slug: &str) -> Result<Option<Tag>>;
    async fn series(&self, slug: &str) -> Result<Option<SeriesInfo>>;
    /// Every (tag, language) among `langs` with a published post: the tag pages that exist
    /// (sitemap), by tag name. 1 query.
    async fn tag_pages(&self, langs: &[String]) -> Result<Vec<TaxonPage>>;
    /// As [`ReaderSource::tag_pages`] for series (`name` is the title). 1 query.
    async fn series_pages(&self, langs: &[String]) -> Result<Vec<TaxonPage>>;
    /// Which of `langs` have a published post with the tag, in order. 1 query.
    async fn tag_langs(&self, tag: &Tag, langs: &[String]) -> Result<Vec<String>>;
    /// Which of `langs` have a published post in the series, in order. 1 query.
    async fn series_langs(&self, series: &SeriesInfo, langs: &[String]) -> Result<Vec<String>>;
    /// Years that have posts, newest first.
    async fn years(&self) -> Result<Vec<i32>>;
    /// Whether a published post or page is at `/<lang>/<slug>`. 1 query.
    async fn post_exists(&self, lang: &str, slug: &str) -> Result<bool>;
    /// Which of `langs` have a published post, in order. 1 query (0 for none).
    async fn langs_with_posts(&self, langs: &[String]) -> Result<Vec<String>>;
}

include!(concat!(env!("OUT_DIR"), "/embedded.rs"));
