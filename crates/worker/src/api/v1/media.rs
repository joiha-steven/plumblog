//! `/api/v1/media`: the library. An upload stores the original bytes in R2 (`MEDIA`) and a
//! row in D1; the AVIF sizes are made by the admin's encoder (issue #13), not here.
//!
//! R2 and D1 cannot share a transaction, so each write orders them to leave at worst an
//! unreferenced R2 object, never a row without its bytes: upload writes R2 first and removes
//! the object if the row fails; delete removes the row first and the object after. Any object
//! left behind is logged with its key.

use plumblog_auth::{Crypto, base64url};
use plumblog_model::api::{DEFAULT_LIMIT, MAX_LIMIT, MAX_MEDIA_BODY, MEDIA_MIMES};
use plumblog_model::{Error, ErrorCode, MediaId, Permission, PostId, Result, image};
use plumblog_store::{Media, MediaQuery, MediaStore, NewMedia};
use worker::{Bucket, HttpMetadata};

use super::{Input, Principal, changed, stored_json};
use crate::api::body::{Out, object};
use crate::api::{Ctx, Raw, Reply};

const MAX_ALT: usize = 1000;
/// Longest file name kept in the R2 key.
const MAX_NAME: usize = 100;
/// Largest side accepted, a sanity bound well past any camera.
const MAX_SIDE: i64 = 1 << 16;

fn invalid(field: &str, why: &str) -> Error {
    Error::new(ErrorCode::InvalidInput, format!("{field}: {why}")).at(field)
}

fn bucket(ctx: &Ctx) -> Result<Bucket> {
    ctx.env
        .bucket("MEDIA")
        .map_err(|e| Error::new(ErrorCode::Internal, format!("R2 binding MEDIA: {e}")))
}

fn alt(v: &str) -> Result<()> {
    if v.chars().count() > MAX_ALT {
        return Err(invalid("alt", "too long"));
    }
    if v.chars().any(char::is_control) {
        return Err(invalid("alt", "control character"));
    }
    Ok(())
}

/// The file name as it goes in the R2 key: lowercase ASCII letters, digits, `.`, `_` and
/// `-`, anything else one `-`; no leading or trailing `.`/`-` (no `..`, no hidden names).
fn key_name(name: &str) -> Result<String> {
    let mut out = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        let c = if c.is_ascii_alphanumeric() || matches!(c, '.' | '_') {
            c
        } else {
            '-'
        };
        let last = out.chars().last();
        if (c == '-' || c == '.') && matches!(last, Some('-' | '.')) {
            continue;
        }
        out.push(c);
    }
    out.truncate(MAX_NAME);
    let out = out.trim_matches(|c| c == '-' || c == '.').to_owned();
    if out.is_empty() {
        return Err(invalid("name", "no usable characters"));
    }
    Ok(out)
}

fn ids_json(ids: &[PostId]) -> Out {
    let items: Vec<String> = ids.iter().map(|i| i.0.to_string()).collect();
    Out::Raw(format!("[{}]", items.join(",")))
}

fn media_json(ctx: &Ctx, m: &Media, usage: Option<&[PostId]>) -> String {
    let name = m.r2_key.rsplit('/').next().unwrap_or_default();
    let mut fields = vec![
        ("id", Out::Int(m.id.0)),
        ("src", Out::Str(format!("/media/{}/{name}", m.id.0))),
        ("mime", Out::Str(m.mime.clone())),
        ("bytes", Out::Int(m.bytes)),
        ("width", Out::Int(m.width)),
        ("height", Out::Int(m.height)),
        ("alt", Out::Str(m.alt.clone())),
        (
            "variants",
            stored_json(ctx, "media.variants_json", &m.variants_json),
        ),
        (
            "uploaded_by",
            m.uploaded_by.map_or(Out::Null, |u| Out::Int(u.0)),
        ),
        ("created_at", Out::Int(m.created_at)),
    ];
    if let Some(ids) = usage {
        fields.push(("usage", ids_json(ids)));
    }
    object(&fields)
}

fn decode_cursor(text: &str) -> Result<(i64, MediaId)> {
    let bad = || invalid("cursor", "not a cursor of this listing");
    let raw = String::from_utf8(base64url::decode(text).ok_or_else(bad)?).map_err(|_| bad())?;
    let (at, id) = raw.split_once('.').ok_or_else(bad)?;
    let at: i64 = at.parse().map_err(|_| bad())?;
    let id: i64 = id.parse().map_err(|_| bad())?;
    if id <= 0 {
        return Err(bad());
    }
    Ok((at, MediaId(id)))
}

/// `GET /api/v1/media`: newest first, keyset-paged; `missing_alt` and `unused` filter.
pub async fn list(ctx: &Ctx, input: &Input) -> Result<Reply> {
    let limit = input.int("limit").unwrap_or(i64::from(DEFAULT_LIMIT));
    let limit = u32::try_from(limit)
        .ok()
        .filter(|l| (1..=MAX_LIMIT).contains(l))
        .ok_or_else(|| invalid("limit", "1-100"))?;
    let q = MediaQuery {
        before: input.str("cursor").map(decode_cursor).transpose()?,
        missing_alt_only: input.bool("missing_alt").unwrap_or(false),
        unused_only: input.bool("unused").unwrap_or(false),
        // One more row than asked says whether another page exists.
        limit: limit + 1,
    };
    let mut rows = ctx.store.list_media(q).await?;
    let more = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    let next = match rows.last() {
        Some(m) if more => Out::Str(base64url::encode(
            format!("{}.{}", m.created_at, m.id.0).as_bytes(),
        )),
        _ => Out::Null,
    };
    let items: Vec<String> = rows.iter().map(|m| media_json(ctx, m, None)).collect();
    Ok(Reply::json(
        200,
        object(&[
            ("media", Out::Raw(format!("[{}]", items.join(",")))),
            ("next_cursor", next),
        ]),
    ))
}

/// `GET /api/v1/media/<id>`, with the posts that use it.
pub async fn get(ctx: &Ctx, input: &Input) -> Result<Reply> {
    let m = ctx.store.get_media(MediaId(input.id())).await?;
    let usage = ctx.store.media_usage(m.id).await?;
    Ok(Reply::json(200, media_json(ctx, &m, Some(&usage))))
}

/// `GET /api/v1/media/<id>/original`: the stored bytes, streamed from R2 to the client by the
/// runtime. An object whose size is not the row's, or over the upload cap, is refused: it was
/// not written by this API, so it is not served as if it were.
pub async fn original(ctx: &Ctx, input: &Input) -> Result<Reply> {
    let m = ctx.store.get_media(MediaId(input.id())).await?;
    let rid = ctx.request_id.as_str();
    // Uploads only accept raster types; a row holding anything else (SVG, HTML) was written
    // around the API and must never be served from the site's origin.
    if !MEDIA_MIMES.contains(&m.mime.as_str()) {
        return Err(Error::new(
            ErrorCode::Storage,
            format!(
                "media {}: stored mime {:?} is not an accepted image type",
                m.id.0, m.mime
            ),
        ));
    }
    let obj = bucket(ctx)?
        .get(m.r2_key.clone())
        .execute()
        .await
        .map_err(|e| Error::new(ErrorCode::Storage, format!("R2 get {}: {e}", m.r2_key)))?;
    let Some(obj) = obj else {
        // The row points at bytes that are gone (removed outside the API): log the key so
        // the next agent can find the object.
        worker::console_error!(
            "[{rid}] media {}: R2 object {} is missing",
            m.id.0,
            m.r2_key
        );
        return Err(Error::new(
            ErrorCode::NotFound,
            format!("media {} has no R2 object", m.id.0),
        ));
    };
    let size = obj.size();
    let cap = u64::try_from(MAX_MEDIA_BODY).unwrap_or(u64::MAX);
    if u64::try_from(m.bytes).ok() != Some(size) || size > cap {
        return Err(Error::new(
            ErrorCode::Storage,
            format!(
                "media {}: R2 object {} is {size} bytes, row says {} (cap {cap})",
                m.id.0, m.r2_key, m.bytes
            ),
        ));
    }
    let body = obj
        .body()
        .ok_or_else(|| {
            Error::new(
                ErrorCode::Storage,
                format!("media {}: R2 object {} has no body", m.id.0, m.r2_key),
            )
        })?
        .response_body()
        .map_err(|e| Error::new(ErrorCode::Storage, format!("R2 body {}: {e}", m.r2_key)))?;
    Ok(Reply::raw(Raw {
        body,
        mime: m.mime,
        length: size,
    }))
}

/// `POST /api/v1/media`: the body is the original image; `mime` must be what its magic bytes
/// say and `width` x `height` what its header declares.
pub async fn upload(ctx: &Ctx, p: &Principal, input: &Input, body: Vec<u8>) -> Result<Reply> {
    let user = p.require(Permission::UploadMedia)?;
    let mime = input.str("mime").unwrap_or_default();
    if !MEDIA_MIMES.contains(&mime) {
        return Err(invalid("mime", "not an accepted image type"));
    }
    let width = input.int("width").unwrap_or(0);
    let height = input.int("height").unwrap_or(0);
    for (field, v) in [("width", width), ("height", height)] {
        if !(1..=MAX_SIDE).contains(&v) {
            return Err(invalid(field, "1-65536"));
        }
    }
    let alt_text = input.str("alt").unwrap_or_default();
    alt(alt_text)?;
    let name = key_name(input.str("name").unwrap_or_default())?;
    if image::sniff(&body) != Some(mime) {
        return Err(invalid("mime", "the bytes are not this type"));
    }
    if !image::size_matches(mime, &body, width, height) {
        return Err(invalid("width", "not the size in the image header"));
    }
    // 128 random bits: R2 keys are never listed publicly, but must not be guessable from ids.
    let mut random = [0u8; 16];
    ctx.crypto.random_bytes(&mut random)?;
    let key = format!("media/{}/{name}", base64url::encode(&random));
    let bytes = i64::try_from(body.len()).unwrap_or(i64::MAX);
    let r2 = bucket(ctx)?;
    r2.put(key.clone(), body)
        .http_metadata(HttpMetadata {
            content_type: Some(mime.to_owned()),
            ..HttpMetadata::default()
        })
        .execute()
        .await
        .map_err(|e| Error::new(ErrorCode::Storage, format!("R2 put {key}: {e}")))?;
    let new = NewMedia {
        r2_key: key.clone(),
        mime: mime.to_owned(),
        bytes,
        width,
        height,
        alt: alt_text.to_owned(),
        variants_json: "[]".to_owned(),
        uploaded_by: Some(user.id),
        created_at: ctx.now,
    };
    let id = match ctx.store.create_media(new.clone()).await {
        Ok(id) => id,
        Err(e) => {
            if let Err(r2e) = r2.delete(key.clone()).await {
                worker::console_error!(
                    "[{}] media upload: row failed and R2 object {key} is left behind: {r2e}",
                    ctx.request_id.as_str()
                );
            }
            return Err(e);
        }
    };
    changed(ctx, p, "media.upload", &id.0.to_string()).await;
    let m = Media {
        id,
        r2_key: new.r2_key,
        mime: new.mime,
        bytes,
        width,
        height,
        alt: new.alt,
        variants_json: new.variants_json,
        uploaded_by: new.uploaded_by,
        created_at: new.created_at,
    };
    Ok(Reply::json(201, media_json(ctx, &m, Some(&[]))))
}

/// `PATCH /api/v1/media/<id>`: the alt text (`""` marks it missing).
pub async fn update(ctx: &Ctx, p: &Principal, input: &Input) -> Result<Reply> {
    p.require(Permission::UploadMedia)?;
    let text = input.str("alt").unwrap_or_default();
    alt(text)?;
    let id = MediaId(input.id());
    ctx.store.set_media_alt(id, text).await?;
    changed(ctx, p, "media.alt", &id.0.to_string()).await;
    get(ctx, input).await
}

/// `DELETE /api/v1/media/<id>`: another uploader's (or an orphan's) needs `media.delete_any`;
/// refused while posts use it (409 with their ids).
pub async fn delete(ctx: &Ctx, p: &Principal, input: &Input) -> Result<Reply> {
    let user = p.require(Permission::UploadMedia)?;
    let m = ctx.store.get_media(MediaId(input.id())).await?;
    // Ownership before the in-use check, so a refused caller learns nothing about usage. The
    // row may change between this read and the delete, but harmlessly: ids are AUTOINCREMENT
    // (never reused) and `uploaded_by` only ever changes to NULL (user deletion), which
    // would only have made this check stricter.
    if m.uploaded_by != Some(user.id) {
        p.require(Permission::DeleteAnyMedia)?;
    }
    if let Err(e) = ctx.store.delete_media(m.id).await {
        if e.code != ErrorCode::Conflict {
            return Err(e);
        }
        let usage = ctx.store.media_usage(m.id).await?;
        worker::console_warn!(
            "[{}] media {} in use by {} posts",
            ctx.request_id.as_str(),
            m.id.0,
            usage.len()
        );
        return Ok(Reply::json(
            409,
            object(&[
                ("error", Out::Str(ErrorCode::Conflict.as_str().to_owned())),
                ("posts", ids_json(&usage)),
            ]),
        ));
    }
    let left = match bucket(ctx) {
        Ok(r2) => r2
            .delete(m.r2_key.clone())
            .await
            .err()
            .map(|e| e.to_string()),
        Err(e) => Some(e.to_string()),
    };
    let action = if let Some(why) = left {
        // The row is gone, so the object is unreachable: log the key for a cleanup.
        worker::console_error!(
            "[{}] media {} deleted but R2 object {} is left behind: {why}",
            ctx.request_id.as_str(),
            m.id.0,
            m.r2_key
        );
        "media.delete_r2_left"
    } else {
        "media.delete"
    };
    changed(ctx, p, action, &m.id.0.to_string()).await;
    Ok(Reply::empty())
}

#[cfg(test)]
mod tests {
    use super::{decode_cursor, key_name};

    #[test]
    fn key_names() {
        assert_eq!(key_name("Heron.JPG").unwrap(), "heron.jpg");
        assert_eq!(key_name("../../etc/passwd").unwrap(), "etc-passwd");
        assert_eq!(key_name("a b  \u{1ea3}nh.png").unwrap(), "a-b-nh.png");
        assert_eq!(key_name(".hidden").unwrap(), "hidden");
        assert!(key_name("...").is_err());
        assert!(key_name("").is_err());
        assert_eq!(key_name(&"x".repeat(300)).unwrap().len(), 100);
    }

    #[test]
    fn cursors() {
        let c = plumblog_auth::base64url::encode(b"17.4");
        assert_eq!(decode_cursor(&c).unwrap().1.0, 4);
        for bad in ["", "!", "MTc", "MTcuMA"] {
            assert!(decode_cursor(bad).is_err(), "{bad}");
        }
    }
}
