//! `save_post`: one batch per save, with every row the save touches.

use plumblog_model::{Error, ErrorCode, PostId, Result};
use plumblog_store::{PostStatus, PostStore, SavePost, Saved, search};
use worker::wasm_bindgen::JsValue;

use super::posts::post_not_found;
use super::redirects::put_redirect_stmts;
use super::{
    D1Store, Failure, GUARD_RETRIES, Stmt, bound, bump, changed_concurrently, flag, guard, int,
    invalid, opt_int, storage, text,
};

/// The `Conflict` of a save from a version that is no longer stored.
fn stale_version(id: PostId, current: i64, expected: i64) -> Error {
    Error::new(
        ErrorCode::Conflict,
        format!(
            "post {} is at version {current}, not {expected}; reload it and edit again",
            id.0
        ),
    )
}

/// Reader URL of a post; redirects are keyed by it.
fn post_path(lang: &str, slug: &str) -> String {
    format!("/{lang}/{slug}")
}

/// A new post's id for statements after its INSERT in the same batch, found by the unique
/// `(lang, slug)` the caller gave; bind [`new_post_key`] after it. (`sqlite_sequence` is not
/// read: D1 does not promise it.)
const NEW_POST_ID: &str = "(SELECT id FROM posts WHERE lang = ? AND slug = ?)";

/// The parameters of [`NEW_POST_ID`].
fn new_post_key(p: &SavePost) -> [JsValue; 2] {
    [text(&p.lang), text(&p.slug)]
}

/// `translation_group` of a new post that starts its own group, until the same batch sets it
/// to the new id. Ids start at 1, so no real group is 0.
const NO_GROUP_YET: i64 = 0;

/// What `save_post` must know about the row it replaces.
pub(super) struct Old {
    pub(super) version: i64,
    pub(super) status: PostStatus,
    pub(super) lang: String,
    pub(super) slug: String,
}

/// Columns a save writes, in INSERT order after `id` and `translation_group`.
fn post_values(p: &SavePost) -> Result<Vec<JsValue>> {
    Ok(vec![
        text(p.kind.as_str()),
        text(&p.lang),
        text(&p.slug),
        text(p.status.as_str()),
        opt_int(p.published_at)?,
        int(p.author_id.0)?,
        text(&p.title),
        text(&p.markdown),
        text(&p.html),
        text(&p.toc_json),
        text(&p.excerpt),
        int(p.word_count)?,
        int(p.reading_minutes)?,
        flag(p.pinned),
        flag(p.comments_enabled),
        opt_int(p.cover_media_id.map(|m| m.0))?,
        opt_int(p.series_id.map(|s| s.0))?,
        opt_int(p.series_position)?,
    ])
}

impl D1Store {
    pub(super) async fn save_post_impl(&self, input: SavePost) -> Result<Saved> {
        if input.status == PostStatus::Trashed {
            return Err(invalid("save_post cannot trash; use trash_post"));
        }
        if matches!(input.status, PostStatus::Scheduled | PostStatus::Published)
            && input.published_at.is_none()
        {
            return Err(invalid(format!(
                "a {} post needs published_at",
                input.status.as_str()
            )));
        }
        let (id, expected) = match (input.id, input.expected_version) {
            (None, None) => return self.insert_post(&input).await,
            (None, Some(_)) => return Err(invalid("a new post has no version to expect")),
            (Some(id), None) => {
                return Err(invalid(format!(
                    "updating post {} needs the version it edited",
                    id.0
                )));
            }
            (Some(id), Some(v)) => (id, v),
        };
        // Read, then write with a guard that the row is still what was read. If another
        // request changed it in between, its write moved the version, so the re-read
        // returns `Conflict` (or the error the row now calls for): a version mismatch is
        // never retried into success.
        for _ in 0..GUARD_RETRIES {
            let old = self.read_old(id).await?;
            if old.version != expected {
                // The same save again (its answer was lost): nothing to write. One more
                // query, only on this path.
                if input.matches(&self.get_post(id).await?) {
                    return Ok(Saved { id, wrote: false });
                }
                return Err(stale_version(id, old.version, expected));
            }
            match self.update_post(&input, id, &old).await {
                Ok(()) => return Ok(Saved { id, wrote: true }),
                Err(Failure::Guard) => {}
                Err(Failure::Error(e)) => return Err(e),
            }
        }
        Err(changed_concurrently("save_post"))
    }

    // 1 query.
    async fn read_old(&self, id: PostId) -> Result<Old> {
        let s = self.stmt(
            "save_post_old",
            "SELECT version, status, lang, slug FROM posts WHERE id = ?",
            &[bound(id.0)],
        )?;
        let mut r = self.row(s).await?.ok_or_else(|| post_not_found(id))?;
        let old = Old {
            version: r.int()?,
            status: r.code("post status", PostStatus::parse)?,
            lang: r.text()?,
            slug: r.text()?,
        };
        if old.status == PostStatus::Trashed {
            return Err(invalid(format!(
                "post {} is trashed; restore it first",
                id.0
            )));
        }
        Ok(old)
    }

    async fn insert_post(&self, p: &SavePost) -> Result<Saved> {
        let mut stmts = Vec::new();
        if let Some(g) = p.translation_group {
            // A group is the id of its first post; it exists while any post has it.
            stmts.push(guard(
                self,
                "save_post_group",
                "EXISTS (SELECT 1 FROM posts WHERE translation_group = ?)",
                &[bound(g.0)],
            )?);
        }
        let insert_at = stmts.len();
        let mut params = vec![
            opt_int(p.translation_group.map(|g| g.0))?,
            int(NO_GROUP_YET)?,
        ];
        params.extend(post_values(p)?);
        params.extend([int(p.now)?, int(p.now)?]);
        params.push(p.client_id.as_deref().map_or(JsValue::NULL, text));
        stmts.push(self.stmt(
            "save_post_insert",
            "INSERT INTO posts (translation_group, kind, lang, slug, status, trashed_from, \
             published_at, author_id, title, markdown, html, toc_json, excerpt, word_count, \
             reading_minutes, pinned, comments_enabled, cover_media_id, series_id, \
             series_position, created_at, updated_at, client_id) \
             VALUES (COALESCE(?, ?), ?, ?, ?, ?, NULL, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, \
             ?, ?, ?) RETURNING id",
            &params,
        )?);
        if p.translation_group.is_none() {
            // A new group is the new post's own id, known only after the INSERT.
            let mut params = vec![int(NO_GROUP_YET)?];
            params.extend(new_post_key(p));
            stmts.push(self.stmt(
                "save_post_new_group",
                "UPDATE posts SET translation_group = id \
                 WHERE translation_group = ? AND lang = ? AND slug = ?",
                &params,
            )?);
        }
        self.linked_rows(&mut stmts, p, NEW_POST_ID, &new_post_key(p))?;
        if p.status == PostStatus::Published {
            stmts.push(bump(self)?);
        }
        match self.batch_guarded("save_post", stmts).await {
            Ok(results) => Self::batch_ints("save_post_insert", &results[insert_at])?
                .first()
                .and_then(|row| row.get("id").copied())
                .map(|id| Saved {
                    id: PostId(id),
                    wrote: true,
                })
                .ok_or_else(|| storage("save_post_insert", "RETURNING gave no id")),
            Err(Failure::Guard) => Err(invalid(format!(
                "translation group {} does not exist",
                p.translation_group.map_or(0, |g| g.0)
            ))),
            Err(Failure::Error(e)) => self.replayed(p, e).await,
        }
    }

    /// A refused insert that carries a client id: if that id is stored (an earlier request,
    /// or one racing this one, won the `posts_author_client` index, or took the slug first
    /// with the same id), it is that post, not written by this save; otherwise the original
    /// error. The store reads only after a refused insert (the API's create handler looks the
    /// id up first anyway, so a race is what normally lands here).
    async fn replayed(&self, p: &SavePost, e: Error) -> Result<Saved> {
        let Some(key) = p
            .client_id
            .as_deref()
            .filter(|_| e.code == ErrorCode::Conflict)
        else {
            return Err(e);
        };
        match self.find_client_post(p.author_id, key).await? {
            Some(id) => Ok(Saved { id, wrote: false }),
            None => Err(e),
        }
    }

    pub(super) async fn update_post(
        &self,
        p: &SavePost,
        id: PostId,
        old: &Old,
    ) -> std::result::Result<(), Failure> {
        let mut stmts = vec![guard(
            self,
            "save_post_unchanged",
            "EXISTS (SELECT 1 FROM posts WHERE id = ? AND version = ? AND status = ? \
             AND lang = ? AND slug = ?)",
            &[
                bound(id.0),
                bound(old.version),
                text(old.status.as_str()),
                text(&old.lang),
                text(&old.slug),
            ],
        )?];
        let mut params = post_values(p)?;
        params.extend([int(p.now)?, bound(id.0), bound(old.version)]);
        stmts.push(self.stmt(
            "save_post_update",
            "UPDATE posts SET kind = ?, lang = ?, slug = ?, status = ?, trashed_from = NULL, \
             published_at = ?, author_id = ?, title = ?, markdown = ?, html = ?, toc_json = ?, \
             excerpt = ?, word_count = ?, reading_minutes = ?, pinned = ?, \
             comments_enabled = ?, cover_media_id = ?, series_id = ?, series_position = ?, \
             updated_at = ?, version = version + 1 WHERE id = ? AND version = ?",
            &params,
        )?);
        stmts.push(self.stmt(
            "save_post_untag",
            "DELETE FROM post_tags WHERE post_id = ?",
            &[bound(id.0)],
        )?);
        stmts.push(self.stmt(
            "save_post_unmedia",
            "DELETE FROM post_media WHERE post_id = ?",
            &[bound(id.0)],
        )?);
        stmts.push(self.stmt(
            "save_post_unindex",
            "DELETE FROM posts_fts WHERE rowid = ?",
            &[bound(id.0)],
        )?);
        self.linked_rows(&mut stmts, p, "?", &[bound(id.0)])?;
        let was_published = old.status == PostStatus::Published;
        let old_path = post_path(&old.lang, &old.slug);
        let new_path = post_path(&p.lang, &p.slug);
        if was_published && old_path != new_path {
            stmts.extend(put_redirect_stmts(self, &old_path, &new_path, p.now)?);
        }
        if was_published || p.status == PostStatus::Published {
            stmts.push(bump(self)?);
        }
        self.batch_guarded("save_post", stmts).await.map(drop)
    }

    /// Tags, body media, search row, revision and the redirect from the new path. `id_sql`
    /// is the post id as SQL (a parameter or [`NEW_POST_ID`]) with its parameters.
    fn linked_rows(
        &self,
        stmts: &mut Vec<Stmt>,
        p: &SavePost,
        id_sql: &str,
        id_params: &[JsValue],
    ) -> Result<()> {
        if !p.tag_ids.is_empty() {
            // One JSON array parameter, so a post with many tags stays under D1's
            // parameter limit; the ids are numbers we format, never user text.
            let list: Vec<String> = p.tag_ids.iter().map(|t| t.0.to_string()).collect();
            let mut params = id_params.to_vec();
            params.push(text(&format!("[{}]", list.join(","))));
            stmts.push(self.stmt(
                "save_post_tags",
                &format!(
                    "INSERT INTO post_tags (post_id, tag_id) \
                     SELECT DISTINCT {id_sql}, value FROM json_each(?)"
                ),
                &params,
            )?);
        }
        if !p.media_ids.is_empty() {
            // Same shape as the tags; an unknown id fails the media FK (InvalidInput).
            let list: Vec<String> = p.media_ids.iter().map(|m| m.0.to_string()).collect();
            let mut params = id_params.to_vec();
            params.push(text(&format!("[{}]", list.join(","))));
            stmts.push(self.stmt(
                "save_post_media",
                &format!(
                    "INSERT INTO post_media (post_id, media_id) \
                     SELECT DISTINCT {id_sql}, value FROM json_each(?)"
                ),
                &params,
            )?);
        }
        let mut params = id_params.to_vec();
        params.extend([
            text(&search::fold(&p.title)),
            text(&search::fold(&p.search_text)),
        ]);
        stmts.push(self.stmt(
            "save_post_index",
            &format!("INSERT INTO posts_fts (rowid, title, body) VALUES ({id_sql}, ?, ?)"),
            &params,
        )?);
        if let Some(r) = p.revision {
            let mut params = id_params.to_vec();
            params.extend([
                opt_int(r.author_id.map(|u| u.0))?,
                text(&p.title),
                text(&p.markdown),
                int(r.created_at)?,
            ]);
            stmts.push(self.stmt(
                "save_post_revision",
                &format!(
                    "INSERT INTO revisions (post_id, author_id, title, markdown, created_at) \
                     VALUES ({id_sql}, ?, ?, ?, ?)"
                ),
                &params,
            )?);
        }
        if p.status == PostStatus::Published {
            // A live path must not redirect away; a draft keeps the old URL working.
            stmts.push(self.stmt(
                "save_post_take_path",
                "DELETE FROM redirects WHERE from_path = ?",
                &[text(&post_path(&p.lang, &p.slug))],
            )?);
        }
        Ok(())
    }
}
