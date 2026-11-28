use std::cmp::Reverse;

use plumblog_model::{MediaId, PostId, Result, UserId};

use super::{MemoryStore, State, conflict, invalid, next_seq, not_found};
use crate::{Media, MediaQuery, MediaStore, NewMedia, PostStatus, probe_len};

#[expect(
    clippy::unused_async_trait_impl,
    reason = "in-memory stand-in for async D1 storage"
)]
impl MediaStore for MemoryStore {
    async fn create_media(&self, media: NewMedia) -> Result<MediaId> {
        self.write(|s| {
            // The schema's CHECKs.
            if media.bytes < 0 || media.width <= 0 || media.height <= 0 {
                return Err(invalid(format!(
                    "media sizes out of range: bytes {}, {}x{}",
                    media.bytes, media.width, media.height
                )));
            }
            if let Some(u) = media.uploaded_by
                && !s.user_exists(u)
            {
                return Err(invalid(format!("uploader {} does not exist", u.0)));
            }
            if s.media.values().any(|m| m.r2_key == media.r2_key) {
                return Err(conflict(format!(
                    "r2 key {:?} already exists",
                    media.r2_key
                )));
            }
            // UNIQUE (uploaded_by, sha256): NULL in either column never collides.
            if let (Some(u), Some(h)) = (media.uploaded_by, media.sha256.as_deref())
                && s.media
                    .values()
                    .any(|m| m.uploaded_by == Some(u) && m.sha256.as_deref() == Some(h))
            {
                return Err(
                    conflict(format!("uploader {} already has sha256 {h}", u.0)).at("sha256")
                );
            }
            let id = MediaId(next_seq(&mut s.seq.media));
            s.media.insert(
                id,
                Media {
                    id,
                    r2_key: media.r2_key,
                    mime: media.mime,
                    bytes: media.bytes,
                    width: media.width,
                    height: media.height,
                    alt: media.alt,
                    variants_json: media.variants_json,
                    uploaded_by: media.uploaded_by,
                    created_at: media.created_at,
                    sha256: media.sha256,
                },
            );
            Ok(id)
        })
    }

    async fn find_media_by_sha256(&self, uploader: UserId, sha256: &str) -> Result<Option<Media>> {
        Ok(self.read(|s| {
            s.media
                .values()
                .find(|m| m.uploaded_by == Some(uploader) && m.sha256.as_deref() == Some(sha256))
                .cloned()
        }))
    }

    async fn get_media(&self, id: MediaId) -> Result<Media> {
        self.read(|s| s.media.get(&id).cloned())
            .ok_or_else(|| not_found("media", id.0))
    }

    async fn public_media(&self, id: MediaId) -> Result<Option<Media>> {
        Ok(self.read(|s| {
            let live = |p: PostId| {
                s.posts
                    .get(&p)
                    .is_some_and(|p| p.status == PostStatus::Published)
            };
            s.media_usage(id)
                .into_iter()
                .any(live)
                .then(|| s.media.get(&id).cloned())
                .flatten()
        }))
    }

    async fn list_media(&self, query: MediaQuery) -> Result<Vec<Media>> {
        Ok(self.read(|s| {
            let mut rows: Vec<&Media> = s
                .media
                .values()
                .filter(|m| !query.missing_alt_only || m.alt.is_empty())
                .filter(|m| !query.unused_only || s.media_usage(m.id).is_empty())
                .filter(|m| query.before.is_none_or(|b| (m.created_at, m.id) < b))
                .collect();
            rows.sort_by_key(|m| Reverse((m.created_at, m.id)));
            rows.into_iter()
                .take(probe_len(query.limit))
                .cloned()
                .collect()
        }))
    }

    async fn set_media_alt(&self, id: MediaId, alt: &str) -> Result<()> {
        self.write(|s| {
            let m = s
                .media
                .get_mut(&id)
                .ok_or_else(|| not_found("media", id.0))?;
            alt.clone_into(&mut m.alt);
            s.bump();
            Ok(())
        })
    }

    async fn media_usage(&self, id: MediaId) -> Result<Vec<PostId>> {
        Ok(self.read(|s| s.media_usage(id)))
    }

    async fn delete_media(&self, id: MediaId) -> Result<()> {
        self.write(|s| {
            if !s.media.contains_key(&id) {
                return Err(not_found("media", id.0));
            }
            // post_media.media_id and posts.cover_media_id are ON DELETE RESTRICT.
            let users = s.media_usage(id);
            if !users.is_empty() {
                return Err(media_in_use(id, &users));
            }
            s.media.remove(&id);
            Ok(())
        })
    }
}

impl State {
    /// Posts using `id` in their body or as cover, ascending, each once.
    fn media_usage(&self, id: MediaId) -> Vec<PostId> {
        let mut ids: Vec<PostId> = self
            .post_media
            .iter()
            .filter(|(_, m)| *m == id)
            .map(|(p, _)| *p)
            .chain(
                self.posts
                    .values()
                    .filter(|p| p.cover_media_id == Some(id))
                    .map(|p| p.id),
            )
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }
}

/// The `Conflict` of deleting media that posts still use.
fn media_in_use(id: MediaId, users: &[PostId]) -> plumblog_model::Error {
    let list: Vec<String> = users.iter().map(|p| p.0.to_string()).collect();
    conflict(format!(
        "media {} is used by posts {}; remove it from them first",
        id.0,
        list.join(", ")
    ))
}
