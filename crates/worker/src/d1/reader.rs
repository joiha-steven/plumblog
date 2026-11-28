//! Reader reads (`ReaderStore`): published posts only. The SQL is in `reader_sql`, where a test
//! checks the plan of each query against the index it is meant to use.

use plumblog_model::{PostId, Result, TagId};
use plumblog_store::{
    READER_MAX_OFFSET, READER_SITEMAP_MAX, ReaderFilter, ReaderLink, ReaderNeighbours, ReaderPage,
    ReaderPost, ReaderRow, ReaderSitemapRow, ReaderStore, ReaderTag, Tag, year_bounds,
};
use worker::js_sys::{Array, JSON};
use worker::wasm_bindgen::{JsCast, JsValue};

use super::posts::{read_post, read_summary};
use super::reader_sql as sql;
use super::{D1Store, Row, bound, int, limit, storage, text};

fn read_tags(name: &'static str, r: &mut Row) -> Result<Vec<ReaderTag>> {
    let json = r.text()?;
    let bad = || storage(name, &format!("bad tag list {json:?}"));
    let list = JSON::parse(&json)
        .ok()
        .and_then(|v| v.dyn_into::<Array>().ok())
        .ok_or_else(bad)?;
    let mut tags = Vec::new();
    for item in list.iter() {
        let cells = item.dyn_into::<Array>().map_err(|_| bad())?;
        let id = cells.get(0).as_f64().ok_or_else(bad)?;
        let slug = cells.get(1).as_string().ok_or_else(bad)?;
        let name = cells.get(2).as_string().ok_or_else(bad)?;
        tags.push((id, ReaderTag { slug, name }));
    }
    tags.sort_by(|a, b| a.0.total_cmp(&b.0));
    Ok(tags.into_iter().map(|(_, t)| t).collect())
}

fn read_link(r: &mut Row) -> Result<ReaderLink> {
    Ok(ReaderLink {
        lang: r.text()?,
        slug: r.text()?,
        title: r.text()?,
    })
}

/// FROM and WHERE of a listing and its parameters; `None` for a year outside 1..=9999.
fn listing(filter: ReaderFilter<'_>) -> Result<Option<(String, Vec<JsValue>)>> {
    let Some(from) = sql::listing_from(filter) else {
        return Ok(None);
    };
    let params = match filter {
        ReaderFilter::Tag(TagId(t)) => vec![bound(t)],
        ReaderFilter::Year(y) => {
            let (from, to) = year_bounds(y).ok_or_else(|| storage("reader_posts", "year"))?;
            vec![int(from)?, int(to)?]
        }
        ReaderFilter::Home(l) | ReaderFilter::Lang(l) => vec![text(l)],
    };
    Ok(Some((from, params)))
}

impl ReaderStore for D1Store {
    // 2 queries (count, page), or 1 when the page is past the end or the cap; 0 for a year
    // outside 1..=9999.
    async fn reader_posts(
        &self,
        filter: ReaderFilter<'_>,
        offset: u32,
        limit_n: u32,
    ) -> Result<ReaderPage> {
        let Some((from, params)) = listing(filter)? else {
            return Ok(ReaderPage {
                total: 0,
                rows: Vec::new(),
            });
        };
        let s = self.stmt("reader_count", &sql::count(&from), &params)?;
        let total = match self.row(s).await? {
            Some(mut r) => r.uint()?,
            None => return Err(storage("reader_count", "COUNT returned no row")),
        };
        if u64::from(offset) >= total || offset >= READER_MAX_OFFSET {
            return Ok(ReaderPage {
                total,
                rows: Vec::new(),
            });
        }
        let mut params = params;
        params.extend([limit(limit_n), JsValue::from(offset)]);
        let s = self.stmt(
            "reader_posts",
            &sql::rows(&from, matches!(filter, ReaderFilter::Home(_))),
            &params,
        )?;
        let rows = self
            .rows(s)
            .await?
            .iter_mut()
            .map(|r| {
                Ok(ReaderRow {
                    post: read_summary(r)?,
                    tags: read_tags("reader_posts", r)?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(ReaderPage { total, rows })
    }

    // 1 query (post page): the post, its tag ids and its tags.
    async fn reader_post(&self, lang: &str, slug: &str) -> Result<Option<ReaderPost>> {
        let s = self.stmt("reader_post", &sql::post(), &[text(lang), text(slug)])?;
        match self.row(s).await? {
            Some(mut r) => Ok(Some(ReaderPost {
                post: read_post(&mut r)?,
                tags: read_tags("reader_post", &mut r)?,
            })),
            None => Ok(None),
        }
    }

    // 1 query (translation switcher, hreflang), by posts_group_lang.
    async fn reader_translations(&self, group: PostId) -> Result<Vec<ReaderLink>> {
        let s = self.stmt("reader_translations", sql::TRANSLATIONS, &[bound(group.0)])?;
        self.rows(s).await?.iter_mut().map(read_link).collect()
    }

    // 1 query: two LIMIT 1 range reads on posts_listing (lang, then date).
    async fn reader_neighbours(&self, post: PostId) -> Result<ReaderNeighbours> {
        let s = self.stmt(
            "reader_neighbours",
            &sql::neighbours(),
            &[bound(post.0), bound(post.0)],
        )?;
        let mut out = ReaderNeighbours {
            older: None,
            newer: None,
        };
        for r in &mut self.rows(s).await? {
            let which = r.text()?;
            let link = Some(read_link(r)?);
            match which.as_str() {
                "older" => out.older = link,
                "newer" => out.newer = link,
                other => {
                    return Err(storage(
                        "reader_neighbours",
                        &format!("unknown side {other:?}"),
                    ));
                }
            }
        }
        Ok(out)
    }

    // 1 query: a covering scan of posts_by_date. '%Y' of ms / 1000.0 is the UTC year,
    // agreeing with plumblog_store::year_of at the Jan 1 edges (contract case). Posts outside
    // years 1..=9999 are left out (strftime answers NULL there), as ReaderFilter::Year does.
    async fn reader_years(&self) -> Result<Vec<i32>> {
        let (from, _) = year_bounds(1).ok_or_else(|| storage("reader_years", "year 1"))?;
        let (_, to) = year_bounds(9999).ok_or_else(|| storage("reader_years", "year 9999"))?;
        let s = self.stmt("reader_years", &sql::years(), &[int(from)?, int(to)?])?;
        self.rows(s)
            .await?
            .iter_mut()
            .map(|r| {
                let y = r.int()?;
                i32::try_from(y).map_err(|_| storage("reader_years", &format!("year {y}")))
            })
            .collect()
    }

    // 1 query: the newest posts of one language on posts_listing, bodies and tags included.
    async fn reader_feed(&self, lang: &str, limit_n: u32) -> Result<Vec<ReaderPost>> {
        let s = self.stmt("reader_feed", &sql::feed(), &[text(lang), limit(limit_n)])?;
        self.rows(s)
            .await?
            .iter_mut()
            .map(|r| {
                Ok(ReaderPost {
                    post: read_post(r)?,
                    tags: read_tags("reader_feed", r)?,
                })
            })
            .collect()
    }

    // 1 query: the newest posts and the newest pages, each by posts_by_date (no sort), merged
    // here; at most 2 x limit rows, and pages are few.
    async fn reader_sitemap(&self, limit_n: u32) -> Result<Vec<ReaderSitemapRow>> {
        let n = limit_n.min(READER_SITEMAP_MAX);
        let s = self.stmt(
            "reader_sitemap",
            &sql::sitemap(),
            &[JsValue::from(n), JsValue::from(n)],
        )?;
        let mut rows = Vec::new();
        for r in &mut self.rows(s).await? {
            let row = ReaderSitemapRow {
                lang: r.text()?,
                slug: r.text()?,
                updated_at: r.int()?,
                translation_group: PostId(r.int()?),
            };
            rows.push((r.int()?, r.int()?, row));
        }
        // Newest first, like every reader list: published_at DESC, id DESC.
        rows.sort_by_key(|(at, id, _)| std::cmp::Reverse((*at, *id)));
        rows.truncate(usize::try_from(n).unwrap_or(usize::MAX));
        Ok(rows.into_iter().map(|(_, _, row)| row).collect())
    }

    // 1 query: each tag stops at its first published post (post_tags_tag, then the post).
    async fn reader_tags(&self) -> Result<Vec<Tag>> {
        let s = self.stmt("reader_tags", &sql::tags(), &[])?;
        self.rows(s)
            .await?
            .iter_mut()
            .map(|r| {
                Ok(Tag {
                    id: TagId(r.int()?),
                    slug: r.text()?,
                    name: r.text()?,
                })
            })
            .collect()
    }
}
