//! Rendered pages in the Cache API. Keys carry the content version, so a publish makes every
//! old entry unreachable without purge calls. Any cache failure falls back to rendering: the
//! blog works without the cache, only slower. The theme tag (`crate::theme_runtime::cache_tag`)
//! is in the key too, so a theme push or rollback never serves the other theme's pages.
//! The Cache API is per data centre, so each miss also leaves a copy in R2 ([`store_key`]),
//! which crates/worker/js/entry.mjs reads, without wasm, wherever the Cache API misses.

use plumblog_model::ErrorCode;
use plumblog_syslog::{Event, EventKind, request_id};
use std::collections::HashMap;
use worker::{Cache, Context, Env, Headers, HttpMetadata, Response, console_error, console_warn};

/// Internal URL that names a cached page; never served. The query string is left out so
/// `?x=1` variants cannot multiply entries. `version_id` (the deployed Worker version) is
/// there because deploys do not bump the content version: without it every data centre kept
/// serving the old engine's or theme's pages until content changed. Without the binding (an
/// older config) the key has no id, so the cache still works. entry.mjs builds the same string.
pub fn key(
    origin: &str,
    version_id: Option<&str>,
    version: u64,
    theme_tag: &str,
    path: &str,
) -> String {
    match version_id {
        Some(id) => format!("{origin}/__page/{id}/v{version}{theme_tag}{path}"),
        None => format!("{origin}/__page/v{version}{theme_tag}{path}"),
    }
}

/// [`key`] for the running deployment: its version id and theme tag.
pub fn key_for(env: &Env, origin: &str, version: u64, path: &str) -> String {
    let id = crate::version_id(env);
    key(
        origin,
        id.as_deref(),
        version,
        crate::theme_runtime::cache_tag(),
        path,
    )
}

/// Largest page body copied to R2. Reader pages are tens of KiB; one near 1 MiB is far past
/// the 5,000-word render budget, and every miss would copy it out of wasm and upload it on a
/// cold isolate's CPU. Bigger pages still go to the Cache API.
pub const MAX_STORED_PAGE: usize = 1024 * 1024;
/// R2's key limit, in bytes: a longer key is skipped rather than failing and logging.
const MAX_R2_KEY: usize = 1024;

/// Name of a rendered page in R2 (bucket `MEDIA`, prefix `pages/`, which backups never copy).
/// No origin: R2 is global and pages never depend on the request's host. `version_id` (the
/// deployed Worker version) is there because deploys do not bump the content version, and a
/// new engine or theme must not serve the old one's pages. entry.mjs builds the same string;
/// tools/worker-test/r2pages.test.mjs pins the two together.
pub fn store_key(version_id: &str, version: u64, theme_tag: &str, path: &str) -> String {
    format!("pages/{version_id}/v{version}{theme_tag}{path}")
}

/// Whether a page the Cache API keeps may also go to R2. GET only: a HEAD has no body to
/// store. No credentials: reader pages ignore them today, and this keeps any page that ever
/// varies by viewer out of a store shared by every reader.
pub fn may_store(get: bool, credentials: bool, body_len: usize, key: &str, limit: usize) -> bool {
    get && !credentials && body_len <= limit && key.len() <= MAX_R2_KEY
}

/// One page for R2: the bytes and the headers a hit must send.
pub struct Stored {
    pub key: String,
    pub body: Vec<u8>,
    pub content_type: &'static str,
    pub etag: String,
    pub cache_control: &'static str,
    pub theme: Option<&'static str>,
    pub path: String,
    pub cf_ray: Option<String>,
}

pub enum Lookup {
    Hit(Response),
    Miss,
    /// The Cache API failed (or is unavailable); render and do not try to store.
    Bypass,
}

impl Lookup {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Hit(_) => "hit",
            Self::Miss => "miss",
            Self::Bypass => "bypass",
        }
    }
}

pub async fn get(key: &str) -> Lookup {
    match Cache::default().get(key, false).await {
        Ok(Some(cached)) => match relabel(cached) {
            Ok(resp) => Lookup::Hit(resp),
            Err(e) => {
                console_warn!("pages: cached copy unreadable, rendering: {e}");
                Lookup::Bypass
            }
        },
        Ok(None) => Lookup::Miss,
        Err(e) => {
            console_warn!("pages: cache lookup failed, rendering: {e}");
            Lookup::Bypass
        }
    }
}

/// Cached responses keep the headers of the render that stored them; mark this one a hit and
/// drop the stored render timing, which no longer describes this request. The body stays the
/// cache's stream: reading it into wasm and copying it back cost CPU on every hit.
fn relabel(cached: Response) -> worker::Result<Response> {
    let headers = Headers::new();
    for (k, v) in cached.headers() {
        if k != "x-plumblog-cache" && k != "server-timing" {
            headers.set(&k, &v)?;
        }
    }
    headers.set("x-plumblog-cache", "hit")?;
    Ok(cached.with_headers(headers))
}

/// Stores after the response is sent, so a slow cache or R2 write never delays the reader.
/// At most one R2 put per call, so per miss. The R2 put (or its failure log) finishes before
/// the Cache API put starts: a Cache API entry then proves this miss's R2 step is over, which
/// tests rely on to show a page was not stored without sleeping.
pub fn put_later(ctx: &Context, env: &Env, key: String, resp: Response, stored: Option<Stored>) {
    let env = env.clone();
    ctx.wait_until(async move {
        if let Some(mut s) = stored {
            // Moved, not cloned: it can be 1 MiB, and only the put needs it.
            let body = std::mem::take(&mut s.body);
            if let Err(e) = put_r2(&env, &s, body).await {
                store_failed(&env, &s, &e.to_string()).await;
            }
        }
        if let Err(e) = Cache::default().put(key.as_str(), resp).await {
            console_warn!("pages: cache store failed (page still served): {e}");
        }
    });
}

/// Binding that holds stored pages; a test build may name a missing one to make puts fail.
fn bucket_name(env: &Env) -> String {
    #[cfg(feature = "contract-tests")]
    if let Ok(v) = env.var("PLUMBLOG_TEST_PAGE_BUCKET") {
        return v.to_string();
    }
    let _ = env;
    "MEDIA".to_owned()
}

/// [`MAX_STORED_PAGE`], or in a test build a lower limit, so the size guard is testable
/// without a megabyte post.
pub fn store_limit(env: &Env) -> usize {
    #[cfg(feature = "contract-tests")]
    if let Some(n) = env
        .var("PLUMBLOG_TEST_PAGE_LIMIT")
        .ok()
        .and_then(|v| v.to_string().parse().ok())
    {
        return n;
    }
    let _ = env;
    MAX_STORED_PAGE
}

async fn put_r2(env: &Env, s: &Stored, body: Vec<u8>) -> worker::Result<()> {
    let mut meta = HashMap::from([
        ("etag".to_owned(), s.etag.clone()),
        ("cache-control".to_owned(), s.cache_control.to_owned()),
    ]);
    if let Some(t) = s.theme {
        meta.insert(
            plumblog_render::pushed::THEME_HEADER.to_owned(),
            t.to_owned(),
        );
    }
    env.bucket(&bucket_name(env))?
        .put(s.key.clone(), body)
        .http_metadata(HttpMetadata {
            content_type: Some(s.content_type.to_owned()),
            ..HttpMetadata::default()
        })
        .custom_metadata(meta)
        .execute()
        .await
        .map(drop)
}

/// The page was served; only the global copy is missing, so readers elsewhere render it.
async fn store_failed(env: &Env, s: &Stored, error: &str) {
    let rid = request_id(s.cf_ray.as_deref(), crate::media::random_id);
    console_error!(
        "[{}] {} path={} key={} error={error}",
        rid.as_str(),
        ErrorCode::PageStoreFailed.as_str(),
        s.path,
        s.key
    );
    let Ok(db) = env.d1("DB") else { return };
    let at = worker::Date::now().as_millis();
    let event = Event::new(EventKind::Error, rid, at)
        .error(ErrorCode::PageStoreFailed)
        .subject(&s.path)
        .field("error", error);
    plumblog_syslog::record(&crate::d1::D1Store::new(db), event, |m: &str| {
        console_error!("{m}");
    })
    .await;
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_store_key_has_no_origin_and_carries_the_deployed_version() {
        assert_eq!(
            super::store_key("abc-1", 7, "-t0123456789abcdef", "/a"),
            "pages/abc-1/v7-t0123456789abcdef/a"
        );
        assert_ne!(
            super::store_key("abc-1", 7, "", "/a"),
            super::store_key("abc-2", 7, "", "/a")
        );
    }

    #[test]
    fn only_anonymous_gets_of_bounded_pages_are_stored() {
        let key = super::store_key("id", 1, "", "/a");
        let limit = super::MAX_STORED_PAGE;
        assert!(super::may_store(true, false, limit, &key, limit));
        assert!(!super::may_store(false, false, 10, &key, limit), "HEAD");
        assert!(
            !super::may_store(true, true, 10, &key, limit),
            "credentials"
        );
        assert!(
            !super::may_store(true, false, limit + 1, &key, limit),
            "size"
        );
        let long = super::store_key("id", 1, "", &format!("/{}", "a".repeat(1024)));
        assert!(
            !super::may_store(true, false, 10, &long, limit),
            "R2 key limit"
        );
    }

    #[test]
    fn the_theme_is_part_of_the_key() {
        let k = |tag| super::key("https://b.test", None, 7, tag, "/a");
        assert_eq!(
            k(""),
            "https://b.test/__page/v7/a",
            "default theme, no version binding: keys unchanged"
        );
        assert_ne!(k("-t0123456789abcdef"), k(""));
        assert_ne!(k("-t0123456789abcdef"), k("-tfedcba9876543210"));
    }

    #[test]
    fn the_deployed_version_is_part_of_the_key() {
        let k = |id| super::key("https://b.test", id, 7, "-t0123456789abcdef", "/a");
        assert_eq!(
            k(Some("abc-1")),
            "https://b.test/__page/abc-1/v7-t0123456789abcdef/a"
        );
        assert_ne!(
            k(Some("abc-1")),
            k(Some("abc-2")),
            "a deploy retires the old pages"
        );
        assert_ne!(k(Some("abc-1")), k(None));
    }
}
