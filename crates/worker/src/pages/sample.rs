//! The embedded sample site (`crates/worker/sample/`): a `ReaderSource` for unit tests of
//! rendering and for `/__seed`, which copies it into the store. The Worker never serves it.

use super::source::{
    Filter, Neighbours, ReaderSource, Settings, SitemapPost, Slice, StoredPost, Tag,
};
use plumblog_markdown::Options;
use plumblog_model::{PostId, Result, TagId};
use std::sync::OnceLock;

pub struct SamplePost {
    pub slug: &'static str,
    pub title: &'static str,
    pub date: &'static str,
    pub pinned: bool,
    pub excerpt: &'static str,
    pub tags: &'static [&'static str],
    /// Empty: the site language.
    pub lang: &'static str,
    /// Slug of the post this one translates; empty if none.
    pub translation_of: &'static str,
    pub markdown: &'static str,
}

pub struct SampleSite {
    pub title: &'static str,
    pub description: &'static str,
    pub footer: &'static str,
    pub lang: &'static str,
    pub languages: &'static [&'static str],
    pub per_page: usize,
    /// Empty: no configured origin.
    pub origin: &'static str,
    /// `YYYY-MM-DD` the sample site was set up: its date even with no posts.
    pub updated: &'static str,
}

include!(concat!(env!("OUT_DIR"), "/sample.rs"));

/// The embedded sample posts (`crates/worker/sample/`), sorted once per isolate.
pub struct SampleSource {
    posts: Vec<StoredPost>,
}

impl SampleSource {
    pub fn get() -> &'static Self {
        static SOURCE: OnceLock<SampleSource> = OnceLock::new();
        SOURCE.get_or_init(|| {
            let mut posts: Vec<StoredPost> = SAMPLE_POSTS.iter().map(to_post).collect();
            link_translations(&mut posts);
            Self::from_posts(posts)
        })
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

/// Fills `translations` both ways for every sample post that names `translation_of`.
fn link_translations(posts: &mut [StoredPost]) {
    let pairs: Vec<(String, String)> = SAMPLE_POSTS
        .iter()
        .filter(|p| !p.translation_of.is_empty())
        .map(|p| (p.slug.to_owned(), p.translation_of.to_owned()))
        .collect();
    for (slug, of) in pairs {
        let find = |posts: &[StoredPost], s: &str| {
            posts
                .iter()
                .find(|p| p.slug == s)
                .map(|p| (p.lang.clone(), p.slug.clone()))
        };
        let (Some(a), Some(b)) = (find(posts, &slug), find(posts, &of)) else {
            continue;
        };
        for p in posts.iter_mut() {
            if p.slug == a.1 {
                p.translations.push(b.clone());
            } else if p.slug == b.1 {
                p.translations.push(a.clone());
            }
        }
    }
}

fn to_post(p: &SamplePost) -> StoredPost {
    StoredPost {
        id: PostId(0),
        is_page: false,
        comments_enabled: false,
        slug: p.slug.to_owned(),
        title: p.title.to_owned(),
        lang: if p.lang.is_empty() {
            SAMPLE_SITE.lang
        } else {
            p.lang
        }
        .to_owned(),
        published_at: unix_from_date(p.date),
        pinned: p.pinned,
        excerpt: (!p.excerpt.is_empty()).then(|| p.excerpt.to_owned()),
        // What saving a post does; done once per isolate for the embedded samples.
        body: plumblog_markdown::render(p.markdown, Options::default()),
        tags: p
            .tags
            .iter()
            .map(|t| Tag {
                id: TagId(0),
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
    async fn settings(&self) -> Result<Settings> {
        Ok(Settings {
            title: SAMPLE_SITE.title.to_owned(),
            description: SAMPLE_SITE.description.to_owned(),
            footer: SAMPLE_SITE.footer.to_owned(),
            lang: SAMPLE_SITE.lang.to_owned(),
            languages: SAMPLE_SITE
                .languages
                .iter()
                .map(|l| (*l).to_owned())
                .collect(),
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
            Filter::Tag(t) => self
                .by_date()
                .into_iter()
                .filter(|p| p.tags.iter().any(|x| x.slug == t.slug))
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
            total: matching.len(),
            posts: matching
                .iter()
                .skip(offset)
                .take(limit)
                .map(|p| (*p).clone())
                .collect(),
        })
    }

    async fn feed(&self, lang: &str, limit: usize) -> Result<Vec<StoredPost>> {
        Ok(self.posts(Filter::Lang(lang), 0, limit).await?.posts)
    }

    async fn sitemap_posts(&self, limit: usize) -> Result<Vec<SitemapPost>> {
        Ok(self
            .by_date()
            .into_iter()
            .take(limit)
            .map(|p| SitemapPost {
                lang: p.lang.clone(),
                slug: p.slug.clone(),
                updated_at: p.published_at,
                // Unique per post, shared with its translations (sample slugs are unique).
                group: self
                    .posts
                    .iter()
                    .position(|q| {
                        q.slug == p.slug || p.translations.iter().any(|(_, s)| *s == q.slug)
                    })
                    .map_or(0, |i| i64::try_from(i).unwrap_or(0) + 1),
            })
            .collect())
    }

    async fn post(&self, lang: &str, slug: &str) -> Result<Option<Neighbours>> {
        let all: Vec<&StoredPost> = self
            .by_date()
            .into_iter()
            .filter(|p| p.lang == lang)
            .collect();
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
