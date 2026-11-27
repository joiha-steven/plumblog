use plumblog_model::{MediaId, Result, UserId};
use plumblog_store::{Media, MediaQuery, MediaStore, NewMedia};

use super::{D1Store, Row, bound, bump_if, flag, int, limit, not_found, opt_int, text};

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
                 ORDER BY created_at DESC, id DESC LIMIT ?4"
            ),
            &[flag(q.missing_alt_only), at, id, limit(q.limit)],
        )?;
        self.rows(s).await?.iter_mut().map(read_media).collect()
    }

    async fn set_media_alt(&self, id: MediaId, alt: &str) -> Result<()> {
        let s = self.stmt(
            "set_media_alt",
            "UPDATE media SET alt = ? WHERE id = ?",
            &[text(alt), bound(id.0)],
        )?;
        let r = self.exec(s).await?;
        if Self::changes("set_media_alt", &r)? == 0 {
            return Err(not_found("media", id.0));
        }
        Ok(())
    }

    async fn delete_media(&self, id: MediaId) -> Result<()> {
        // Bump first, while the covers still point at it; the FK then sets them NULL.
        let bump = bump_if(
            self,
            "EXISTS (SELECT 1 FROM posts WHERE cover_media_id = ? AND status = 'published')",
            &[bound(id.0)],
        )?;
        let del = self.stmt(
            "delete_media",
            "DELETE FROM media WHERE id = ?",
            &[bound(id.0)],
        )?;
        let results = self.batch("delete_media", vec![bump, del]).await?;
        if Self::changes("delete_media", &results[1])? == 0 {
            return Err(not_found("media", id.0));
        }
        Ok(())
    }
}
