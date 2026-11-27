use std::collections::BTreeSet;

use plumblog_model::{CommentId, PostId, Result};

use super::{MemoryStore, invalid, next_seq, not_found};
use crate::{Comment, CommentStatus, CommentStore, NewComment, page_len};

#[expect(
    clippy::unused_async_trait_impl,
    reason = "in-memory stand-in for async D1 storage"
)]
impl CommentStore for MemoryStore {
    async fn add_comment(&self, comment: NewComment) -> Result<CommentId> {
        self.write(|s| {
            if !s.posts.contains_key(&comment.post_id) {
                return Err(invalid(format!(
                    "post {} does not exist",
                    comment.post_id.0
                )));
            }
            if let Some(parent) = comment.parent_id
                && s.comments
                    .get(&parent)
                    .is_none_or(|c| c.post_id != comment.post_id)
            {
                return Err(invalid(format!(
                    "parent comment {} is not on post {}",
                    parent.0, comment.post_id.0
                )));
            }
            let id = CommentId(next_seq(&mut s.seq.comments));
            if comment.status == CommentStatus::Approved {
                s.bump();
            }
            s.comments.insert(
                id,
                Comment {
                    id,
                    post_id: comment.post_id,
                    parent_id: comment.parent_id,
                    author_name: comment.author_name,
                    body: comment.body,
                    status: comment.status,
                    created_at: comment.created_at,
                },
            );
            Ok(id)
        })
    }

    async fn list_comments(&self, post: PostId, status: CommentStatus) -> Result<Vec<Comment>> {
        Ok(self.read(|s| {
            let mut rows: Vec<Comment> = s
                .comments
                .values()
                .filter(|c| c.post_id == post && c.status == status)
                .cloned()
                .collect();
            rows.sort_by_key(|c| (c.created_at, c.id));
            rows
        }))
    }

    async fn list_comments_by_status(
        &self,
        status: CommentStatus,
        after: Option<(i64, CommentId)>,
        limit: u32,
    ) -> Result<Vec<Comment>> {
        Ok(self.read(|s| {
            let mut rows: Vec<&Comment> = s
                .comments
                .values()
                .filter(|c| c.status == status)
                .filter(|c| after.is_none_or(|a| (c.created_at, c.id) > a))
                .collect();
            rows.sort_by_key(|c| (c.created_at, c.id));
            rows.into_iter().take(page_len(limit)).cloned().collect()
        }))
    }

    async fn set_comment_status(&self, id: CommentId, status: CommentStatus) -> Result<()> {
        self.write(|s| {
            let c = s
                .comments
                .get_mut(&id)
                .ok_or_else(|| not_found("comment", id.0))?;
            let visible = c.status == CommentStatus::Approved || status == CommentStatus::Approved;
            c.status = status;
            if visible {
                s.bump();
            }
            Ok(())
        })
    }

    async fn delete_comment(&self, id: CommentId) -> Result<()> {
        self.write(|s| {
            if !s.comments.contains_key(&id) {
                return Err(not_found("comment", id.0));
            }
            // parent_id is ON DELETE CASCADE, so replies at any depth go too.
            let mut doomed = BTreeSet::from([id]);
            loop {
                let more: Vec<CommentId> = s
                    .comments
                    .values()
                    .filter(|c| !doomed.contains(&c.id))
                    .filter(|c| c.parent_id.is_some_and(|p| doomed.contains(&p)))
                    .map(|c| c.id)
                    .collect();
                if more.is_empty() {
                    break;
                }
                doomed.extend(more);
            }
            let approved = doomed
                .iter()
                .any(|d| s.comments[d].status == CommentStatus::Approved);
            s.comments.retain(|k, _| !doomed.contains(k));
            if approved {
                s.bump();
            }
            Ok(())
        })
    }
}
