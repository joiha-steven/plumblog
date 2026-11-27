//! The site's configured origin (settings `origin`, written by `blog setup`): the passkey
//! relying party (rp id and expected `clientDataJSON.origin`), the host a Turnstile token must
//! name, and the CSRF reference.
//!
//! There is no fallback to the request for passkeys or Turnstile: an rp id is permanent (every
//! passkey is bound to it) and the Host header is the client's to choose. Without a configured
//! origin those refuse with `settings.origin_missing`; password sign-in and setup still work.

use plumblog_model::{Error, ErrorCode, Result};
use plumblog_store::{ORIGIN_KEY, SettingsStore};

use super::Ctx;
use super::body::json_string;

pub struct Site {
    /// `https://blog.example` (scheme, host, port when not the default; no slash).
    pub origin: String,
    /// `blog.example`: the rp id and the Turnstile hostname.
    pub host: String,
}

/// Hosts where plain `http` is allowed (local development).
const LOCAL_HOSTS: &[&str] = &["localhost", "127.0.0.1", "[::1]"];

/// A stored origin, strictly: `https://host[:port]` (or `http` for a local host), lowercase,
/// nothing after it but an optional `/`. The text must already be in canonical form, so what
/// is stored is exactly what browsers report as `origin`.
fn parse(text: &str) -> std::result::Result<Site, String> {
    let url = worker::Url::parse(text).map_err(|e| e.to_string())?;
    let host = url.host_str().unwrap_or_default().to_owned();
    match url.scheme() {
        "https" => {}
        "http" if LOCAL_HOSTS.contains(&host.as_str()) => {}
        other => {
            return Err(format!(
                "scheme {other:?} is not https (http only for localhost)"
            ));
        }
    }
    if host.is_empty() || host.ends_with('.') {
        return Err("has no host, or one ending in a dot".to_owned());
    }
    let origin = url.origin().ascii_serialization();
    // Catches userinfo, paths, queries, fragments, upper case, default ports and IDN forms in
    // one rule: the text must be the origin itself.
    if text != origin && text != format!("{origin}/") {
        return Err(format!("is not a canonical origin (want {origin:?})"));
    }
    Ok(Site { origin, host })
}

/// The configured origin, `None` when the site has none.
///
/// # Errors
///
/// `Internal` when the stored value is not an acceptable origin (logged, never guessed past);
/// storage errors pass through.
pub async fn configured(ctx: &Ctx) -> Result<Option<Site>> {
    let Some(raw) = ctx.store.get_setting(ORIGIN_KEY).await? else {
        return Ok(None);
    };
    let text = json_string(&raw).unwrap_or_default();
    parse(&text).map(Some).map_err(|why| {
        Error::new(
            ErrorCode::Internal,
            format!("setting {ORIGIN_KEY} {raw:?} {why}; fix it with blog setup"),
        )
    })
}

/// The configured origin, required (passkeys, Turnstile).
///
/// # Errors
///
/// `OriginMissing` (logged) when none is configured; otherwise as [`configured`].
pub async fn site(ctx: &Ctx) -> Result<Site> {
    configured(ctx).await?.ok_or_else(|| {
        Error::new(
            ErrorCode::OriginMissing,
            "settings.origin_missing: passkeys and Turnstile need the site origin; set it with blog setup",
        )
    })
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn accepts_canonical_origins() {
        for (text, origin, host) in [
            (
                "https://blog.example",
                "https://blog.example",
                "blog.example",
            ),
            (
                "https://blog.example/",
                "https://blog.example",
                "blog.example",
            ),
            (
                "https://blog.example:8443",
                "https://blog.example:8443",
                "blog.example",
            ),
            (
                "http://localhost:8787",
                "http://localhost:8787",
                "localhost",
            ),
            (
                "http://127.0.0.1:8787",
                "http://127.0.0.1:8787",
                "127.0.0.1",
            ),
            ("http://[::1]:8787", "http://[::1]:8787", "[::1]"),
        ] {
            let s = parse(text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(
                (s.origin.as_str(), s.host.as_str()),
                (origin, host),
                "{text}"
            );
        }
    }

    #[test]
    fn refuses_hostile_values() {
        for bad in [
            "",
            "blog.example",
            "//blog.example",
            "http://blog.example",
            "http://localhost.evil.example",
            "http://127.0.0.2",
            "ftp://blog.example",
            "javascript:alert(1)",
            "data:text/html,x",
            "file:///etc/passwd",
            "https://",
            "https://u:p@blog.example",
            "https://u@blog.example",
            "https://blog.example/path",
            "https://blog.example//",
            "https://blog.example/?q",
            "https://blog.example?q",
            "https://blog.example/#f",
            "https://blog.example#f",
            "https://Blog.Example",
            "HTTPS://blog.example",
            "https://blog.example:443",
            "https://blog.example.",
            "https://b\u{fc}cher.example",
            " https://blog.example",
            "https://blog.example ",
            "https://blog.example\n",
            "https://blog.example\\@evil.example",
            "https://evil.example\\.blog.example",
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }
}
