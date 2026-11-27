//! Reader reads (`ReaderStore`): published posts only. The index each query uses is named in
//! `migrations/0006_reader_indexes.sql` and `0001_init.sql`.

use plumblog_model::{PostId, Result, TagId};
use plumblog_store::{
    READER_MAX_OFFSET, READER_SITEMAP_MAX, ReaderFilter, ReaderLink, ReaderNeighbours, ReaderPage,
    ReaderPost, ReaderRow, ReaderSitemapRow, ReaderStore, ReaderTag, Tag, year_bounds,
};
use worker::js_sys::{Array, JSON};
use worker::wasm_bindgen::{JsCast, JsValue};

use super::posts::{POST_COLS, SUMMARY_COLS, read_post, read_summary};
use super::{D1Store, Row, bound, int, limit, storage, text};

/// Tags of `p` as JSON `[[id, slug, name], ...]`: names may hold any character, so no
/// separator-joined list. JSON order is not relied on; [`read_tags`] sorts by id.
const TAGS_COL: &str = "(SELECT json_group_array(json_array(t.id, t.slug, t.name)) \
     FROM post_tags pt JOIN tags t ON t.id = pt.tag_id WHERE pt.post_id = p.id)";

/// Every reader listing: published posts, never pages.
const LISTED: &str = "p.status = 'published' AND p.kind = 'post'";

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

/// FROM and WHERE of a listing, the extra sort column and parameters. A tag drives from
/// `post_tags_tag` (CROSS JOIN fixes the join order), so a tag page reads its own posts,
/// not every published post.
fn listing(filter: ReaderFilter<'_>) -> Result<Option<(String, Vec<JsValue>)>> {
    Ok(Some(match filter {
        ReaderFilter::All => (format!("posts p WHERE {LISTED}"), Vec::new()),
        ReaderFilter::Tag(TagId(t)) => (
            format!(
                "post_tags pt CROSS JOIN posts p ON p.id = pt.post_id \
                 WHERE pt.tag_id = ? AND {LISTED}"
            ),
            vec![bound(t)],
        ),
        ReaderFilter::Year(y) => {
            let Some((from, to)) = year_bounds(y) else {
                return Ok(None);
            };
            (
                format!("posts p WHERE {LISTED} AND p.published_at >= ? AND p.published_at < ?"),
                vec![int(from)?, int(to)?],
            )
        }
        ReaderFilter::Lang(l) => (
            format!("posts p WHERE {LISTED} AND p.lang = ?"),
            vec![text(l)],
        ),
    }))
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
        let s = self.stmt(
            "reader_count",
            &format!("SELECT COUNT(*) FROM {from}"),
            &params,
        )?;
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
        // The inner query pages over the index alone (ids and sort keys); only the page's
        // rows are then read in full, with their tags.
        let (keys, inner_order, outer_order) = if filter == ReaderFilter::All {
            (
                "p.id, p.pinned, p.published_at",
                "p.pinned DESC, p.published_at DESC, p.id DESC",
                "k.pinned DESC, k.published_at DESC, k.id DESC",
            )
        } else {
            (
                "p.id, p.published_at",
                "p.published_at DESC, p.id DESC",
                "k.published_at DESC, k.id DESC",
            )
        };
        let mut params = params;
        params.extend([limit(limit_n), JsValue::from(offset)]);
        let s = self.stmt(
            "reader_posts",
            &format!(
                "SELECT {SUMMARY_COLS}, {TAGS_COL} FROM \
                 (SELECT {keys} FROM {from} ORDER BY {inner_order} LIMIT ? OFFSET ?) k \
                 CROSS JOIN posts p ON p.id = k.id ORDER BY {outer_order}"
            ),
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
        let s = self.stmt(
            "reader_post",
            &format!(
                "SELECT {POST_COLS}, {TAGS_COL} FROM posts p \
                 WHERE p.lang = ? AND p.slug = ? AND p.status = 'published'"
            ),
            &[text(lang), text(slug)],
        )?;
        match self.row(s).await? {
            Some(mut r) => Ok(Some(ReaderPost {
                post: read_post(&mut r)?,
                tags: read_tags("reader_post", &mut r)?,
            })),
            None => Ok(None),
        }
    }

    // 1 query (translation switcher, hreflang).
    async fn reader_translations(&self, group: PostId) -> Result<Vec<ReaderLink>> {
        let s = self.stmt(
            "reader_translations",
            "SELECT lang, slug, title FROM posts \
             WHERE translation_group = ? AND status = 'published' ORDER BY lang",
            &[bound(group.0)],
        )?;
        self.rows(s).await?.iter_mut().map(read_link).collect()
    }

    // 1 query: two LIMIT 1 range reads on posts_listing (lang, then date).
    async fn reader_neighbours(&self, post: PostId) -> Result<ReaderNeighbours> {
        let side = |cmp: &str, order: &str, tag: &str| {
            format!(
                "SELECT * FROM (SELECT '{tag}', p.lang, p.slug, p.title \
                 FROM posts me JOIN posts p ON {LISTED} AND p.lang = me.lang \
                 AND (p.published_at, p.id) {cmp} (me.published_at, me.id) \
                 WHERE me.id = ? AND me.status = 'published' AND me.kind = 'post' \
                 ORDER BY p.published_at {order}, p.id {order} LIMIT 1)"
            )
        };
        let s = self.stmt(
            "reader_neighbours",
            &format!(
                "{} UNION ALL {}",
                side("<", "DESC", "older"),
                side(">", "ASC", "newer")
            ),
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
        let s = self.stmt(
            "reader_years",
            &format!(
                "SELECT DISTINCT CAST(strftime('%Y', p.published_at / 1000.0, 'unixepoch') \
                 AS INTEGER) AS y FROM posts p WHERE {LISTED} \
                 AND p.published_at >= ? AND p.published_at < ? ORDER BY y DESC"
            ),
            &[int(from)?, int(to)?],
        )?;
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
        let s = self.stmt(
            "reader_feed",
            &format!(
                "SELECT {POST_COLS}, {TAGS_COL} FROM posts p WHERE {LISTED} AND p.lang = ? \
                 ORDER BY p.published_at DESC, p.id DESC LIMIT ?"
            ),
            &[text(lang), limit(limit_n)],
        )?;
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

    // 1 query: the date index alone, no bodies.
    async fn reader_sitemap(&self, limit_n: u32) -> Result<Vec<ReaderSitemapRow>> {
        let s = self.stmt(
            "reader_sitemap",
            "SELECT p.lang, p.slug, p.updated_at, p.translation_group FROM posts p \
             WHERE p.status = 'published' ORDER BY p.published_at DESC, p.id DESC LIMIT ?",
            &[JsValue::from(limit_n.min(READER_SITEMAP_MAX))],
        )?;
        self.rows(s)
            .await?
            .iter_mut()
            .map(|r| {
                Ok(ReaderSitemapRow {
                    lang: r.text()?,
                    slug: r.text()?,
                    updated_at: r.int()?,
                    translation_group: PostId(r.int()?),
                })
            })
            .collect()
    }

    // 1 query: each tag stops at its first published post (post_tags_tag, then the post).
    async fn reader_tags(&self) -> Result<Vec<Tag>> {
        let s = self.stmt(
            "reader_tags",
            &format!(
                "SELECT t.id, t.slug, t.name FROM tags t WHERE EXISTS (SELECT 1 \
                 FROM post_tags pt CROSS JOIN posts p ON p.id = pt.post_id \
                 WHERE pt.tag_id = t.id AND {LISTED}) ORDER BY t.name, t.id"
            ),
            &[],
        )?;
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
