//! `GET`/`HEAD /media/<id>[/<file name>]`: image bytes from R2 on the public site
//! (docs/ARCHITECTURE.md, Images). Anonymous readers get only media a published post or
//! page uses, cached for good; a signed-in user or a token gets any item, uncached. Every
//! other case is the same empty 404, so nothing tells a draft's image from no image.
//!
//! Nothing here writes to D1 or sets a cookie: the principal is resolved read-only.

use plumblog_auth::json::{self, Value};
use plumblog_model::api::{MAX_MEDIA_BODY, MEDIA_MIMES};
use plumblog_model::{ErrorCode, MediaId};
use plumblog_store::{Media, MediaStore};
use plumblog_syslog::request_id;
use worker::{
    Bucket, Conditional, Env, Headers, Method, Object, Range, Request, Response, Result,
    console_error, console_log, console_warn, js_sys,
};

use crate::api::{Ctx, Principal, resolve_read_only};
use crate::d1::D1Store;
use crate::webcrypto::WebCrypto;

pub const PREFIX: &str = "/media/";

/// A URL always names the same bytes (a re-upload is a new id), so a public copy never goes
/// stale. Unpublishing cannot recall copies already cached.
const PUBLIC_CACHE: &str = "public, max-age=31536000, immutable";
const PRIVATE_CACHE: &str = "private, no-store";

/// The id and file name of `/media/<id>` or `/media/<id>/<name>`; `None` for anything else
/// (a non-canonical id such as `012` or `12.avif`, an empty name, more segments).
fn parse_path(path: &str) -> Option<(MediaId, Option<&str>)> {
    let rest = path.strip_prefix(PREFIX)?;
    let (id, name) = match rest.split_once('/') {
        Some((id, name)) if !name.is_empty() && !name.contains('/') => (id, Some(name)),
        Some(_) => return None,
        None => (rest, None),
    };
    if id.is_empty() || id.starts_with('0') || !id.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    id.parse().ok().map(|id| (MediaId(id), name))
}

/// The R2 object a URL names, with what the row says it must be.
#[derive(Debug, PartialEq, Eq)]
struct Target {
    key: String,
    mime: String,
    bytes: i64,
}

fn last_segment(key: &str) -> &str {
    key.rsplit('/').next().unwrap_or_default()
}

/// The original when `name` is missing or its file name, else the `variants_json` entry
/// whose `key` ends in `name`. `Ok(None)` when nothing matches; `Err` when the stored
/// variants (or the matching entry) are not what the admin encoder writes.
fn pick(m: &Media, name: Option<&str>) -> std::result::Result<Option<Target>, String> {
    let Some(name) = name.filter(|n| *n != last_segment(&m.r2_key)) else {
        return Ok(Some(Target {
            key: m.r2_key.clone(),
            mime: m.mime.clone(),
            bytes: m.bytes,
        }));
    };
    let Some(v) = variants(&m.variants_json)?
        .into_iter()
        .find(|v| last_segment(&v.key) == name)
    else {
        return Ok(None);
    };
    let bytes = v
        .bytes
        .ok_or_else(|| format!("variant {} has no bytes", v.key))?;
    Ok(Some(Target {
        key: v.key,
        mime: v.mime,
        bytes,
    }))
}

/// One `variants_json` entry, as `POST /api/v1/media/{id}/variants` writes it.
#[derive(Default)]
pub(crate) struct Variant {
    pub key: String,
    pub mime: String,
    pub width: i64,
    pub bytes: Option<i64>,
    /// `""` when absent.
    pub sha256: String,
}

/// An item's stored sizes, or why `variants_json` is unreadable: each entry needs `key`,
/// `mime` and `width`. `{}` is what older rows and fixtures hold: no sizes.
pub(crate) fn variants(text: &str) -> std::result::Result<Vec<Variant>, String> {
    // The strict reader takes an object at the top.
    let wrapped = format!("{{\"v\":{text}}}");
    let items = match json::parse_document(wrapped.as_bytes(), usize::MAX) {
        Ok(mut doc) if doc.len() == 1 => match doc.remove(0).1 {
            Value::Arr(items) => items,
            Value::Obj(o) if o.is_empty() => return Ok(Vec::new()),
            _ => return Err("variants_json is not an array".into()),
        },
        _ => return Err("variants_json is not JSON".into()),
    };
    items
        .into_iter()
        .map(|item| {
            let Value::Obj(fields) = item else {
                return Err("a variant is not an object".into());
            };
            let (mut v, mut seen) = (Variant::default(), 0);
            for (k, val) in fields {
                match (k.as_str(), val) {
                    ("key", Value::Str(t)) => (v.key, seen) = (t, seen | 1),
                    ("mime", Value::Str(t)) => (v.mime, seen) = (t, seen | 2),
                    ("width", Value::Int(n)) => (v.width, seen) = (n, seen | 4),
                    ("bytes", Value::Int(n)) => v.bytes = Some(n),
                    ("sha256", Value::Str(t)) => v.sha256 = t,
                    _ => {}
                }
            }
            if seen == 7 {
                Ok(v)
            } else {
                Err(format!("variant {:?} has no key, mime or width", v.key))
            }
        })
        .collect()
}

/// What a `Range` header asks of an object of `size` bytes.
#[derive(Debug, PartialEq, Eq)]
enum Want {
    /// No header, a multi-range or a malformed one: the whole body (RFC 9110 lets a server
    /// ignore any `Range`).
    Full,
    /// Bytes `start..=end`.
    Part { start: u64, end: u64 },
    /// 416.
    Unsatisfiable,
}

fn parse_range(header: Option<&str>, size: u64) -> Want {
    let Some(spec) = header.map(str::trim).and_then(|h| {
        h.get(..6)
            .filter(|u| u.eq_ignore_ascii_case("bytes="))
            .map(|_| h[6..].trim())
    }) else {
        return Want::Full;
    };
    let num = |s: &str| -> Option<u64> {
        let s = s.trim();
        (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse().ok())
            .flatten()
    };
    let Some((a, b)) = spec.split_once('-') else {
        return Want::Full;
    };
    if spec.contains(',') {
        return Want::Full;
    }
    let last = size.checked_sub(1);
    match (a.trim().is_empty(), b.trim().is_empty()) {
        // `-n`: the last n bytes.
        (true, false) => match (num(b), last) {
            (None, _) => Want::Full,
            (Some(0), _) | (_, None) => Want::Unsatisfiable,
            (Some(n), Some(last)) => Want::Part {
                start: size.saturating_sub(n),
                end: last,
            },
        },
        // `a-`.
        (false, true) => match (num(a), last) {
            (None, _) => Want::Full,
            (Some(start), Some(last)) if start <= last => Want::Part { start, end: last },
            (Some(_), _) => Want::Unsatisfiable,
        },
        // `a-b`.
        (false, false) => match (num(a), num(b), last) {
            (Some(start), Some(end), _) if start > end => Want::Full,
            (Some(start), Some(end), Some(last)) if start <= last => Want::Part {
                start,
                end: end.min(last),
            },
            (Some(_), Some(_), _) => Want::Unsatisfiable,
            _ => Want::Full,
        },
        (true, true) => Want::Full,
    }
}

/// The one entity tag of an `If-None-Match` header, unquoted (R2 compares bare etags);
/// `None` for `*`, a list or nothing, which are then not used (a 200 is always correct).
fn single_etag(header: Option<&str>) -> Option<String> {
    let h = header?.trim();
    if h.contains(',') {
        return None;
    }
    let h = h.strip_prefix("W/").unwrap_or(h);
    let tag = h.strip_prefix('"')?.strip_suffix('"')?;
    (!tag.is_empty() && !tag.contains('"')).then(|| tag.to_owned())
}

pub(crate) fn random_id() -> String {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a random float in [0, 1) scaled into u64 range"
    )]
    let n = (js_sys::Math::random() * 1e15) as u64;
    format!("req-{n:x}")
}

/// An empty answer that no cache keeps.
fn empty(status: u16) -> Result<Response> {
    let mut res = Response::empty()?.with_status(status);
    res.headers_mut().set("cache-control", "no-store")?;
    Ok(res)
}

/// Handles `GET`/`HEAD` under [`PREFIX`].
pub async fn serve(req: &Request, env: &Env) -> Result<Response> {
    let rid = request_id(req.headers().get("cf-ray")?.as_deref(), random_id);
    let path = req.path();
    let Some((id, name)) = parse_path(&path) else {
        console_log!("[{}] media.not_found {path}: not a media URL", rid.as_str());
        return empty(404);
    };
    let (Ok(crypto), Ok(db)) = (WebCrypto::new(), env.d1("DB")) else {
        console_error!("[{}] media: no WebCrypto or D1 binding", rid.as_str());
        return empty(500);
    };
    let ctx = Ctx {
        store: D1Store::new(db),
        crypto,
        env: env.clone(),
        now: i64::try_from(worker::Date::now().as_millis()).unwrap_or(0),
        request_id: rid.clone(),
        ip: None,
        request_origin: String::new(),
    };
    match answer(&ctx, req, id, name).await {
        Ok(res) => Ok(res),
        Err(Fail::NotFound(why)) => {
            console_log!("[{}] media.not_found {path}: {why}", rid.as_str());
            empty(404)
        }
        Err(Fail::Gone(why)) => {
            console_error!("[{}] media {}: {why}", rid.as_str(), id.0);
            empty(404)
        }
        Err(Fail::Broken(why)) => {
            console_error!("[{}] media {} failed: {why}", rid.as_str(), id.0);
            empty(500)
        }
    }
}

enum Fail {
    /// Logged, answered with the one empty 404.
    NotFound(String),
    /// The row's R2 object is gone (removed outside the API): logged as an error, 404.
    Gone(String),
    /// Ours: logged as an error, 500.
    Broken(String),
}

fn broken(e: impl std::fmt::Display) -> Fail {
    Fail::Broken(e.to_string())
}

/// The row this request may read, and whether it was read signed in (any item) rather
/// than as a reader (published use only).
async fn find(ctx: &Ctx, req: &Request, id: MediaId) -> std::result::Result<(Media, bool), Fail> {
    let rid = ctx.request_id.as_str();
    let signed_in = match resolve_read_only(ctx, req).await {
        Ok(Principal::Anonymous { .. }) => false,
        // Any signed-in user or token reads drafts on `/api/v1` (`read`); so here too.
        Ok(p) => p.require_read().is_ok(),
        // A bad token only loses what it could have opened: the public answer is the same.
        Err(e) if e.code == ErrorCode::Unauthenticated => {
            console_warn!("[{rid}] media {}: {e}; answering as anonymous", id.0);
            false
        }
        Err(e) => return Err(broken(e)),
    };
    let row = if signed_in {
        match ctx.store.get_media(id).await {
            Ok(m) => Some(m),
            Err(e) if e.code == ErrorCode::NotFound => None,
            Err(e) => return Err(broken(e)),
        }
    } else {
        ctx.store.public_media(id).await.map_err(broken)?
    };
    let Some(m) = row else {
        return Err(Fail::NotFound(format!(
            "media {} unknown or not on a published post",
            id.0
        )));
    };
    Ok((m, signed_in))
}

async fn answer(
    ctx: &Ctx,
    req: &Request,
    id: MediaId,
    name: Option<&str>,
) -> std::result::Result<Response, Fail> {
    let (m, signed_in) = find(ctx, req, id).await?;
    let target = pick(&m, name)
        .map_err(|why| Fail::Broken(format!("{why} ({:?})", m.variants_json)))?
        .ok_or_else(|| {
            Fail::NotFound(format!(
                "media {} has no file {:?}",
                id.0,
                name.unwrap_or_default()
            ))
        })?;
    // Uploads only accept raster types; anything else (SVG, HTML) was written around the API
    // and must never be served from the site's origin.
    if !MEDIA_MIMES.contains(&target.mime.as_str()) {
        return Err(Fail::Broken(format!(
            "{}: stored mime {:?} is not an accepted image type",
            target.key, target.mime
        )));
    }
    let size = u64::try_from(target.bytes)
        .ok()
        .filter(|s| *s <= u64::try_from(MAX_MEDIA_BODY).unwrap_or(u64::MAX))
        .ok_or_else(|| Fail::Broken(format!("{}: row says {} bytes", target.key, target.bytes)))?;
    let cache = if signed_in {
        PRIVATE_CACHE
    } else {
        PUBLIC_CACHE
    };
    let bucket: Bucket = ctx.env.bucket("MEDIA").map_err(broken)?;
    let header = |n: &str| req.headers().get(n).ok().flatten();
    let etag_in = single_etag(header("if-none-match").as_deref());
    if req.method() == Method::Head {
        let obj = bucket.head(target.key.clone()).await.map_err(broken)?;
        let obj = present(&target, obj)?;
        if etag_in.as_deref() == Some(obj.etag().as_str()) {
            return not_modified(&target, &obj, cache);
        }
        let mut res = Response::empty().map_err(broken)?;
        set_headers(res.headers_mut(), &target, &obj, cache, size).map_err(broken)?;
        return Ok(res);
    }
    let want = parse_range(header("range").as_deref(), size);
    if want == Want::Unsatisfiable {
        // RFC 9110 13.2.2: If-None-Match is decided before Range, so a client holding the
        // current copy gets 304 even with a range past the end (rare; one extra R2 head).
        if let Some(tag) = etag_in.as_deref() {
            let obj = bucket.head(target.key.clone()).await.map_err(broken)?;
            let obj = present(&target, obj)?;
            if tag == obj.etag() {
                return not_modified(&target, &obj, cache);
            }
        }
        let mut res = empty(416).map_err(broken)?;
        res.headers_mut()
            .set("content-range", &format!("bytes */{size}"))
            .map_err(broken)?;
        return Ok(res);
    }
    let mut get = bucket.get(target.key.clone());
    if let Some(tag) = etag_in.clone() {
        get = get.only_if(Conditional {
            etag_does_not_match: Some(tag),
            ..Conditional::default()
        });
    }
    if let Want::Part { start, end } = want {
        get = get.range(Range::OffsetWithLength {
            offset: start,
            length: end - start + 1,
        });
    }
    let obj = present(&target, get.execute().await.map_err(broken)?)?;
    let Some(body) = obj.body() else {
        // R2 leaves the body out only when `onlyIf` failed: the reader's copy is current.
        if etag_in.is_some() {
            return not_modified(&target, &obj, cache);
        }
        return Err(Fail::Broken(format!(
            "{}: R2 object has no body",
            target.key
        )));
    };
    let body = body.response_body().map_err(broken)?;
    let mut res = Response::from_body(body).map_err(broken)?;
    let length = match want {
        Want::Part { start, end } => {
            res = res.with_status(206);
            res.headers_mut()
                .set("content-range", &format!("bytes {start}-{end}/{size}"))
                .map_err(broken)?;
            end - start + 1
        }
        _ => size,
    };
    set_headers(res.headers_mut(), &target, &obj, cache, length).map_err(broken)?;
    Ok(res)
}

/// The object, if it is there and as large as the row says.
fn present(target: &Target, obj: Option<Object>) -> std::result::Result<Object, Fail> {
    let Some(obj) = obj else {
        // The row points at bytes that are gone (removed outside the API).
        return Err(Fail::Gone(format!("R2 object {} is missing", target.key)));
    };
    if u64::try_from(target.bytes).ok() != Some(obj.size()) {
        return Err(Fail::Broken(format!(
            "R2 object {} is {} bytes, row says {}",
            target.key,
            obj.size(),
            target.bytes
        )));
    }
    Ok(obj)
}

fn not_modified(target: &Target, obj: &Object, cache: &str) -> std::result::Result<Response, Fail> {
    let mut res = Response::empty().map_err(broken)?.with_status(304);
    let h = res.headers_mut();
    h.set("etag", &obj.http_etag()).map_err(broken)?;
    h.set("cache-control", cache).map_err(broken)?;
    h.set("content-type", &target.mime).map_err(broken)?;
    Ok(res)
}

fn set_headers(h: &Headers, target: &Target, obj: &Object, cache: &str, length: u64) -> Result<()> {
    h.set("content-type", &target.mime)?;
    h.set("content-length", &length.to_string())?;
    h.set("accept-ranges", "bytes")?;
    h.set("etag", &obj.http_etag())?;
    h.set("cache-control", cache)?;
    h.set("x-content-type-options", "nosniff")?;
    // Defence in depth: even if a browser renders the bytes as a document, it runs
    // sandboxed, with no scripts and an opaque origin.
    h.set("content-security-policy", "sandbox")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths() {
        assert_eq!(parse_path("/media/12"), Some((MediaId(12), None)));
        assert_eq!(
            parse_path("/media/12/heron.avif"),
            Some((MediaId(12), Some("heron.avif")))
        );
        for bad in [
            "/media/",
            "/media/abc",
            "/media/012",
            "/media/12.avif",
            "/media/-1",
            "/media/12/",
            "/media/12/a/b",
            "/media/99999999999999999999",
        ] {
            assert_eq!(parse_path(bad), None, "{bad}");
        }
    }

    fn row(variants: &str) -> Media {
        Media {
            id: MediaId(1),
            r2_key: "media/abc/heron.png".into(),
            mime: "image/png".into(),
            bytes: 10,
            width: 4,
            height: 3,
            alt: String::new(),
            variants_json: variants.into(),
            uploaded_by: None,
            created_at: 0,
            sha256: None,
        }
    }

    #[test]
    fn picks_original_and_variants() {
        let v = r#"[{"key":"media/abc/heron-480.avif","width":480,"height":360,"bytes":7,"mime":"image/avif"}]"#;
        let m = row(v);
        let original = Target {
            key: "media/abc/heron.png".into(),
            mime: "image/png".into(),
            bytes: 10,
        };
        assert_eq!(pick(&m, None), Ok(Some(original)));
        assert_eq!(pick(&m, Some("heron.png")).unwrap().unwrap().bytes, 10);
        assert_eq!(
            pick(&m, Some("heron-480.avif")),
            Ok(Some(Target {
                key: "media/abc/heron-480.avif".into(),
                mime: "image/avif".into(),
                bytes: 7,
            }))
        );
        assert_eq!(pick(&m, Some("other.png")), Ok(None));
        assert_eq!(pick(&row("[]"), Some("x.avif")), Ok(None));
        assert_eq!(pick(&row("{}"), Some("x.avif")), Ok(None));
        assert!(pick(&row("nope"), Some("x.avif")).is_err());
        assert!(pick(&row(r#"[{"key":"media/a/x.avif"}]"#), Some("x.avif")).is_err());
        // A broken variant list never blocks the original.
        assert!(pick(&row("nope"), Some("heron.png")).unwrap().is_some());
    }

    #[test]
    fn ranges() {
        let r = |h: &str| parse_range(Some(h), 100);
        assert_eq!(parse_range(None, 100), Want::Full);
        assert_eq!(r("bytes=0-9"), Want::Part { start: 0, end: 9 });
        assert_eq!(r("bytes=90-200"), Want::Part { start: 90, end: 99 });
        assert_eq!(r("bytes=10-"), Want::Part { start: 10, end: 99 });
        assert_eq!(r("bytes=-10"), Want::Part { start: 90, end: 99 });
        assert_eq!(r("bytes=-500"), Want::Part { start: 0, end: 99 });
        assert_eq!(r("BYTES=0-0"), Want::Part { start: 0, end: 0 });
        assert_eq!(r("bytes=100-"), Want::Unsatisfiable);
        assert_eq!(r("bytes=100-200"), Want::Unsatisfiable);
        assert_eq!(r("bytes=-0"), Want::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=0-"), 0), Want::Unsatisfiable);
        for ignored in [
            "bytes=0-1,5-6",
            "bytes=5-1",
            "bytes=a-b",
            "bytes=-",
            "bytes=1",
            "items=0-1",
            "bytes=+1-2",
            "",
        ] {
            assert_eq!(r(ignored), Want::Full, "{ignored}");
        }
    }

    #[test]
    fn etags() {
        assert_eq!(single_etag(Some("\"abc\"")), Some("abc".into()));
        assert_eq!(single_etag(Some("W/\"abc\"")), Some("abc".into()));
        assert_eq!(single_etag(Some("*")), None);
        assert_eq!(single_etag(Some("\"a\", \"b\"")), None);
        assert_eq!(single_etag(Some("abc")), None);
        assert_eq!(single_etag(None), None);
    }
}
