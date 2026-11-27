use std::cmp::Reverse;

use plumblog_model::{PostId, Result, RevisionId};

use super::post_save::post_path;
use super::{MemoryStore, invalid, not_found, post_not_found};
use crate::{Post, PostQuery, PostStatus, PostStore, PostSummary, Revision, SavePost};

#[expect(
    clippy::unused_async_trait_impl,
    reason = "in-memory stand-in for async D1 storage"
)]
impl PostStore for MemoryStore {
    async fn save_post(&self, post: SavePost) -> Result<PostId> {
        self.write(|s| s.save_post(post))
    }

    async fn get_post(&self, id: PostId) -> Result<Post> {
        self.read(|s| s.posts.get(&id).cloned())
            .ok_or_else(|| post_not_found(id))
    }

    async fn find_post(&self, lang: &str, slug: &str) -> Result<Option<Post>> {
        Ok(self.read(|s| {
            s.posts
                .values()
                .find(|p| p.lang == lang && p.slug == slug)
                .cloned()
        }))
    }

    async fn list_translations(&self, group: PostId) -> Result<Vec<PostSummary>> {
        Ok(self.read(|s| {
            let mut rows: Vec<PostSummary> = s
                .posts
                .values()
                .filter(|p| p.translation_group == group)
                .map(PostSummary::from)
                .collect();
            rows.sort_by(|a, b| (&a.lang, a.id).cmp(&(&b.lang, b.id)));
            rows
        }))
    }

    async fn list_posts(&self, query: &PostQuery) -> Result<Vec<PostSummary>> {
        self.read(|s| s.list_posts(query))
    }

    async fn trash_post(&self, id: PostId, now: i64) -> Result<()> {
        self.write(|s| {
            let p = s.posts.get_mut(&id).ok_or_else(|| post_not_found(id))?;
            if p.status == PostStatus::Trashed {
                return Err(invalid(format!("post {} is already trashed", id.0)));
            }
            let was_published = p.status == PostStatus::Published;
            p.trashed_from = Some(p.status);
            p.status = PostStatus::Trashed;
            p.updated_at = now;
            p.version += 1;
            if was_published {
                s.bump();
            }
            Ok(())
        })
    }

    async fn restore_post(&self, id: PostId, now: i64) -> Result<()> {
        self.write(|s| {
            let p = s.posts.get_mut(&id).ok_or_else(|| post_not_found(id))?;
            let Some(to) = p.trashed_from.filter(|_| p.status == PostStatus::Trashed) else {
                return Err(invalid(format!("post {} is not trashed", id.0)));
            };
            p.status = to;
            p.trashed_from = None;
            p.updated_at = now;
            p.version += 1;
            if to == PostStatus::Published {
                // Live again: its path wins over a redirect away from it (see save_post).
                let path = post_path(&p.lang, &p.slug);
                s.redirects.remove(&path);
                s.bump();
            }
            Ok(())
        })
    }

    async fn delete_post(&self, id: PostId) -> Result<()> {
        self.write(|s| {
            let p = s.posts.get(&id).ok_or_else(|| post_not_found(id))?;
            if p.status != PostStatus::Trashed {
                return Err(invalid(format!(
                    "post {} must be trashed before it is deleted",
                    id.0
                )));
            }
            s.posts.remove(&id);
            s.fts.remove(&id);
            // ON DELETE CASCADE: post_tags (inside the post), post_media, revisions, comments.
            s.post_media.retain(|(p, _)| *p != id);
            s.revisions.retain(|_, r| r.post_id != id);
            s.comments.retain(|_, c| c.post_id != id);
            Ok(())
        })
    }

    async fn publish_due(&self, now: i64, limit: u32) -> Result<Vec<PostId>> {
        self.write(|s| {
            let mut due: Vec<(i64, PostId)> = s
                .posts
                .values()
                .filter(|p| p.status == PostStatus::Scheduled)
                .filter_map(|p| p.published_at.filter(|t| *t <= now).map(|t| (t, p.id)))
                .collect();
            due.sort_unstable();
            due.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
            let mut ids: Vec<PostId> = due.into_iter().map(|(_, id)| id).collect();
            ids.sort_unstable();
            for id in &ids {
                if let Some(p) = s.posts.get_mut(id) {
                    p.status = PostStatus::Published;
                    p.version += 1;
                }
            }
            // Each newly live path takes over from any redirect away from it (see save_post).
            for id in &ids {
                if let Some(p) = s.posts.get(id) {
                    let path = post_path(&p.lang, &p.slug);
                    s.redirects.remove(&path);
                }
            }
            if !ids.is_empty() {
                s.bump();
            }
            Ok(ids)
        })
    }

    async fn list_revisions(&self, post: PostId) -> Result<Vec<Revision>> {
        Ok(self.read(|s| {
            let mut rows: Vec<Revision> = s
                .revisions
                .values()
                .filter(|r| r.post_id == post)
                .cloned()
                .collect();
            rows.sort_by_key(|r| Reverse((r.created_at, r.id)));
            rows
        }))
    }

    async fn get_revision(&self, id: RevisionId) -> Result<Revision> {
        self.read(|s| s.revisions.get(&id).cloned())
            .ok_or_else(|| not_found("revision", id.0))
    }

    async fn search(&self, lang: &str, query: &str, limit: u32) -> Result<Vec<PostSummary>> {
        Ok(self.read(|s| s.search(lang, query, limit)))
    }
}
