//! Rendered pages in the Cache API. Keys carry the content version, so a publish makes every
//! old entry unreachable without purge calls. Any cache failure falls back to rendering: the
//! blog works without the cache, only slower. The theme tag (`crate::theme_runtime::cache_tag`)
//! is in the key too, so a theme push or rollback never serves the other theme's pages.

use worker::{Cache, Context, Headers, Response, console_warn};

/// Internal URL that names a cached page; never served. The query string is left out so
/// `?x=1` variants cannot multiply entries.
pub fn key(origin: &str, version: u64, theme_tag: &str, path: &str) -> String {
    format!("{origin}/__page/v{version}{theme_tag}{path}")
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
        Ok(Some(mut cached)) => match relabel(&mut cached).await {
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
/// drop the stored render timing, which no longer describes this request.
async fn relabel(cached: &mut Response) -> worker::Result<Response> {
    let headers = Headers::new();
    for (k, v) in cached.headers() {
        if k != "x-plumblog-cache" && k != "server-timing" {
            headers.set(&k, &v)?;
        }
    }
    headers.set("x-plumblog-cache", "hit")?;
    let status = cached.status_code();
    let body = cached.text().await?;
    Ok(Response::ok(body)?
        .with_status(status)
        .with_headers(headers))
}

/// Stores after the response is sent, so a slow cache write never delays the reader.
pub fn put_later(ctx: &Context, key: String, resp: Response) {
    ctx.wait_until(async move {
        if let Err(e) = Cache::default().put(key.as_str(), resp).await {
            console_warn!("pages: cache store failed (page still served): {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_theme_is_part_of_the_key() {
        let k = |tag| super::key("https://b.test", 7, tag, "/a");
        assert_eq!(
            k(""),
            "https://b.test/__page/v7/a",
            "default theme: keys unchanged"
        );
        assert_ne!(k("-t0123456789abcdef"), k(""));
        assert_ne!(k("-t0123456789abcdef"), k("-tfedcba9876543210"));
    }
}
