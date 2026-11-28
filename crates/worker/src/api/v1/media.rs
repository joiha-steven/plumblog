//! `/api/v1/media`: the library. An upload stores the original bytes in R2 (`MEDIA`) and a
//! row in D1; the smaller AVIF/WebP sizes are made by the admin's encoder (issue #13) and
//! stored one per call by `POST /api/v1/media/<id>/variants`.
//!
//! R2 and D1 cannot share a transaction, so each write orders them to leave at worst an
//! unreferenced R2 object, never a row without its bytes: upload writes R2 first and removes
//! the object if the row fails; delete removes the row first and the object after. Any object
//! left behind is logged with its key.

use plumblog_auth::{Crypto, base64url};
use plumblog_model::api::{DEFAULT_LIMIT, MAX_LIMIT, MAX_MEDIA_BODY, MEDIA_MIMES};
use plumblog_model::{Error, ErrorCode, MediaId, Permission, PostId, Result, image};
use plumblog_store::{Media, MediaQuery, MediaStore, NewMedia};
use worker::js_sys::Reflect;
use worker::wasm_bindgen::JsValue;
use worker::{Bucket, HttpMetadata};

use super::{Input, Principal, changed, stored_json};
use crate::api::body::{Out, object};
use crate::api::{Ctx, Raw, Reply};
use crate::media::variants;

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

/// The `sha256` query field: 64 lowercase hex characters, as text and as the 32 bytes R2
/// checks the body against.
fn sha256_field(text: &str) -> Result<(String, Vec<u8>)> {
    let bad = || invalid("sha256", "64 lowercase hex characters");
    if text.len() != 64 {
        return Err(bad());
    }
    let digit = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    let bytes = text
        .as_bytes()
        .chunks(2)
        .map(|p| Some(digit(p[0])? << 4 | digit(p[1])?))
        .collect::<Option<Vec<u8>>>()
        .ok_or_else(bad)?;
    Ok((text.to_owned(), bytes))
}

/// R2's error code for a body that is not the checksum the put named (`BadDigest`).
const R2_BAD_DIGEST: f64 = 10_037.0;

/// R2 refused the put because the body is not the `sha256` the client named. Two signals,
/// because the runtimes differ: R2's numeric error `code` 10037 is the stable one where the
/// error carries it (`worker` keeps `code` only as a string, so it is read from the original
/// value); local workerd throws a plain `Error` with no `code` (checked in worker-test) whose
/// message says "The SHA-256 checksum you specified did not match what we received".
fn checksum_mismatch(e: &worker::Error) -> bool {
    if let worker::Error::UnknownJsError { original, .. } = e
        && Reflect::get(original, &JsValue::from_str("code"))
            .ok()
            .and_then(|c| c.as_f64())
            == Some(R2_BAD_DIGEST)
    {
        return true;
    }
    let msg = e.to_string().to_ascii_lowercase();
    msg.contains("checksum") || msg.contains("baddigest")
}

/// What inserting an upload's row came to.
#[derive(Debug, PartialEq, Eq)]
enum Inserted {
    New(MediaId),
    /// An identical upload by the same caller got its row in first.
    Winner(Media),
}

/// Inserts the upload's row. On any failure `remove_object` deletes this request's R2 object,
/// so no bytes stay without a row; when the failure is a `Conflict` on `sha256` (an identical
/// upload by the same caller won the race), the winner is returned instead of the error.
async fn insert_row<S: MediaStore, F: Future<Output = ()>>(
    store: &S,
    new: &NewMedia,
    remove_object: impl FnOnce() -> F,
) -> Result<Inserted> {
    let e = match store.create_media(new.clone()).await {
        Ok(id) => return Ok(Inserted::New(id)),
        Err(e) => e,
    };
    remove_object().await;
    if e.code == ErrorCode::Conflict
        && e.field.as_deref() == Some("sha256")
        && let (Some(uploader), Some(hex)) = (new.uploaded_by, new.sha256.as_deref())
        && let Some(winner) = store.find_media_by_sha256(uploader, hex).await?
    {
        return Ok(Inserted::Winner(winner));
    }
    Err(e)
}

/// The item answered when the caller already uploaded these bytes: 200, nothing written.
async fn existing(ctx: &Ctx, m: &Media) -> Result<Reply> {
    let usage = ctx.store.media_usage(m.id).await?;
    Ok(Reply::json(200, media_json(ctx, m, Some(&usage))))
}

/// Most sizes one item keeps: four widths in two formats.
const MAX_VARIANTS: usize = 8;
/// The types a size may be, with the extension its R2 key gets.
const VARIANT_TYPES: &[(&str, &str)] = &[("image/avif", "avif"), ("image/webp", "webp")];
/// The widths the admin encoder makes (docs/ARCHITECTURE.md, Images).
const VARIANT_WIDTHS: [i64; 4] = [480, 960, 1600, 2400];

/// Where a new size would go in an item's list.
#[derive(Debug, PartialEq, Eq)]
enum Slot {
    /// Not there yet: the `variants_json` with it appended.
    Free(String),
    /// The same bytes are there already (a retry).
    Same,
    /// This (mime, width) holds other bytes.
    Taken,
    Full,
}

/// Where `entry` (one encoded JSON object) for (mime, width, sha256) goes in `text`.
fn slot(text: &str, mime: &str, width: i64, sha: &str, entry: &str) -> Result<Slot> {
    let list = variants(text).map_err(|why| Error::new(ErrorCode::Storage, why))?;
    if let Some(v) = list.iter().find(|v| v.mime == mime && v.width == width) {
        return Ok(if v.sha256 == sha {
            Slot::Same
        } else {
            Slot::Taken
        });
    }
    if list.len() >= MAX_VARIANTS {
        return Ok(Slot::Full);
    }
    // Appended as text, so fields this code does not know are kept as stored.
    Ok(Slot::Free(match text.trim().strip_suffix(']') {
        Some(head) if !list.is_empty() => format!("{head},{entry}]"),
        _ => format!("[{entry}]"),
    }))
}

/// `media/<random>/heron.jpg` -> `media/<random>/heron-480-<sha256>.avif`. The full hash keeps
/// two encodings of one (mime, width) in separate objects, so a lost race removes only its own
/// (a shorter prefix can be made to collide).
fn variant_key(original: &str, width: i64, sha: &str, ext: &str) -> String {
    let (dir, name) = original.rsplit_once('/').unwrap_or(("", original));
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    let dir = if dir.is_empty() {
        String::new()
    } else {
        format!("{dir}/")
    };
    format!("{dir}{stem}-{width}-{sha}.{ext}")
}

/// The item, when the caller may change it (delete it, add a size): their own upload, else
/// `media.delete_any`, which an item whose uploader was deleted needs too.
pub async fn owned(ctx: &Ctx, p: &Principal, id: MediaId) -> Result<Media> {
    let user = p.require(Permission::UploadMedia)?;
    let m = ctx.store.get_media(id).await?;
    // The row may change after this read, but harmlessly: ids are AUTOINCREMENT (never
    // reused) and `uploaded_by` only ever changes to NULL (user deletion), which would only
    // have made this check stricter.
    if m.uploaded_by != Some(user.id) {
        p.require(Permission::DeleteAnyMedia)?;
    }
    Ok(m)
}

fn taken() -> Error {
    Error::new(
        ErrorCode::Conflict,
        "this mime and width already hold other bytes",
    )
    .at("width")
}

fn full() -> Error {
    Error::new(
        ErrorCode::Conflict,
        format!("an item keeps at most {MAX_VARIANTS} sizes"),
    )
    .at("variants")
}

/// A size as checked against its original, ready to store.
struct NewVariant {
    mime: &'static str,
    width: i64,
    key: String,
    /// Its `variants_json` entry.
    entry: String,
    hex: String,
    sum: Vec<u8>,
}

/// The input checks of a new size: an upload's (type, magic bytes, header size, `sha256`),
/// then in the original's aspect within one pixel of rounding, at a width it allows.
fn new_variant(m: &Media, input: &Input, body: &[u8]) -> Result<NewVariant> {
    let want = input.str("mime").unwrap_or_default();
    let (mime, ext) = *VARIANT_TYPES
        .iter()
        .find(|(t, _)| *t == want)
        .ok_or_else(|| invalid("mime", "image/avif or image/webp"))?;
    let width = input.int("width").unwrap_or(0);
    let height = input.int("height").unwrap_or(0);
    for (field, v) in [("width", width), ("height", height)] {
        if !(1..=MAX_SIDE).contains(&v) {
            return Err(invalid(field, "1-65536"));
        }
    }
    let (hex, sum) = sha256_field(input.str("sha256").unwrap_or_default())?;
    if image::sniff(body) != Some(mime) {
        return Err(invalid("mime", "the bytes are not this type"));
    }
    if !image::size_matches(mime, body, width, height) {
        return Err(invalid("width", "not the size in the image header"));
    }
    // A JPEG or AVIF original (EXIF orientation, `irot`) may be stored in header order, a
    // quarter turn from the display order the browser encodes in: either orientation's
    // aspect then, within 1 px. The width is a standard one no wider than that orientation,
    // or exactly its width (a format conversion).
    let turns = if matches!(m.mime.as_str(), "image/jpeg" | "image/avif") {
        2
    } else {
        1
    };
    let max = [(m.width, m.height), (m.height, m.width)]
        .into_iter()
        .take(turns)
        .filter(|&(w, h)| (height * w - h * width).abs() <= w)
        .map(|(w, _)| w)
        .max()
        .ok_or_else(|| invalid("height", "not the original's aspect"))?;
    if width > max || (width != max && !VARIANT_WIDTHS.contains(&width)) {
        return Err(invalid(
            "width",
            "480, 960, 1600 or 2400, at most the original's width, or exactly it",
        ));
    }
    let key = variant_key(&m.r2_key, width, &hex, ext);
    let bytes = i64::try_from(body.len()).unwrap_or(i64::MAX);
    let entry = object(&[
        ("key", Out::Str(key.clone())),
        ("width", Out::Int(width)),
        ("height", Out::Int(height)),
        ("bytes", Out::Int(bytes)),
        ("mime", Out::Str(mime.to_owned())),
        ("sha256", Out::Str(hex.clone())),
    ]);
    Ok(NewVariant {
        mime,
        width,
        key,
        entry,
        hex,
        sum,
    })
}

/// `POST /api/v1/media/<id>/variants`: one smaller AVIF or WebP of the original, made by the
/// admin's encoder, checked by [`new_variant`]. One size per (mime, width): the same bytes
/// again answer 200 with the item, other bytes 409. The key is the original's with
/// `-<width>-<sha256>.<ext>`, so a retry overwrites its own object with the same bytes.
///
/// R2 first, then the row by compare and swap ([`MediaStore::replace_media_variants`]), so two
/// sizes added at once are both kept. An object is removed only when no row can name it.
pub async fn add_variant(ctx: &Ctx, p: &Principal, input: &Input, body: Vec<u8>) -> Result<Reply> {
    let m = owned(ctx, p, MediaId(input.id())).await?;
    let NewVariant {
        mime,
        width,
        key,
        entry,
        hex,
        sum,
    } = new_variant(&m, input, &body)?;
    // Refused before the put where the row already says so, so nothing is written.
    match slot(&m.variants_json, mime, width, &hex, &entry)? {
        Slot::Free(_) => {}
        Slot::Same => return answer(ctx, &m, 200).await,
        Slot::Taken => return Err(taken()),
        Slot::Full => return Err(full()),
    }
    let r2 = bucket(ctx)?;
    r2.put(key.clone(), body)
        .http_metadata(HttpMetadata {
            content_type: Some(mime.to_owned()),
            ..HttpMetadata::default()
        })
        .sha256(sum)
        .execute()
        .await
        .map_err(|e| {
            if checksum_mismatch(&e) {
                invalid("sha256", "not the SHA-256 of the body")
            } else {
                Error::new(ErrorCode::Storage, format!("R2 put {key}: {e}"))
            }
        })?;
    let rid = ctx.request_id.as_str();
    let remove = || async {
        if let Err(e) = r2.delete(key.clone()).await {
            worker::console_error!(
                "[{rid}] media {}: R2 object {key} is left behind: {e}",
                m.id.0
            );
        }
    };
    let mut cur = m.clone();
    // Each lost swap means another size was added, and there are at most MAX_VARIANTS.
    for _ in 0..=MAX_VARIANTS {
        let next = match slot(&cur.variants_json, mime, width, &hex, &entry)? {
            Slot::Free(next) => next,
            // A retry of these bytes won the race: the object holds them either way.
            Slot::Same => return answer(ctx, &cur, 200).await,
            // Other bytes won this (mime, width), or the list filled up. Entries are never
            // removed while the item exists, and the key holds this request's full hash, which
            // the winner's other bytes cannot share, so no row can come to name this object.
            Slot::Taken => {
                remove().await;
                return Err(taken());
            }
            Slot::Full => {
                remove().await;
                return Err(full());
            }
        };
        match ctx
            .store
            .replace_media_variants(m.id, &cur.variants_json, &next)
            .await
        {
            Ok(()) => {
                changed(ctx, p, "media.variant", &m.id.0.to_string()).await;
                cur.variants_json = next;
                return answer(ctx, &cur, 201).await;
            }
            Err(e) if e.code == ErrorCode::Conflict => {
                cur = match ctx.store.get_media(m.id).await {
                    Ok(row) => row,
                    Err(e) => return Err(gone(e, remove, rid, &key).await),
                };
            }
            Err(e) => return Err(gone(e, remove, rid, &key).await),
        }
    }
    Err(Error::new(
        ErrorCode::Storage,
        format!("media {}: variants kept changing", m.id.0),
    ))
}

/// A failed row write: the object goes only when the row is gone (nothing can name the key);
/// on any other failure the row may still come to name it, so it stays and is logged.
async fn gone<F: Future<Output = ()>>(
    e: Error,
    remove: impl FnOnce() -> F,
    rid: &str,
    key: &str,
) -> Error {
    if e.code == ErrorCode::NotFound {
        remove().await;
    } else {
        worker::console_error!(
            "[{rid}] media variant: row write failed, R2 object {key} kept: {}",
            e.detail
        );
    }
    e
}

/// The item with its usage, as a variant add answers it.
async fn answer(ctx: &Ctx, m: &Media, status: u16) -> Result<Reply> {
    let usage = ctx.store.media_usage(m.id).await?;
    Ok(Reply::json(status, media_json(ctx, m, Some(&usage))))
}

/// `POST /api/v1/media`: the body is the original image; `mime` must be what its magic bytes
/// say and `width` x `height` what its header declares.
///
/// With `sha256` (the client hashed the body: hashing 20 MiB in the Worker costs most of the
/// CPU budget, see Budgets), an upload whose (caller, sha256) exists answers that item with 200
/// and writes nothing, so a retry makes no second copy; R2 checks the hash on put, so a wrong
/// one stores nothing and is 400. Two identical uploads at once: the row's unique index picks
/// one, the other removes its object and answers the winner.
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
    let sha256 = input.str("sha256").map(sha256_field).transpose()?;
    if image::sniff(&body) != Some(mime) {
        return Err(invalid("mime", "the bytes are not this type"));
    }
    if !image::size_matches(mime, &body, width, height) {
        return Err(invalid("width", "not the size in the image header"));
    }
    if let Some((hex, _)) = &sha256
        && let Some(m) = ctx.store.find_media_by_sha256(user.id, hex).await?
    {
        return existing(ctx, &m).await;
    }
    // 128 random bits: R2 keys are never listed publicly, but must not be guessable from ids.
    let mut random = [0u8; 16];
    ctx.crypto.random_bytes(&mut random)?;
    let key = format!("media/{}/{name}", base64url::encode(&random));
    let bytes = i64::try_from(body.len()).unwrap_or(i64::MAX);
    let r2 = bucket(ctx)?;
    let mut put = r2.put(key.clone(), body).http_metadata(HttpMetadata {
        content_type: Some(mime.to_owned()),
        ..HttpMetadata::default()
    });
    if let Some((_, sum)) = &sha256 {
        put = put.sha256(sum.clone());
    }
    // A put R2 refuses stores nothing (writes are atomic), so a mismatch leaves no object.
    put.execute().await.map_err(|e| {
        if sha256.is_some() && checksum_mismatch(&e) {
            invalid("sha256", "not the SHA-256 of the body")
        } else {
            Error::new(ErrorCode::Storage, format!("R2 put {key}: {e}"))
        }
    })?;
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
        sha256: sha256.map(|(hex, _)| hex),
    };
    let removed = || async {
        if let Err(r2e) = r2.delete(key.clone()).await {
            worker::console_error!(
                "[{}] media upload: row failed and R2 object {key} is left behind: {r2e}",
                ctx.request_id.as_str()
            );
        }
    };
    let id = match insert_row(&ctx.store, &new, removed).await? {
        Inserted::New(id) => id,
        Inserted::Winner(m) => return existing(ctx, &m).await,
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
        sha256: new.sha256,
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
    // Ownership before the in-use check, so a refused caller learns nothing about usage.
    let m = owned(ctx, p, MediaId(input.id())).await?;
    let variants = match ctx.store.delete_media(m.id).await {
        Ok(text) => text,
        Err(e) if e.code != ErrorCode::Conflict => return Err(e),
        Err(_) => {
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
    };
    // The sizes as the delete found them, so one added after the read above goes too.
    let mut keys = vec![m.r2_key.clone()];
    let mut left = Vec::new();
    match crate::media::variants(&variants) {
        Ok(list) => keys.extend(list.into_iter().map(|v| v.key)),
        Err(why) => left.push(format!("its sizes ({why}): {variants}")),
    }
    match bucket(ctx) {
        Ok(r2) => {
            for key in keys {
                if let Err(e) = r2.delete(key.clone()).await {
                    left.push(format!("{key}: {e}"));
                }
            }
        }
        Err(e) => left.push(format!("{}: {e}", keys.join(", "))),
    }
    let action = if left.is_empty() {
        "media.delete"
    } else {
        // The row is gone, so the objects are unreachable: log the keys for a cleanup.
        worker::console_error!(
            "[{}] media {} deleted but R2 objects are left behind: {}",
            ctx.request_id.as_str(),
            m.id.0,
            left.join("; ")
        );
        "media.delete_r2_left"
    };
    changed(ctx, p, action, &m.id.0.to_string()).await;
    Ok(Reply::empty())
}

#[cfg(test)]
mod tests {
    use super::{Slot, decode_cursor, key_name, slot, variant_key};

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
    fn variant_keys() {
        let h = "0123456789abcdef";
        assert_eq!(
            variant_key("media/abc/heron.jpg", 480, h, "avif"),
            "media/abc/heron-480-0123456789abcdef.avif"
        );
        assert_eq!(
            variant_key("media/abc/a.b.png", 960, h, "webp"),
            "media/abc/a.b-960-0123456789abcdef.webp"
        );
        assert_eq!(
            variant_key("media/abc/raw", 480, h, "avif"),
            "media/abc/raw-480-0123456789abcdef.avif"
        );
        assert_eq!(
            variant_key("o/1.jpg", 2, h, "avif"),
            "o/1-2-0123456789abcdef.avif"
        );
    }

    /// The loser of a race for one (mime, width) removes its own object, so its key must
    /// never be the winner's: hashes sharing a prefix (made to, or by chance) still differ.
    #[test]
    fn keys_of_other_bytes_never_collide() {
        let a = format!("deadbeef{}", "0".repeat(56));
        let b = format!("deadbeef{}", "1".repeat(56));
        assert_ne!(
            variant_key("media/abc/heron.jpg", 480, &a, "avif"),
            variant_key("media/abc/heron.jpg", 480, &b, "avif")
        );
    }

    #[test]
    fn slots() {
        let e = r#"{"key":"k","mime":"image/avif","width":480,"sha256":"aa"}"#;
        let one = format!("[{e}]");
        assert_eq!(
            slot("[]", "image/avif", 480, "aa", e).unwrap(),
            Slot::Free(one.clone())
        );
        assert_eq!(
            slot("{}", "image/avif", 480, "aa", e).unwrap(),
            Slot::Free(one.clone())
        );
        assert_eq!(slot(&one, "image/avif", 480, "aa", e).unwrap(), Slot::Same);
        assert_eq!(slot(&one, "image/avif", 480, "bb", e).unwrap(), Slot::Taken);
        let w = r#"{"key":"w","mime":"image/webp","width":480,"x":[1]}"#;
        assert_eq!(
            slot(&format!("[ {w} ]"), "image/avif", 480, "aa", e).unwrap(),
            Slot::Free(format!("[ {w} ,{e}]"))
        );
        let eight: Vec<String> = (1..=8)
            .map(|i| format!(r#"{{"key":"k{i}","mime":"image/avif","width":{i}}}"#))
            .collect();
        let full = format!("[{}]", eight.join(","));
        assert_eq!(slot(&full, "image/avif", 480, "aa", e).unwrap(), Slot::Full);
        assert_eq!(slot(&full, "image/avif", 3, "", e).unwrap(), Slot::Same);
        for bad in ["", "3", r#"["x"]"#, r#"[{"key":"k"}]"#] {
            assert!(slot(bad, "image/avif", 480, "aa", e).is_err(), "{bad}");
        }
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

#[cfg(test)]
mod insert_tests {
    use std::cell::Cell;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    use plumblog_model::ErrorCode;
    use plumblog_store::memory::MemoryStore;
    use plumblog_store::{MediaStore, NewMedia, NewUser, UserStore};

    use super::{Inserted, insert_row};

    /// The memory store never awaits, so one poll finishes.
    fn run<F: Future>(f: F) -> F::Output {
        match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("memory store future was pending"),
        }
    }

    fn upload(key: &str, user: plumblog_model::UserId, sha256: Option<&str>) -> NewMedia {
        NewMedia {
            r2_key: key.to_owned(),
            mime: "image/png".to_owned(),
            bytes: 1,
            width: 1,
            height: 1,
            alt: String::new(),
            variants_json: "[]".to_owned(),
            uploaded_by: Some(user),
            created_at: 1,
            sha256: sha256.map(str::to_owned),
        }
    }

    fn store() -> (MemoryStore, plumblog_model::UserId) {
        let db = MemoryStore::default();
        let user = run(db.create_user(NewUser {
            login: "w".to_owned(),
            slug: "w".to_owned(),
            display_name: "W".to_owned(),
            bio: String::new(),
            role: plumblog_model::Role::Writer,
            password_hash: None,
            created_at: 1,
        }))
        .unwrap();
        (db, user)
    }

    /// The loser of two identical uploads: its insert conflicts on `sha256`, its own object
    /// is removed and the winner's item comes back.
    #[test]
    fn a_lost_race_removes_the_object_and_returns_the_winner() {
        let (db, user) = store();
        let hash = "ab".repeat(32);
        let won = run(db.create_media(upload("media/a/x.png", user, Some(&hash)))).unwrap();
        let removed = Cell::new(0);
        let got = run(insert_row(
            &db,
            &upload("media/b/x.png", user, Some(&hash)),
            || async { removed.set(removed.get() + 1) },
        ))
        .unwrap();
        assert_eq!(removed.get(), 1, "the loser's R2 object is deleted");
        match got {
            Inserted::Winner(m) => assert_eq!((m.id, m.r2_key.as_str()), (won, "media/a/x.png")),
            Inserted::New(id) => panic!("expected the winner, got a new item {id:?}"),
        }
    }

    #[test]
    fn a_new_row_keeps_its_object() {
        let (db, user) = store();
        let removed = Cell::new(0);
        let got = run(insert_row(
            &db,
            &upload("media/a/x.png", user, Some(&"cd".repeat(32))),
            || async { removed.set(removed.get() + 1) },
        ))
        .unwrap();
        assert!(matches!(got, Inserted::New(_)), "{got:?}");
        assert_eq!(removed.get(), 0);
    }

    /// Any other failure (here a duplicate R2 key, no hash) removes the object and is the error.
    #[test]
    fn another_failure_removes_the_object_and_is_returned() {
        let (db, user) = store();
        run(db.create_media(upload("media/a/x.png", user, None))).unwrap();
        let removed = Cell::new(0);
        let err = run(insert_row(
            &db,
            &upload("media/a/x.png", user, None),
            || async {
                removed.set(removed.get() + 1);
            },
        ))
        .unwrap_err();
        assert_eq!((err.code, err.field), (ErrorCode::Conflict, None));
        assert_eq!(removed.get(), 1);
    }
}
