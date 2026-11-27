//! `save_post`: one batch per save, with every row the save touches.

use plumblog_model::{PostId, Result};
use plumblog_store::{PostStatus, SavePost, search};
use worker::wasm_bindgen::JsValue;

use super::posts::post_not_found;
use super::redirects::put_redirect_stmts;
use super::{
    D1Store, Failure, GUARD_RETRIES, Stmt, bound, bump, changed_concurrently, flag, guard, int,
    invalid, opt_int, text,
};

/// Reader URL of a post; redirects are keyed by it.
fn post_path(lang: &str, slug: &str) -> String {
    format!("/{lang}/{slug}")
}

/// The id a new post gets, computed the way SQLite's AUTOINCREMENT does (largest id ever
/// handed out, or the largest present, plus one). Setting it explicitly lets one INSERT use
/// it as the new group too.
const NEXT_POST_ID: &str = "(SELECT MAX(COALESCE((SELECT seq FROM sqlite_sequence WHERE name = 'posts'), 0), \
     COALESCE((SELECT MAX(id) FROM posts), 0)) + 1)";

/// The new post's id for statements after its INSERT in the same batch.
const NEW_POST_ID: &str = "(SELECT seq FROM sqlite_sequence WHERE name = 'posts')";

/// What `save_post` must know about the row it replaces.
pub(super) struct Old {
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
    pub(super) async fn save_post_impl(&self, input: SavePost) -> Result<PostId> {
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
        let Some(id) = input.id else {
            return self.insert_post(&input).await;
        };
        // Read, then write with a guard that the row is still what was read; if another
        // request changed it in between, read again so redirects and bumps follow the
        // row that is really there.
        for _ in 0..GUARD_RETRIES {
            let old = self.read_old(id).await?;
            match self.update_post(&input, id, &old).await {
                Ok(()) => return Ok(id),
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
            "SELECT status, lang, slug FROM posts WHERE id = ?",
            &[bound(id.0)],
        )?;
        let mut r = self.row(s).await?.ok_or_else(|| post_not_found(id))?;
        let old = Old {
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

    async fn insert_post(&self, p: &SavePost) -> Result<PostId> {
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
        let mut params = vec![opt_int(p.translation_group.map(|g| g.0))?];
        params.extend(post_values(p)?);
        params.extend([int(p.now)?, int(p.now)?]);
        stmts.push(self.stmt(
            "save_post_insert",
            &format!(
                "INSERT INTO posts (id, translation_group, kind, lang, slug, status, \
                 trashed_from, published_at, author_id, title, markdown, html, toc_json, \
                 excerpt, word_count, reading_minutes, pinned, comments_enabled, \
                 cover_media_id, series_id, series_position, created_at, updated_at) \
                 SELECT n, COALESCE(?, n), ?, ?, ?, ?, NULL, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, \
                 ?, ?, ?, ?, ? FROM (SELECT {NEXT_POST_ID} AS n)"
            ),
            &params,
        )?);
        self.linked_rows(&mut stmts, p, NEW_POST_ID, &[])?;
        if p.status == PostStatus::Published {
            stmts.push(bump(self)?);
        }
        match self.batch_guarded("save_post", stmts).await {
            Ok(results) => Self::last_row_id("save_post_insert", &results[insert_at]).map(PostId),
            Err(Failure::Guard) => Err(invalid(format!(
                "translation group {} does not exist",
                p.translation_group.map_or(0, |g| g.0)
            ))),
            Err(Failure::Error(e)) => Err(e),
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
            "EXISTS (SELECT 1 FROM posts WHERE id = ? AND status = ? AND lang = ? AND slug = ?)",
            &[
                bound(id.0),
                text(old.status.as_str()),
                text(&old.lang),
                text(&old.slug),
            ],
        )?];
        let mut params = post_values(p)?;
        params.extend([int(p.now)?, bound(id.0)]);
        stmts.push(self.stmt(
            "save_post_update",
            "UPDATE posts SET kind = ?, lang = ?, slug = ?, status = ?, trashed_from = NULL, \
             published_at = ?, author_id = ?, title = ?, markdown = ?, html = ?, toc_json = ?, \
             excerpt = ?, word_count = ?, reading_minutes = ?, pinned = ?, \
             comments_enabled = ?, cover_media_id = ?, series_id = ?, series_position = ?, \
             updated_at = ? WHERE id = ?",
            &params,
        )?);
        stmts.push(self.stmt(
            "save_post_untag",
            "DELETE FROM post_tags WHERE post_id = ?",
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

    /// Tags, search row, revision and the redirect from the new path. `id_sql` is the
    /// post id as SQL (a parameter or [`NEW_POST_ID`]) with its parameters.
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
