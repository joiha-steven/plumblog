use plumblog_model::{Error, ErrorCode, MediaId, PostId, Result, UserId};
use plumblog_store::{Media, MediaQuery, MediaStore, NewMedia};

use super::{
    D1Store, Failure, GUARD_RETRIES, Row, bound, bump_if, changed_concurrently, flag, guard, int,
    not_found, opt_int, probe_limit, text,
};

const MEDIA_COLS: &str =
    "id, r2_key, mime, bytes, width, height, alt, variants_json, uploaded_by, created_at";

fn read_media(r: &mut Row) -> Result<Media> {
    Ok(Media {
        id: MediaId(r.int()?),
        r2_key: r.text()?,
        mime: r.text()?,
        bytes: r.int()?,
        width: r.int()?,
        height: r.int()?,
        alt: r.text()?,
        variants_json: r.text()?,
        uploaded_by: r.opt_int()?.map(UserId),
        created_at: r.int()?,
    })
}

impl MediaStore for D1Store {
    async fn create_media(&self, media: NewMedia) -> Result<MediaId> {
        // The schema's CHECKs reject bad sizes (InvalidInput), its FK a missing uploader.
        let s = self.stmt(
            "create_media",
            "INSERT INTO media (r2_key, mime, bytes, width, height, alt, variants_json, \
             uploaded_by, created_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            &[
                text(&media.r2_key),
                text(&media.mime),
                int(media.bytes)?,
                int(media.width)?,
                int(media.height)?,
                text(&media.alt),
                text(&media.variants_json),
                opt_int(media.uploaded_by.map(|u| u.0))?,
                int(media.created_at)?,
            ],
        )?;
        let r = self.exec(s).await?;
        Self::last_row_id("create_media", &r).map(MediaId)
    }

    // 1 query.
    async fn get_media(&self, id: MediaId) -> Result<Media> {
        let s = self.stmt(
            "get_media",
            &format!("SELECT {MEDIA_COLS} FROM media WHERE id = ?"),
            &[bound(id.0)],
        )?;
        match self.row(s).await? {
            Some(mut r) => read_media(&mut r),
            None => Err(not_found("media", id.0)),
        }
    }

    // 1 query: the row and the reader test together, by primary key, `post_media_media` and
    // `posts_cover`.
    async fn public_media(&self, id: MediaId) -> Result<Option<Media>> {
        let s = self.stmt(
            "public_media",
            &format!(
                "SELECT {MEDIA_COLS} FROM media WHERE id = ?1 AND (\
                 EXISTS (SELECT 1 FROM post_media pm JOIN posts p ON p.id = pm.post_id \
                 WHERE pm.media_id = ?1 AND p.status = 'published') \
                 OR EXISTS (SELECT 1 FROM posts WHERE cover_media_id = ?1 \
                 AND status = 'published'))"
            ),
            &[bound(id.0)],
        )?;
        self.row(s)
            .await?
            .map(|mut r| read_media(&mut r))
            .transpose()
    }

    // 1 query.
    async fn list_media(&self, q: MediaQuery) -> Result<Vec<Media>> {
        let (at, id) = match q.before {
            Some((at, id)) => (bound(at), bound(id.0)),
            None => (
                worker::wasm_bindgen::JsValue::NULL,
                worker::wasm_bindgen::JsValue::NULL,
            ),
        };
        let s = self.stmt(
            "list_media",
            &format!(
                "SELECT {MEDIA_COLS} FROM media \
                 WHERE (?1 = 0 OR alt = '') AND (?2 IS NULL OR (created_at, id) < (?2, ?3)) \
                 AND (?5 = 0 OR NOT ({IN_USE})) \
                 ORDER BY created_at DESC, id DESC LIMIT ?4"
            ),
            &[
                flag(q.missing_alt_only),
                at,
                id,
                probe_limit(q.limit),
                flag(q.unused_only),
            ],
        )?;
        self.rows(s).await?.iter_mut().map(read_media).collect()
    }

    async fn set_media_alt(&self, id: MediaId, alt: &str) -> Result<()> {
        let s = self.stmt(
            "set_media_alt",
            "UPDATE media SET alt = ? WHERE id = ?",
            &[text(alt), bound(id.0)],
        )?;
        let bump = bump_if(
            self,
            "EXISTS (SELECT 1 FROM media WHERE id = ?)",
            &[bound(id.0)],
        )?;
        let results = self.batch("set_media_alt", vec![bump, s]).await?;
        if Self::changes("set_media_alt", &results[1])? == 0 {
            return Err(not_found("media", id.0));
        }
        Ok(())
    }

    // 1 query (media details "used in").
    async fn media_usage(&self, id: MediaId) -> Result<Vec<PostId>> {
        let s = self.stmt(
            "media_usage",
            "SELECT post_id FROM post_media WHERE media_id = ?1 \
             UNION SELECT id FROM posts WHERE cover_media_id = ?1 ORDER BY 1",
            &[bound(id.0)],
        )?;
        self.rows(s)
            .await?
            .iter_mut()
            .map(|r| r.int().map(PostId))
            .collect()
    }

    async fn delete_media(&self, id: MediaId) -> Result<()> {
        // The schema's RESTRICT FKs would refuse too, but only as a bare FK error; the guard
        // lets us name the posts. Unused media is on no reader page: no bump.
        for _ in 0..GUARD_RETRIES {
            let unused = guard(
                self,
                "delete_media_unused",
                &format!("NOT ({})", IN_USE.replace("media.id", "?1")),
                &[bound(id.0)],
            )?;
            let del = self.stmt(
                "delete_media",
                "DELETE FROM media WHERE id = ?",
                &[bound(id.0)],
            )?;
            match self.batch_guarded("delete_media", vec![unused, del]).await {
                Ok(results) => {
                    if Self::changes("delete_media", &results[1])? == 0 {
                        return Err(not_found("media", id.0));
                    }
                    return Ok(());
                }
                Err(Failure::Guard) => {
                    let users = self.media_usage(id).await?;
                    if !users.is_empty() {
                        return Err(media_in_use(id, &users));
                    }
                    // Freed between the batch and the read: try again.
                }
                Err(Failure::Error(e)) => return Err(e),
            }
        }
        Err(changed_concurrently("delete_media"))
    }
}

/// SQL true while a post uses `media.id` in its body or as cover.
const IN_USE: &str = "EXISTS (SELECT 1 FROM post_media WHERE media_id = media.id) \
     OR EXISTS (SELECT 1 FROM posts WHERE cover_media_id = media.id)";

/// The `Conflict` of deleting media that posts still use.
fn media_in_use(id: MediaId, users: &[PostId]) -> Error {
    let list: Vec<String> = users.iter().map(|p| p.0.to_string()).collect();
    Error::new(
        ErrorCode::Conflict,
        format!(
            "media {} is used by posts {}; remove it from them first",
            id.0,
            list.join(", ")
        ),
    )
}
