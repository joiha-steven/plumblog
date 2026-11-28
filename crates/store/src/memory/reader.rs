//! Reader reads: published posts only, in the orders D1 uses.

use std::cmp::Reverse;

use plumblog_model::{PostId, Result, SeriesId, TagId};

use super::{MemoryStore, State, count};
use crate::{
    Post, PostKind, PostStatus, PostSummary, READER_MAX_OFFSET, READER_SITEMAP_MAX, ReaderCover,
    ReaderFilter, ReaderLink, ReaderNeighbours, ReaderPage, ReaderPost, ReaderRow, ReaderSeries,
    ReaderSitemapRow, ReaderStore, ReaderTag, ReaderTaxonPage, Tag, page_len, year_bounds, year_of,
};

fn listed(p: &Post) -> bool {
    p.status == PostStatus::Published && p.kind == PostKind::Post
}

fn link(p: &Post) -> ReaderLink {
    ReaderLink {
        lang: p.lang.clone(),
        slug: p.slug.clone(),
        title: p.title.clone(),
    }
}

/// `(published_at, id)`: the date order every reader list uses.
fn date_key(p: &Post) -> (i64, PostId) {
    (p.published_at.unwrap_or(i64::MIN), p.id)
}

impl State {
    /// Tags of `p` ascending by id, as D1 returns them.
    fn reader_tags_of(&self, p: &Post) -> Vec<ReaderTag> {
        p.tag_ids
            .iter()
            .filter_map(|t| self.tags.get(t))
            .map(|t| ReaderTag {
                slug: t.slug.clone(),
                name: t.name.clone(),
            })
            .collect()
    }

    fn matches(filter: ReaderFilter<'_>, p: &Post) -> bool {
        match filter {
            ReaderFilter::Home(l) | ReaderFilter::Lang(l) => p.lang == l,
            ReaderFilter::Tag(t, l) => p.lang == l && p.tag_ids.contains(&t),
            ReaderFilter::Series(id, l) => p.lang == l && p.series_id == Some(id),
            ReaderFilter::Year(y) => year_bounds(y)
                .is_some_and(|(a, b)| p.published_at.is_some_and(|at| (a..b).contains(&at))),
        }
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "in-memory stand-in for async D1 storage"
)]
impl ReaderStore for MemoryStore {
    async fn reader_posts(
        &self,
        filter: ReaderFilter<'_>,
        offset: u32,
        limit: u32,
    ) -> Result<ReaderPage> {
        Ok(self.read(|s| {
            let mut rows: Vec<&Post> = s
                .posts
                .values()
                .filter(|p| listed(p) && State::matches(filter, p))
                .collect();
            let total = count(rows.len());
            if u64::from(offset) >= total || offset >= READER_MAX_OFFSET {
                return ReaderPage {
                    total,
                    rows: Vec::new(),
                };
            }
            if matches!(filter, ReaderFilter::Series(..)) {
                rows.sort_by_key(|p| (p.series_position, date_key(p)));
            } else {
                let pinned_first = matches!(filter, ReaderFilter::Home(_));
                rows.sort_by_key(|p| Reverse((pinned_first && p.pinned, date_key(p))));
            }
            let rows = rows
                .into_iter()
                .skip(usize::try_from(offset).unwrap_or(usize::MAX))
                .take(page_len(limit))
                .map(|p| ReaderRow {
                    post: PostSummary::from(p),
                    tags: s.reader_tags_of(p),
                })
                .collect();
            ReaderPage { total, rows }
        }))
    }

    async fn reader_post(&self, lang: &str, slug: &str) -> Result<Option<ReaderPost>> {
        Ok(self.read(|s| {
            s.posts
                .values()
                .find(|p| p.lang == lang && p.slug == slug && p.status == PostStatus::Published)
                .map(|p| ReaderPost {
                    post: p.clone(),
                    tags: s.reader_tags_of(p),
                    series: p
                        .series_id
                        .and_then(|id| s.series.get(&id))
                        .map(|x| ReaderSeries {
                            slug: x.slug.clone(),
                            title: x.title.clone(),
                        }),
                    cover: p
                        .cover_media_id
                        .and_then(|id| s.media.get(&id))
                        .map(|m| ReaderCover {
                            id: m.id,
                            r2_key: m.r2_key.clone(),
                            width: m.width,
                            height: m.height,
                            alt: m.alt.clone(),
                            variants_json: m.variants_json.clone(),
                        }),
                })
        }))
    }

    async fn reader_translations(&self, group: PostId) -> Result<Vec<ReaderLink>> {
        Ok(self.read(|s| {
            let mut rows: Vec<&Post> = s
                .posts
                .values()
                .filter(|p| p.translation_group == group && p.status == PostStatus::Published)
                .collect();
            rows.sort_by(|a, b| a.lang.cmp(&b.lang));
            rows.into_iter().map(link).collect()
        }))
    }

    async fn reader_neighbours(&self, post: PostId) -> Result<ReaderNeighbours> {
        Ok(self.read(|s| {
            let Some(me) = s.posts.get(&post).filter(|p| listed(p)) else {
                return ReaderNeighbours {
                    older: None,
                    newer: None,
                };
            };
            let here = date_key(me);
            let same = || s.posts.values().filter(|p| listed(p) && p.lang == me.lang);
            ReaderNeighbours {
                older: same()
                    .filter(|p| date_key(p) < here)
                    .max_by_key(|p| date_key(p))
                    .map(link),
                newer: same()
                    .filter(|p| date_key(p) > here)
                    .min_by_key(|p| date_key(p))
                    .map(link),
            }
        }))
    }

    async fn reader_post_exists(&self, lang: &str, slug: &str) -> Result<bool> {
        Ok(self.read(|s| {
            s.posts
                .values()
                .any(|p| p.status == PostStatus::Published && p.lang == lang && p.slug == slug)
        }))
    }

    async fn reader_langs_with_posts(&self, langs: &[String]) -> Result<Vec<String>> {
        Ok(self.read(|s| {
            langs
                .iter()
                .filter(|l| s.posts.values().any(|p| listed(p) && p.lang == **l))
                .cloned()
                .collect()
        }))
    }

    async fn reader_years(&self) -> Result<Vec<i32>> {
        Ok(self.read(|s| {
            let mut years: Vec<i32> = s
                .posts
                .values()
                .filter(|p| listed(p))
                .filter_map(|p| p.published_at)
                .map(year_of)
                .filter(|y| (1..=9999).contains(y))
                .filter_map(|y| i32::try_from(y).ok())
                .collect();
            years.sort_unstable_by(|a, b| b.cmp(a));
            years.dedup();
            years
        }))
    }

    async fn reader_feed(&self, lang: &str, limit: u32) -> Result<Vec<ReaderPost>> {
        Ok(self.read(|s| {
            let mut rows: Vec<&Post> = s
                .posts
                .values()
                .filter(|p| listed(p) && p.lang == lang)
                .collect();
            rows.sort_by_key(|p| Reverse(date_key(p)));
            rows.into_iter()
                .take(page_len(limit))
                .map(|p| ReaderPost {
                    post: p.clone(),
                    tags: s.reader_tags_of(p),
                    series: None,
                    cover: None,
                })
                .collect()
        }))
    }

    async fn reader_sitemap(&self, limit: u32) -> Result<Vec<ReaderSitemapRow>> {
        Ok(self.read(|s| {
            let mut rows: Vec<&Post> = s
                .posts
                .values()
                .filter(|p| p.status == PostStatus::Published)
                .collect();
            rows.sort_by_key(|p| Reverse(date_key(p)));
            rows.into_iter()
                .take(usize::try_from(limit.min(READER_SITEMAP_MAX)).unwrap_or(0))
                .map(|p| ReaderSitemapRow {
                    lang: p.lang.clone(),
                    slug: p.slug.clone(),
                    updated_at: p.updated_at,
                    translation_group: p.translation_group,
                    is_page: p.kind == PostKind::Page,
                })
                .collect()
        }))
    }

    async fn reader_tag_pages(&self, langs: &[String]) -> Result<Vec<ReaderTaxonPage>> {
        Ok(self.read(|s| {
            let mut tags: Vec<&Tag> = s.tags.values().collect();
            tags.sort_by(|a, b| (&a.name, a.id).cmp(&(&b.name, b.id)));
            let mut out = Vec::new();
            for t in tags {
                for l in langs {
                    if s.posts
                        .values()
                        .any(|p| listed(p) && p.lang == *l && p.tag_ids.contains(&t.id))
                    {
                        out.push(ReaderTaxonPage {
                            slug: t.slug.clone(),
                            name: t.name.clone(),
                            lang: l.clone(),
                        });
                    }
                }
            }
            out
        }))
    }

    async fn reader_series_pages(&self, langs: &[String]) -> Result<Vec<ReaderTaxonPage>> {
        Ok(self.read(|s| {
            let mut all: Vec<_> = s.series.values().collect();
            all.sort_by(|a, b| (&a.title, a.id).cmp(&(&b.title, b.id)));
            let mut out = Vec::new();
            for x in all {
                for l in langs {
                    if s.posts
                        .values()
                        .any(|p| listed(p) && p.lang == *l && p.series_id == Some(x.id))
                    {
                        out.push(ReaderTaxonPage {
                            slug: x.slug.clone(),
                            name: x.title.clone(),
                            lang: l.clone(),
                        });
                    }
                }
            }
            out
        }))
    }

    async fn reader_tag_langs(&self, tag: TagId, langs: &[String]) -> Result<Vec<String>> {
        Ok(self.read(|s| {
            langs
                .iter()
                .filter(|l| {
                    s.posts
                        .values()
                        .any(|p| listed(p) && p.lang == **l && p.tag_ids.contains(&tag))
                })
                .cloned()
                .collect()
        }))
    }

    async fn reader_series_langs(&self, series: SeriesId, langs: &[String]) -> Result<Vec<String>> {
        Ok(self.read(|s| {
            langs
                .iter()
                .filter(|l| {
                    s.posts
                        .values()
                        .any(|p| listed(p) && p.lang == **l && p.series_id == Some(series))
                })
                .cloned()
                .collect()
        }))
    }
}
