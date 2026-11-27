//! Reads for the public site: published posts only, shaped for reader pages.
//!
//! Paging here is LIMIT/OFFSET, the one exception to "lists page by keyset": reader pages
//! are numbered (`/page/N`) and each is computed once per content version, then cached.
//! Admin and API lists stay keyset. The guards that keep OFFSET cheap are in
//! [`ReaderStore::reader_posts`].

use plumblog_model::{PostId, Result, TagId};

use crate::{Post, PostSummary, Tag};

/// Largest OFFSET a reader listing runs, whatever the count says: a crawler asking for
/// `/page/9999999` gets an empty page after one count, never a deep scan.
pub const READER_MAX_OFFSET: u32 = 10_000;

/// Most rows [`ReaderStore::reader_sitemap`] returns: the sitemap protocol's limit for one
/// file.
pub const READER_SITEMAP_MAX: u32 = 50_000;

/// Which published posts (kind `post`) a reader listing covers, and in which order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReaderFilter<'a> {
    /// Every language; pinned first, then by date. Pinning is a home page feature.
    All,
    /// By date only, like every filter below.
    Tag(TagId),
    /// Calendar year in UTC: `published_at` from Jan 1 00:00:00.000 up to the next Jan 1.
    /// Years outside 1..=9999 match nothing.
    Year(i32),
    /// One language (feeds, llms.txt).
    Lang(&'a str),
}

/// A tag as the reader shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReaderTag {
    pub slug: String,
    pub name: String,
}

/// A listing row: the post without bodies and its tags, ascending by tag id (the same order
/// as `post.tag_ids`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReaderRow {
    pub post: PostSummary,
    pub tags: Vec<ReaderTag>,
}

/// One page of a listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReaderPage {
    /// Every published post the filter matches, not only this page.
    pub total: u64,
    pub rows: Vec<ReaderRow>,
}

/// A published post or page with its tags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReaderPost {
    pub post: Post,
    pub tags: Vec<ReaderTag>,
}

/// Enough of a post to link to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReaderLink {
    pub lang: String,
    pub slug: String,
    pub title: String,
}

/// One sitemap entry: a published post or page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReaderSitemapRow {
    pub lang: String,
    pub slug: String,
    /// Unix ms.
    pub updated_at: i64,
    /// Rows sharing it are translations of each other.
    pub translation_group: PostId,
}

/// The posts around one post in date order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReaderNeighbours {
    /// Published just before it.
    pub older: Option<ReaderLink>,
    /// Published just after it.
    pub newer: Option<ReaderLink>,
}

/// `[start, end)` of `year` in Unix ms (UTC), or `None` outside 1..=9999.
#[must_use]
pub fn year_bounds(year: i32) -> Option<(i64, i64)> {
    if !(1..=9999).contains(&year) {
        return None;
    }
    let jan1 = |y: i64| {
        let p = y - 1;
        (p * 365 + p.div_euclid(4) - p.div_euclid(100) + p.div_euclid(400) - 719_162) * 86_400_000
    };
    Some((jan1(i64::from(year)), jan1(i64::from(year) + 1)))
}

/// The UTC year of a Unix ms time (proleptic Gregorian).
#[must_use]
pub fn year_of(ms: i64) -> i64 {
    let z = ms.div_euclid(86_400_000) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let month = (5 * doy + 2) / 153;
    yoe + era * 400 + i64::from(month >= 10)
}

// Workers are single-threaded, so futures need not be `Send` (see plumblog_auth::Crypto).
#[allow(async_fn_in_trait)]
pub trait ReaderStore {
    /// Published posts of kind `post` matching `filter`, `published_at` DESC, id DESC; for
    /// [`ReaderFilter::All`] pinned posts come first. `limit` is capped at `MAX_PAGE`.
    ///
    /// Guard: the count runs first (1 query). When `offset` is at or past the count, or at
    /// or past [`READER_MAX_OFFSET`], no rows are read and the page is empty with the true
    /// total, so a caller answers `/page/N` beyond the last page after one cheap count and
    /// never makes the store skip rows. Otherwise the rows are a 2nd query.
    async fn reader_posts(
        &self,
        filter: ReaderFilter<'_>,
        offset: u32,
        limit: u32,
    ) -> Result<ReaderPage>;
    /// The published post or page at `/<lang>/<slug>` with its tags; `None` for any other
    /// status. 1 query.
    async fn reader_post(&self, lang: &str, slug: &str) -> Result<Option<ReaderPost>>;
    /// Published posts and pages of a translation group, ordered by lang. 1 query.
    async fn reader_translations(&self, group: PostId) -> Result<Vec<ReaderLink>>;
    /// The published posts (kind `post`, same language) just before and after `post` by
    /// (`published_at`, id). Pinned posts are not special here: neighbours follow date only.
    /// Both `None` if `post` is not a published post. 1 query.
    async fn reader_neighbours(&self, post: PostId) -> Result<ReaderNeighbours>;
    /// UTC years with a published post (kind `post`), newest first. 1 query.
    async fn reader_years(&self) -> Result<Vec<i32>>;
    /// Tags with at least one published post (kind `post`), ordered by name then id.
    /// 1 query.
    async fn reader_tags(&self) -> Result<Vec<Tag>>;
    /// The newest published posts of kind `post` in one language with their bodies: what a
    /// feed needs. `published_at` DESC, id DESC; `limit` is capped at `MAX_PAGE`. 1 query.
    async fn reader_feed(&self, lang: &str, limit: u32) -> Result<Vec<ReaderPost>>;
    /// Every published post and page, newest first (`published_at` DESC, id DESC), at most
    /// `limit` capped at [`READER_SITEMAP_MAX`]: the sitemap in one query, without bodies.
    async fn reader_sitemap(&self, limit: u32) -> Result<Vec<ReaderSitemapRow>>;
}

#[cfg(test)]
mod tests {
    use super::{year_bounds, year_of};

    #[test]
    fn year_bounds_known_values() {
        assert_eq!(year_bounds(1970), Some((0, 31_536_000_000)));
        assert_eq!(year_bounds(2000).map(|b| b.0), Some(946_684_800_000));
        // 2024 is a leap year: 366 days.
        assert_eq!(year_bounds(2024).map(|b| b.1 - b.0), Some(366 * 86_400_000));
        assert_eq!(year_bounds(0), None);
        assert_eq!(year_bounds(10_000), None);
    }

    #[test]
    fn year_of_agrees_with_bounds_at_the_edges() {
        for y in [1, 1969, 1970, 2000, 2024, 2100, 9999] {
            let (a, b) = year_bounds(y).unwrap();
            assert_eq!(year_of(a), i64::from(y));
            assert_eq!(year_of(a - 1), i64::from(y) - 1);
            assert_eq!(year_of(b - 1), i64::from(y));
        }
    }
}
