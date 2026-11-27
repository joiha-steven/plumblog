use std::cmp::Reverse;

use plumblog_model::{MediaId, Result};

use super::{MemoryStore, conflict, invalid, next_seq, not_found};
use crate::{Media, MediaQuery, MediaStore, NewMedia, PostStatus, page_len};

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
                },
            );
            Ok(id)
        })
    }

    async fn get_media(&self, id: MediaId) -> Result<Media> {
        self.read(|s| s.media.get(&id).cloned())
            .ok_or_else(|| not_found("media", id.0))
    }

    async fn list_media(&self, query: MediaQuery) -> Result<Vec<Media>> {
        Ok(self.read(|s| {
            let mut rows: Vec<&Media> = s
                .media
                .values()
                .filter(|m| !query.missing_alt_only || m.alt.is_empty())
                .filter(|m| query.before.is_none_or(|b| (m.created_at, m.id) < b))
                .collect();
            rows.sort_by_key(|m| Reverse((m.created_at, m.id)));
            rows.into_iter()
                .take(page_len(query.limit))
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

    async fn delete_media(&self, id: MediaId) -> Result<()> {
        self.write(|s| {
            s.media
                .remove(&id)
                .ok_or_else(|| not_found("media", id.0))?;
            let mut published = false;
            // posts.cover_media_id is ON DELETE SET NULL.
            for p in s
                .posts
                .values_mut()
                .filter(|p| p.cover_media_id == Some(id))
            {
                p.cover_media_id = None;
                published |= p.status == PostStatus::Published;
            }
            if published {
                s.bump();
            }
            Ok(())
        })
    }
}
