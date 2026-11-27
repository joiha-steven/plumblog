//! Downloads from GitHub Releases: HTTPS only, timeouts, no token ever (the repo is public),
//! and no redirects except GitHub's own hop from a release URL to its asset storage hosts.
//! The base is overridable only to a loopback test server, as the Cloudflare client's.

use crate::cloudflare::client::base_override_ok;
use std::time::Duration;

/// Where release files come from, outside tests.
pub const DOWNLOAD_BASE: &str = "https://github.com/joiha-steven/plumblog/releases/download";
pub const LATEST_URL: &str = "https://api.github.com/repos/joiha-steven/plumblog/releases/latest";
/// Shown to the owner when a download must be done by hand.
pub const RELEASES_PAGE: &str = "https://github.com/joiha-steven/plumblog/releases";
/// Test-only base override; accepted only for a loopback `http://` address.
pub const BASE_ENV: &str = "PLUMBLOG_RELEASE_BASE";
/// The only hosts a release download may be redirected to (GitHub's release asset storage).
pub const REDIRECT_HOSTS: &[&str] = &[
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];
const MAX_HOPS: usize = 3;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// A release archive is tens of MB at most; a slow line still finishes.
const CALL_TIMEOUT: Duration = Duration::from_mins(10);

#[derive(Debug, PartialEq, Eq)]
pub enum FetchError {
    /// `BASE_ENV` is not a loopback test server.
    BaseRefused(String),
    /// 404: the release or the file does not exist.
    NotFound(String),
    /// A redirect to a host outside [`REDIRECT_HOSTS`] (the host, or the refused URL's start).
    RedirectRefused(String),
    /// Another status, a network failure, or a body over the size limit: (url, detail).
    Failed(String, String),
}

#[derive(Clone, Debug)]
pub struct Source {
    download: String,
    latest: String,
    local: bool,
}

impl Source {
    #[must_use]
    pub fn github() -> Self {
        Self {
            download: DOWNLOAD_BASE.into(),
            latest: LATEST_URL.into(),
            local: false,
        }
    }

    /// A loopback test server: files at `<base>/download/vX.Y.Z/<file>`, latest at
    /// `<base>/latest`.
    ///
    /// # Errors
    /// `BaseRefused` for anything but `http://127.0.0.1[:port]` or `http://[::1][:port]`.
    pub fn local(base: &str) -> Result<Self, FetchError> {
        if !base_override_ok(base) {
            let shown: String = base.chars().filter(|c| !c.is_control()).collect();
            return Err(FetchError::BaseRefused(shown));
        }
        let b = base.trim_end_matches('/');
        Ok(Self {
            download: format!("{b}/download"),
            latest: format!("{b}/latest"),
            local: true,
        })
    }

    /// [`Source::github`], or [`Source::local`] from `PLUMBLOG_RELEASE_BASE` (empty = unset).
    ///
    /// # Errors
    /// As [`Source::local`].
    pub fn from_env() -> Result<Self, FetchError> {
        match std::env::var(BASE_ENV).ok().filter(|v| !v.is_empty()) {
            Some(b) => Self::local(&b),
            None => Ok(Self::github()),
        }
    }

    /// `tag` is a checked `vX.Y.Z`; `file` a fixed release file name.
    #[must_use]
    pub fn file_url(&self, tag: &str, file: &str) -> String {
        format!("{}/{tag}/{file}", self.download)
    }

    #[must_use]
    pub fn latest_url(&self) -> &str {
        &self.latest
    }

    fn agent(&self) -> ureq::Agent {
        let mut config = ureq::Agent::config_builder()
            .http_status_as_error(false)
            // Each hop is checked here instead (see `next_hop`).
            .max_redirects(0)
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_global(Some(CALL_TIMEOUT))
            .user_agent(concat!("plumblog-cli/", env!("CARGO_PKG_VERSION")))
            .https_only(!self.local);
        if self.local {
            config = config.proxy(None);
        }
        config.build().into()
    }

    /// The URL a redirect may go to, or why not.
    fn next_hop(&self, location: &str) -> Result<String, FetchError> {
        if self.local && base_override_ok(location.split(['?', '#']).next().unwrap_or("")) {
            return Ok(location.to_string());
        }
        let host = location
            .strip_prefix("https://")
            .map(|r| r.split(['/', '?', '#']).next().unwrap_or(""));
        match host {
            Some(h) if REDIRECT_HOSTS.contains(&h) => Ok(location.to_string()),
            Some(h) => Err(FetchError::RedirectRefused(clean(h))),
            None => Err(FetchError::RedirectRefused(clean(
                &location.chars().take(60).collect::<String>(),
            ))),
        }
    }

    /// GET `url`, following only allowed redirects; the body, at most `limit` bytes.
    ///
    /// # Errors
    /// [`FetchError`].
    pub fn get(&self, url: &str, accept: &str, limit: u64) -> Result<Vec<u8>, FetchError> {
        let agent = self.agent();
        let mut url = url.to_string();
        for _ in 0..=MAX_HOPS {
            let failed = |d: String| FetchError::Failed(strip_query(&url), d);
            let resp = agent
                .get(&url)
                .header("Accept", accept)
                .call()
                // ureq's text can hold the URL, query included (signed asset links).
                .map_err(|e| failed(e.to_string().replace(&url, &strip_query(&url))))?;
            let status = resp.status().as_u16();
            match status {
                200 => {
                    return resp
                        .into_body()
                        .into_with_config()
                        .limit(limit)
                        .read_to_vec()
                        .map_err(|e| failed(e.to_string()));
                }
                301 | 302 | 303 | 307 | 308 => {
                    let loc = resp
                        .headers()
                        .get("location")
                        .and_then(|v| v.to_str().ok())
                        .ok_or_else(|| failed(format!("HTTP {status} without Location")))?;
                    url = self.next_hop(loc)?;
                }
                404 => return Err(FetchError::NotFound(strip_query(&url))),
                s => return Err(failed(format!("HTTP {s}"))),
            }
        }
        Err(FetchError::Failed(
            strip_query(&url),
            "too many redirects".into(),
        ))
    }
}

fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

/// Signed asset URLs carry a token in the query; never print it.
fn strip_query(url: &str) -> String {
    clean(url.split(['?', '#']).next().unwrap_or(""))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirects_only_to_github_asset_hosts_over_https() {
        let gh = Source::github();
        assert!(
            gh.next_hop("https://objects.githubusercontent.com/x?sig=1")
                .is_ok()
        );
        assert!(
            gh.next_hop("https://release-assets.githubusercontent.com/x")
                .is_ok()
        );
        for bad in [
            "http://objects.githubusercontent.com/x",
            "https://objects.githubusercontent.com.evil.example/x",
            "https://objects.githubusercontent.com@evil.example/x",
            "https://evil.example/x",
            "http://127.0.0.1:1/x",
            "/relative",
        ] {
            assert!(gh.next_hop(bad).is_err(), "{bad}");
        }
        let local = Source::local("http://127.0.0.1:9").unwrap();
        assert!(local.next_hop("http://127.0.0.1:9/download/x").is_ok());
        assert!(local.next_hop("http://evil.example/x").is_err());
        assert!(Source::local("https://example.com").is_err());
        assert!(Source::local("http://127.0.0.1.evil.example").is_err());
    }
}
