//! Post reads, status changes and search. `save_post` lives in `post_save.rs`.

use plumblog_model::{
    Error, ErrorCode, MediaId, PostId, Result, RevisionId, SeriesId, TagId, UserId,
};
use plumblog_store::{
    Post, PostCounts, PostCursor, PostKind, PostOrder, PostQuery, PostStatus, PostStore,
    PostSummary, Revision, SavePost, search,
};
use worker::wasm_bindgen::JsValue;

use super::{D1Store, Row, bound, bump_if, flag, int, invalid, limit, not_found, storage, text};

/// Every `posts` column, in [`read_post`] order, with tag ids as one text column so a post
/// and its tags are one query.
pub(super) const POST_COLS: &str = "p.id, p.kind, p.translation_group, p.lang, p.slug, p.status, \
     p.trashed_from, p.published_at, p.author_id, p.title, p.markdown, p.html, p.toc_json, \
     p.excerpt, p.word_count, p.reading_minutes, p.pinned, p.comments_enabled, \
     p.cover_media_id, p.series_id, p.series_position, p.created_at, p.updated_at, p.version, \
     (SELECT group_concat(tag_id) FROM post_tags WHERE post_id = p.id)";

/// [`POST_COLS`] without the bodies: list pages never load them.
pub(super) const SUMMARY_COLS: &str = "p.id, p.kind, p.translation_group, p.lang, p.slug, p.status, \
     p.trashed_from, p.published_at, p.author_id, p.title, \
     p.excerpt, p.word_count, p.reading_minutes, p.pinned, p.comments_enabled, \
     p.cover_media_id, p.series_id, p.series_position, p.created_at, p.updated_at, p.version, \
     (SELECT group_concat(tag_id) FROM post_tags WHERE post_id = p.id)";

const REVISION_COLS: &str = "id, post_id, author_id, title, markdown, created_at";

pub(super) fn post_not_found(id: PostId) -> Error {
    Error::new(ErrorCode::PostNotFound, format!("post {} not found", id.0))
}

/// `"3,1,2"` from `group_concat` -> ascending ids.
fn tag_ids(r: &mut Row) -> Result<Vec<TagId>> {
    let Some(list) = r.opt_text()? else {
        return Ok(Vec::new());
    };
    let mut ids = list
        .split(',')
        .map(|s| {
            s.parse::<i64>()
                .map(TagId)
                .map_err(|_| storage("post_tags", &format!("bad tag id list {list:?}")))
        })
        .collect::<Result<Vec<TagId>>>()?;
    ids.sort_unstable();
    Ok(ids)
}

pub(super) fn read_post(r: &mut Row) -> Result<Post> {
    Ok(Post {
        id: PostId(r.int()?),
        kind: r.code("post kind", PostKind::parse)?,
        translation_group: PostId(r.int()?),
        lang: r.text()?,
        slug: r.text()?,
        status: r.code("post status", PostStatus::parse)?,
        trashed_from: r.opt_code("post status", PostStatus::parse)?,
        published_at: r.opt_int()?,
        author_id: UserId(r.int()?),
        title: r.text()?,
        markdown: r.text()?,
        html: r.text()?,
        toc_json: r.text()?,
        excerpt: r.text()?,
        word_count: r.int()?,
        reading_minutes: r.int()?,
        pinned: r.flag()?,
        comments_enabled: r.flag()?,
        cover_media_id: r.opt_int()?.map(MediaId),
        series_id: r.opt_int()?.map(SeriesId),
        series_position: r.opt_int()?,
        created_at: r.int()?,
        updated_at: r.int()?,
        version: r.int()?,
        tag_ids: tag_ids(r)?,
    })
}

pub(super) fn read_summary(r: &mut Row) -> Result<PostSummary> {
    Ok(PostSummary {
        id: PostId(r.int()?),
        kind: r.code("post kind", PostKind::parse)?,
        translation_group: PostId(r.int()?),
        lang: r.text()?,
        slug: r.text()?,
        status: r.code("post status", PostStatus::parse)?,
        trashed_from: r.opt_code("post status", PostStatus::parse)?,
        published_at: r.opt_int()?,
        author_id: UserId(r.int()?),
        title: r.text()?,
        excerpt: r.text()?,
        word_count: r.int()?,
        reading_minutes: r.int()?,
        pinned: r.flag()?,
        comments_enabled: r.flag()?,
        cover_media_id: r.opt_int()?.map(MediaId),
        series_id: r.opt_int()?.map(SeriesId),
        series_position: r.opt_int()?,
        created_at: r.int()?,
        updated_at: r.int()?,
        version: r.int()?,
        tag_ids: tag_ids(r)?,
    })
}

fn read_revision(r: &mut Row) -> Result<Revision> {
    Ok(Revision {
        id: RevisionId(r.int()?),
        post_id: PostId(r.int()?),
        author_id: r.opt_int()?.map(UserId),
        title: r.text()?,
        markdown: r.text()?,
        created_at: r.int()?,
    })
}

/// `WHERE`, `ORDER BY` and parameters of a listing. Only fixed fragments go into the SQL;
/// every value is a parameter.
fn listing_sql(q: &PostQuery) -> Result<(String, Vec<JsValue>)> {
    let mut wh: Vec<&str> = Vec::new();
    let mut params: Vec<JsValue> = Vec::new();
    if let Some(k) = q.kind {
        wh.push("p.kind = ?");
        params.push(text(k.as_str()));
    }
    if let Some(s) = q.status {
        wh.push("p.status = ?");
        params.push(text(s.as_str()));
    }
    if let Some(l) = &q.lang {
        wh.push("p.lang = ?");
        params.push(text(l));
    }
    if let Some(a) = q.author {
        wh.push("p.author_id = ?");
        params.push(int(a.0)?);
    }
    if let Some(t) = q.tag {
        wh.push("EXISTS (SELECT 1 FROM post_tags pt WHERE pt.post_id = p.id AND pt.tag_id = ?)");
        params.push(int(t.0)?);
    }
    if let Some(x) = q.series {
        wh.push("p.series_id = ?");
        params.push(int(x.0)?);
    }
    if let Some(b) = q.pinned {
        wh.push("p.pinned = ?");
        params.push(flag(b));
    }
    let order = match q.order {
        PostOrder::PublishedDesc => {
            wh.push("p.published_at IS NOT NULL");
            "p.published_at DESC, p.id DESC"
        }
        PostOrder::UpdatedDesc => "p.updated_at DESC, p.id DESC",
        // NULL positions last, like the memory store.
        PostOrder::SeriesAsc => "p.series_position IS NULL, p.series_position, p.id",
    };
    if let Some(PostCursor { key, id }) = q.after {
        match (q.order, key) {
            (PostOrder::PublishedDesc, Some(k)) => {
                wh.push("(p.published_at, p.id) < (?, ?)");
                params.extend([bound(k), bound(id.0)]);
            }
            (PostOrder::UpdatedDesc, Some(k)) => {
                wh.push("(p.updated_at, p.id) < (?, ?)");
                params.extend([bound(k), bound(id.0)]);
            }
            (PostOrder::SeriesAsc, Some(k)) => {
                wh.push(
                    "(p.series_position IS NULL OR p.series_position > ? \
                     OR (p.series_position = ? AND p.id > ?))",
                );
                params.extend([bound(k), bound(k), bound(id.0)]);
            }
            (PostOrder::SeriesAsc, None) => {
                wh.push("(p.series_position IS NULL AND p.id > ?)");
                params.push(bound(id.0));
            }
            (order, None) => {
                return Err(invalid(format!(
                    "cursor after post {} has no sort value for {order:?}",
                    id.0
                )));
            }
        }
    }
    let where_sql = if wh.is_empty() {
        String::new()
    } else {
        format!("WHERE {}", wh.join(" AND "))
    };
    params.push(limit(q.limit));
    Ok((format!("{where_sql} ORDER BY {order} LIMIT ?"), params))
}

/// Why a status change matched no row: the post is missing, or in the wrong state.
/// Nothing was written when this runs, so a read afterwards is safe.
async fn explain_miss(store: &D1Store, id: PostId, wrong_state: &str) -> Error {
    let exists = async {
        let s = store.stmt(
            "post_exists",
            "SELECT 1 FROM posts WHERE id = ?",
            &[bound(id.0)],
        )?;
        Ok::<bool, Error>(store.row(s).await?.is_some())
    };
    match exists.await {
        Ok(true) => invalid(format!("post {} {wrong_state}", id.0)),
        Ok(false) => post_not_found(id),
        Err(e) => e,
    }
}

impl PostStore for D1Store {
    async fn save_post(&self, post: SavePost) -> Result<PostId> {
        self.save_post_impl(post).await
    }

    // 1 query (post page by id): the post and its tag ids.
    async fn get_post(&self, id: PostId) -> Result<Post> {
        let s = self.stmt(
            "get_post",
            &format!("SELECT {POST_COLS} FROM posts p WHERE p.id = ?"),
            &[bound(id.0)],
        )?;
        match self.row(s).await? {
            Some(mut r) => read_post(&mut r),
            None => Err(post_not_found(id)),
        }
    }

    // 1 query (reader post page /<lang>/<slug>): the post and its tag ids.
    async fn find_post(&self, lang: &str, slug: &str) -> Result<Option<Post>> {
        let s = self.stmt(
            "find_post",
            &format!("SELECT {POST_COLS} FROM posts p WHERE p.lang = ? AND p.slug = ?"),
            &[text(lang), text(slug)],
        )?;
        self.row(s)
            .await?
            .map(|mut r| read_post(&mut r))
            .transpose()
    }

    // 1 query (translation switcher, hreflang).
    async fn list_translations(&self, group: PostId) -> Result<Vec<PostSummary>> {
        let s = self.stmt(
            "list_translations",
            &format!(
                "SELECT {SUMMARY_COLS} FROM posts p WHERE p.translation_group = ? \
                 ORDER BY p.lang, p.id"
            ),
            &[bound(group.0)],
        )?;
        self.rows(s).await?.iter_mut().map(read_summary).collect()
    }

    // 1 query (home, archives, tag/author/series pages, feeds, admin list).
    async fn list_posts(&self, query: &PostQuery) -> Result<Vec<PostSummary>> {
        let (tail, params) = listing_sql(query)?;
        let s = self.stmt(
            "list_posts",
            &format!("SELECT {SUMMARY_COLS} FROM posts p {tail}"),
            &params,
        )?;
        self.rows(s).await?.iter_mut().map(read_summary).collect()
    }

    // 1 query: a covering scan of `posts_listing` (see `reader_sql::post_counts`).
    async fn count_posts_by_status(
        &self,
        kind: Option<PostKind>,
        lang: Option<&str>,
    ) -> Result<PostCounts> {
        let mut params = Vec::new();
        if let Some(k) = kind {
            params.push(text(k.as_str()));
        }
        if let Some(l) = lang {
            params.push(text(l));
        }
        let s = self.stmt(
            "count_posts_by_status",
            &super::reader_sql::post_counts(kind.is_some(), lang.is_some()),
            &params,
        )?;
        let mut counts = PostCounts::default();
        for mut r in self.rows(s).await? {
            // An unknown status is an error, never a silently missing count.
            let status = r.code("post status", PostStatus::parse)?;
            counts.add(status, r.int()?);
        }
        Ok(counts)
    }

    async fn trash_post(&self, id: PostId, now: i64) -> Result<()> {
        // The bump reads the status inside the same transaction, before the update.
        let bump = bump_if(
            self,
            "EXISTS (SELECT 1 FROM posts WHERE id = ? AND status = 'published')",
            &[bound(id.0)],
        )?;
        // SQLite evaluates every SET expression on the old row, so trashed_from = old status.
        let up = self.stmt(
            "trash_post",
            "UPDATE posts SET trashed_from = status, status = 'trashed', updated_at = ?, \
             version = version + 1 WHERE id = ? AND status <> 'trashed'",
            &[int(now)?, bound(id.0)],
        )?;
        let results = self.batch("trash_post", vec![bump, up]).await?;
        if Self::changes("trash_post", &results[1])? == 0 {
            // Nothing was written; only the error remains to be told apart.
            return Err(explain_miss(self, id, "is already trashed").await);
        }
        Ok(())
    }

    async fn restore_post(&self, id: PostId, now: i64) -> Result<()> {
        let bump = bump_if(
            self,
            "EXISTS (SELECT 1 FROM posts WHERE id = ? AND status = 'trashed' \
             AND trashed_from = 'published')",
            &[bound(id.0)],
        )?;
        let up = self.stmt(
            "restore_post",
            "UPDATE posts SET status = trashed_from, trashed_from = NULL, updated_at = ?, \
             version = version + 1 WHERE id = ? AND status = 'trashed'",
            &[int(now)?, bound(id.0)],
        )?;
        // Before the update, while the condition still holds: a post that goes back to
        // published takes its path back from any redirect away from it.
        let take_path = self.stmt(
            "restore_post_take_path",
            "DELETE FROM redirects WHERE from_path = (SELECT '/' || lang || '/' || slug \
             FROM posts WHERE id = ? AND status = 'trashed' AND trashed_from = 'published')",
            &[bound(id.0)],
        )?;
        let results = self
            .batch("restore_post", vec![bump, take_path, up])
            .await?;
        if Self::changes("restore_post", &results[2])? == 0 {
            return Err(explain_miss(self, id, "is not trashed").await);
        }
        Ok(())
    }

    async fn delete_post(&self, id: PostId) -> Result<()> {
        // The search row goes only if the post goes: both carry the same condition, which
        // the first statement does not change. FK cascades remove tags, body media,
        // revisions and comments.
        let fts = self.stmt(
            "delete_post_fts",
            "DELETE FROM posts_fts WHERE rowid = ? \
             AND EXISTS (SELECT 1 FROM posts WHERE id = ? AND status = 'trashed')",
            &[bound(id.0), bound(id.0)],
        )?;
        let del = self.stmt(
            "delete_post",
            "DELETE FROM posts WHERE id = ? AND status = 'trashed'",
            &[bound(id.0)],
        )?;
        let results = self.batch("delete_post", vec![fts, del]).await?;
        if Self::changes("delete_post", &results[1])? == 0 {
            return Err(explain_miss(self, id, "must be trashed before it is deleted").await);
        }
        Ok(())
    }

    async fn publish_due(&self, now: i64, limit_n: u32) -> Result<Vec<PostId>> {
        // One batch; every statement selects the same due set (earliest `limit_n`), which
        // only the last one changes. Paths built in SQL match `/<lang>/<slug>` from save_post.
        let due = "id IN (SELECT id FROM posts WHERE status = 'scheduled' AND published_at <= ? \
                   ORDER BY published_at, id LIMIT ?)";
        let p = || [bound(now), bound(i64::from(limit_n))];
        let redirects = self.stmt(
            "publish_due_redirects",
            &format!(
                "DELETE FROM redirects WHERE from_path IN \
                 (SELECT '/' || lang || '/' || slug FROM posts WHERE {due})"
            ),
            &p(),
        )?;
        let bump = bump_if(
            self,
            &format!("EXISTS (SELECT 1 FROM posts WHERE {due})"),
            &p(),
        )?;
        let up = self.stmt(
            "publish_due",
            &format!(
                "UPDATE posts SET status = 'published', version = version + 1 WHERE {due} \
                 RETURNING id"
            ),
            &p(),
        )?;
        let results = self.batch("publish_due", vec![redirects, bump, up]).await?;
        let mut ids = Self::batch_ints("publish_due", &results[2])?
            .into_iter()
            .map(|row| {
                row.get("id")
                    .copied()
                    .map(PostId)
                    .ok_or_else(|| storage("publish_due", "RETURNING row without id"))
            })
            .collect::<Result<Vec<PostId>>>()?;
        ids.sort_unstable();
        Ok(ids)
    }

    // 1 query.
    async fn list_revisions(&self, post: PostId) -> Result<Vec<Revision>> {
        let s = self.stmt(
            "list_revisions",
            &format!(
                "SELECT {REVISION_COLS} FROM revisions WHERE post_id = ? \
                 ORDER BY created_at DESC, id DESC"
            ),
            &[bound(post.0)],
        )?;
        self.rows(s).await?.iter_mut().map(read_revision).collect()
    }

    // 1 query.
    async fn get_revision(&self, id: RevisionId) -> Result<Revision> {
        let s = self.stmt(
            "get_revision",
            &format!("SELECT {REVISION_COLS} FROM revisions WHERE id = ?"),
            &[bound(id.0)],
        )?;
        match self.row(s).await? {
            Some(mut r) => read_revision(&mut r),
            None => Err(not_found("revision", id.0)),
        }
    }

    // 1 query (reader search), 0 for a query without words.
    async fn search(&self, lang: &str, query: &str, limit_n: u32) -> Result<Vec<PostSummary>> {
        // User text never reaches MATCH raw: to_fts_query keeps only quoted word prefixes.
        let Some(fts) = search::to_fts_query(query) else {
            return Ok(Vec::new());
        };
        let s = self.stmt(
            "search",
            &format!(
                "SELECT {SUMMARY_COLS} FROM posts_fts JOIN posts p ON p.id = posts_fts.rowid \
                 WHERE posts_fts MATCH ? AND p.status = 'published' AND p.lang = ? \
                 ORDER BY p.published_at DESC, p.id DESC LIMIT ?"
            ),
            &[text(&fts), text(lang), limit(limit_n)],
        )?;
        self.rows(s).await?.iter_mut().map(read_summary).collect()
    }
}
