use plumblog_model::{CommentId, PostId, Result};
use plumblog_store::{Comment, CommentStatus, CommentStore, NewComment};
use worker::wasm_bindgen::JsValue;

use super::{
    D1Store, Failure, Row, bound, bump, bump_if, guard, int, invalid, limit, not_found, opt_int,
    opt_text, text,
};

pub(super) const COMMENT_COLS: &str =
    "id, post_id, parent_id, author_name, body, body_html, status, created_at";

fn read_comment(r: &mut Row) -> Result<Comment> {
    Ok(Comment {
        id: CommentId(r.int()?),
        post_id: PostId(r.int()?),
        parent_id: r.opt_int()?.map(CommentId),
        author_name: r.text()?,
        body: r.text()?,
        body_html: r.opt_text()?,
        status: r.code("comment status", CommentStatus::parse)?,
        created_at: r.int()?,
    })
}

impl CommentStore for D1Store {
    async fn add_comment(&self, c: NewComment) -> Result<CommentId> {
        let mut stmts = Vec::new();
        if let Some(parent) = c.parent_id {
            // The FK only says the parent exists; it must also be on the same post.
            stmts.push(guard(
                self,
                "add_comment_parent",
                "EXISTS (SELECT 1 FROM comments WHERE id = ? AND post_id = ?)",
                &[bound(parent.0), int(c.post_id.0)?],
            )?);
        }
        let insert_at = stmts.len();
        // A missing post fails the FK: InvalidInput.
        stmts.push(self.stmt(
            "add_comment",
            "INSERT INTO comments (post_id, parent_id, author_name, body, body_html, status, \
             created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
            &[
                int(c.post_id.0)?,
                opt_int(c.parent_id.map(|p| p.0))?,
                text(&c.author_name),
                text(&c.body),
                opt_text(c.body_html.as_deref()),
                text(c.status.as_str()),
                int(c.created_at)?,
            ],
        )?);
        if c.status == CommentStatus::Approved {
            stmts.push(bump(self)?);
        }
        match self.batch_guarded("add_comment", stmts).await {
            Ok(results) => Self::last_row_id("add_comment", &results[insert_at]).map(CommentId),
            Err(Failure::Guard) => Err(invalid(format!(
                "parent comment {} is not on post {}",
                c.parent_id.map_or(0, |p| p.0),
                c.post_id.0
            ))),
            Err(Failure::Error(e)) => Err(e),
        }
    }

    // 1 query.
    async fn get_comment(&self, id: CommentId) -> Result<Comment> {
        let s = self.stmt(
            "get_comment",
            &format!("SELECT {COMMENT_COLS} FROM comments WHERE id = ?"),
            &[bound(id.0)],
        )?;
        match self.row(s).await? {
            Some(mut r) => read_comment(&mut r),
            None => Err(not_found("comment", id.0)),
        }
    }

    // 1 query (post page: the newest approved comments). Reads one row past the page to
    // know whether more exist; comments_post serves it backwards.
    async fn latest_comments(
        &self,
        post: PostId,
        status: CommentStatus,
        limit_n: u32,
    ) -> Result<(Vec<Comment>, bool)> {
        let keep = plumblog_store::page_len(limit_n);
        let s = self.stmt(
            "latest_comments",
            &format!(
                "SELECT {COMMENT_COLS} FROM comments WHERE post_id = ? AND status = ? \
                 ORDER BY created_at DESC, id DESC LIMIT ?"
            ),
            &[
                bound(post.0),
                text(status.as_str()),
                bound(i64::try_from(keep).unwrap_or(0) + 1),
            ],
        )?;
        let mut rows = self
            .rows(s)
            .await?
            .iter_mut()
            .map(read_comment)
            .collect::<Result<Vec<_>>>()?;
        let more = rows.len() > keep;
        rows.truncate(keep);
        rows.reverse();
        Ok((rows, more))
    }

    // 1 query: a primary-key range (`reader_sql::comments_by_id`). Reads one row past the
    // page to know whether more exist.
    async fn comments_by_id(
        &self,
        after: Option<CommentId>,
        limit_n: u32,
    ) -> Result<(Vec<Comment>, bool)> {
        let keep = plumblog_store::page_len(limit_n);
        let s = self.stmt(
            "comments_by_id",
            &super::reader_sql::comments_by_id(),
            &[
                bound(after.map_or(0, |c| c.0)),
                bound(i64::try_from(keep).unwrap_or(0) + 1),
            ],
        )?;
        let mut rows = self
            .rows(s)
            .await?
            .iter_mut()
            .map(read_comment)
            .collect::<Result<Vec<_>>>()?;
        let more = rows.len() > keep;
        rows.truncate(keep);
        Ok((rows, more))
    }

    // 1 query.
    async fn list_comments(&self, post: PostId, status: CommentStatus) -> Result<Vec<Comment>> {
        let s = self.stmt(
            "list_comments",
            &format!(
                "SELECT {COMMENT_COLS} FROM comments WHERE post_id = ? AND status = ? \
                 ORDER BY created_at, id"
            ),
            &[bound(post.0), text(status.as_str())],
        )?;
        self.rows(s).await?.iter_mut().map(read_comment).collect()
    }

    // 1 query.
    async fn list_comments_by_status(
        &self,
        status: CommentStatus,
        after: Option<(i64, CommentId)>,
        limit_n: u32,
    ) -> Result<Vec<Comment>> {
        let (at, id) = match after {
            Some((at, id)) => (bound(at), bound(id.0)),
            None => (JsValue::NULL, JsValue::NULL),
        };
        let s = self.stmt(
            "list_comments_by_status",
            &format!(
                "SELECT {COMMENT_COLS} FROM comments \
                 WHERE status = ?1 AND (?2 IS NULL OR (created_at, id) > (?2, ?3)) \
                 ORDER BY created_at, id LIMIT ?4"
            ),
            &[text(status.as_str()), at, id, limit(limit_n)],
        )?;
        self.rows(s).await?.iter_mut().map(read_comment).collect()
    }

    async fn set_comment_status(&self, id: CommentId, status: CommentStatus) -> Result<()> {
        // Readers see the change if the old or the new status is approved; the old one is
        // read inside the batch, before the update.
        let bump = if status == CommentStatus::Approved {
            bump_if(
                self,
                "EXISTS (SELECT 1 FROM comments WHERE id = ?)",
                &[bound(id.0)],
            )?
        } else {
            bump_if(
                self,
                "EXISTS (SELECT 1 FROM comments WHERE id = ? AND status = 'approved')",
                &[bound(id.0)],
            )?
        };
        let up = self.stmt(
            "set_comment_status",
            "UPDATE comments SET status = ? WHERE id = ?",
            &[text(status.as_str()), bound(id.0)],
        )?;
        let results = self.batch("set_comment_status", vec![bump, up]).await?;
        if Self::changes("set_comment_status", &results[1])? == 0 {
            return Err(not_found("comment", id.0));
        }
        Ok(())
    }

    async fn delete_comment(&self, id: CommentId) -> Result<()> {
        // Bump if the comment or any reply below it is approved, checked before the delete;
        // the FK cascade on parent_id then removes the replies at any depth.
        let bump = bump_if(
            self,
            "EXISTS (WITH RECURSIVE doomed (id) AS (SELECT ? \
             UNION SELECT c.id FROM comments c JOIN doomed d ON c.parent_id = d.id) \
             SELECT 1 FROM comments WHERE id IN (SELECT id FROM doomed) AND status = 'approved')",
            &[bound(id.0)],
        )?;
        let del = self.stmt(
            "delete_comment",
            "DELETE FROM comments WHERE id = ?",
            &[bound(id.0)],
        )?;
        let results = self.batch("delete_comment", vec![bump, del]).await?;
        if Self::changes("delete_comment", &results[1])? == 0 {
            return Err(not_found("comment", id.0));
        }
        Ok(())
    }
}
