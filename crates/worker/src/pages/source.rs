//! What reader pages read. Shaped as queries so a D1 implementation fetches only one page of
//! rows; until the storage issue (#6) lands, `SampleSource` serves the embedded sample posts.
//! When `store` grows post listing, its traits replace this one and only this file changes.

use plumblog_markdown::{Options, Rendered};
use plumblog_model::Result;
use std::sync::OnceLock;

#[derive(Clone, Debug)]
pub struct StoredPost {
    pub slug: String,
    pub title: String,
    pub lang: String,
    pub published_at: i64,
    pub pinned: bool,
    pub excerpt: Option<String>,
    pub body: Rendered,
    pub tags: Vec<Tag>,
    /// Other language versions: `(lang, slug)`. Filled by the translations issue.
    pub translations: Vec<(String, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tag {
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
    /// Public origin (`https://example.com`) for canonical URLs, feeds and the sitemap.
    /// Production must set it: otherwise the request's Host header decides public URLs.
    pub origin: Option<String>,
    /// Unix time of the last publish or settings change, at least the site's creation (feed
    /// date when there are no posts).
    pub updated_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter<'a> {
    All,
    Tag(&'a str),
    Year(i32),
    /// One language, newest first (feeds).
    Lang(&'a str),
}

/// One page of posts and whether more follow.
pub struct Slice {
    pub posts: Vec<StoredPost>,
    pub more: bool,
}

/// A post with its chronological neighbours.
pub struct Neighbours {
    pub post: StoredPost,
    pub older: Option<StoredPost>,
    pub newer: Option<StoredPost>,
}

// Workers are single-threaded, so futures need not be `Send` (see auth::Crypto).
#[allow(async_fn_in_trait)]
pub trait ReaderSource {
    /// Bumped by every publish, edit, delete and settings change; part of every cache key, so
    /// stale pages are never served and nothing needs purging.
    async fn content_version(&self) -> Result<u64>;
    async fn settings(&self) -> Result<Settings>;
    /// Published posts, pinned first, then newest first.
    async fn posts(&self, filter: Filter<'_>, offset: usize, limit: usize) -> Result<Slice>;
    async fn post(&self, slug: &str) -> Result<Option<Neighbours>>;
    async fn tag(&self, slug: &str) -> Result<Option<Tag>>;
    /// Every tag with at least one published post (sitemap).
    async fn tags(&self) -> Result<Vec<Tag>>;
    /// Years that have posts, newest first.
    async fn years(&self) -> Result<Vec<i32>>;
}

pub struct SamplePost {
    pub slug: &'static str,
    pub title: &'static str,
    pub date: &'static str,
    pub pinned: bool,
    pub excerpt: &'static str,
    pub tags: &'static [&'static str],
    pub markdown: &'static str,
}

pub struct SampleSite {
    pub title: &'static str,
    pub description: &'static str,
    pub footer: &'static str,
    pub lang: &'static str,
    pub per_page: usize,
    /// Empty: no configured origin.
    pub origin: &'static str,
    /// `YYYY-MM-DD` the sample site was set up: its date even with no posts.
    pub updated: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/embedded.rs"));

/// The embedded sample posts (`crates/worker/sample/`), sorted once per isolate.
pub struct SampleSource {
    posts: Vec<StoredPost>,
}

impl SampleSource {
    pub fn get() -> &'static Self {
        static SOURCE: OnceLock<SampleSource> = OnceLock::new();
        SOURCE.get_or_init(|| Self::from_posts(SAMPLE_POSTS.iter().map(to_post).collect()))
    }

    /// Any set of posts in the sample order (tests build sites the samples do not cover).
    pub fn from_posts(mut posts: Vec<StoredPost>) -> Self {
        posts.sort_by(|a, b| {
            b.pinned
                .cmp(&a.pinned)
                .then(b.published_at.cmp(&a.published_at))
        });
        Self { posts }
    }

    fn by_date(&self) -> Vec<&StoredPost> {
        let mut all: Vec<&StoredPost> = self.posts.iter().collect();
        all.sort_by_key(|p| std::cmp::Reverse(p.published_at));
        all
    }
}

fn to_post(p: &SamplePost) -> StoredPost {
    StoredPost {
        slug: p.slug.to_owned(),
        title: p.title.to_owned(),
        lang: SAMPLE_SITE.lang.to_owned(),
        published_at: unix_from_date(p.date),
        pinned: p.pinned,
        excerpt: (!p.excerpt.is_empty()).then(|| p.excerpt.to_owned()),
        // What saving a post does; done once per isolate for the embedded samples.
        body: plumblog_markdown::render(p.markdown, Options::default()),
        tags: p
            .tags
            .iter()
            .map(|t| Tag {
                slug: (*t).to_owned(),
                name: (*t).to_owned(),
            })
            .collect(),
        translations: Vec::new(),
    }
}

/// `YYYY-MM-DD` at noon UTC (noon keeps the calendar day in every time zone a reader is in).
fn unix_from_date(date: &str) -> i64 {
    let mut parts = date.split('-').map(|p| p.parse::<i64>().unwrap_or(1));
    let (y, m, d) = (
        parts.next().unwrap_or(1970),
        parts.next().unwrap_or(1),
        parts.next().unwrap_or(1),
    );
    // Hinnant's days-from-civil.
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (era * 146_097 + doe - 719_468) * 86_400 + 43_200
}

fn year_of(unix: i64) -> i32 {
    // Same algorithm as render's civil(); only the year is needed here.
    let z = unix.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let y = yoe + era * 400 + i64::from(mp >= 10);
    i32::try_from(y).unwrap_or(i32::MAX)
}

// The sample never awaits; async keeps it a drop-in for the D1 implementation.
#[expect(
    clippy::unused_async_trait_impl,
    reason = "in-memory stand-in for async D1 storage"
)]
impl ReaderSource for SampleSource {
    async fn content_version(&self) -> Result<u64> {
        Ok(1)
    }

    async fn settings(&self) -> Result<Settings> {
        Ok(Settings {
            title: SAMPLE_SITE.title.to_owned(),
            description: SAMPLE_SITE.description.to_owned(),
            footer: SAMPLE_SITE.footer.to_owned(),
            lang: SAMPLE_SITE.lang.to_owned(),
            per_page: SAMPLE_SITE.per_page,
            origin: (!SAMPLE_SITE.origin.is_empty()).then(|| SAMPLE_SITE.origin.to_owned()),
            // The site's own date, moved forward by posts; never epoch 0 on an empty site.
            updated_at: self
                .posts
                .iter()
                .map(|p| p.published_at)
                .fold(unix_from_date(SAMPLE_SITE.updated), i64::max),
        })
    }

    async fn posts(&self, filter: Filter<'_>, offset: usize, limit: usize) -> Result<Slice> {
        let matching: Vec<&StoredPost> = match filter {
            Filter::All => self.posts.iter().collect(),
            // Lists other than home are by date only; pinning is a home page feature.
            Filter::Tag(slug) => self
                .by_date()
                .into_iter()
                .filter(|p| p.tags.iter().any(|t| t.slug == slug))
                .collect(),
            Filter::Year(y) => self
                .by_date()
                .into_iter()
                .filter(|p| year_of(p.published_at) == y)
                .collect(),
            Filter::Lang(lang) => self
                .by_date()
                .into_iter()
                .filter(|p| p.lang == lang)
                .collect(),
        };
        Ok(Slice {
            posts: matching
                .iter()
                .skip(offset)
                .take(limit)
                .map(|p| (*p).clone())
                .collect(),
            more: matching.len() > offset.saturating_add(limit),
        })
    }

    async fn post(&self, slug: &str) -> Result<Option<Neighbours>> {
        let all = self.by_date();
        let Some(i) = all.iter().position(|p| p.slug == slug) else {
            return Ok(None);
        };
        Ok(Some(Neighbours {
            post: all[i].clone(),
            newer: i.checked_sub(1).map(|j| all[j].clone()),
            older: all.get(i + 1).map(|p| (*p).clone()),
        }))
    }

    async fn tag(&self, slug: &str) -> Result<Option<Tag>> {
        Ok(self
            .posts
            .iter()
            .flat_map(|p| &p.tags)
            .find(|t| t.slug == slug)
            .cloned())
    }

    async fn tags(&self) -> Result<Vec<Tag>> {
        let mut tags: Vec<Tag> = self
            .posts
            .iter()
            .flat_map(|p| p.tags.iter().cloned())
            .collect();
        tags.sort_by(|a, b| a.slug.cmp(&b.slug));
        tags.dedup_by(|a, b| a.slug == b.slug);
        Ok(tags)
    }

    async fn years(&self) -> Result<Vec<i32>> {
        let mut years: Vec<i32> = self.posts.iter().map(|p| year_of(p.published_at)).collect();
        years.sort_unstable_by(|a, b| b.cmp(a));
        years.dedup();
        Ok(years)
    }
}

#[cfg(test)]
mod tests {
    use super::{unix_from_date, year_of};

    #[test]
    fn dates_round_trip() {
        assert_eq!(unix_from_date("1970-01-01"), 43_200);
        assert_eq!(year_of(unix_from_date("2026-02-28")), 2026);
        assert_eq!(year_of(unix_from_date("2025-12-31")), 2025);
        assert_eq!(year_of(unix_from_date("2024-01-01")), 2024);
    }
}
