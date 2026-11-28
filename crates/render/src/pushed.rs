//! A theme pushed as a static asset: `/_theme/<name>/theme.json`, every text file of the
//! theme in one JSON document, so the Worker loads it with a single fetch. `blog theme push`
//! writes it with these types; the Worker reads it with [`PushedTheme::parse`].
//!
//! Files under the theme's `assets/` are not in the document: each is its own static asset at
//! `/_theme/<name>/assets/<path below assets/>`, listed in `assets` with its SHA-256 so a
//! later deploy can fetch the theme back from the site and check every byte.

use crate::theme::ThemeFiles;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Version of the `theme.json` shape. Bumped only on an incompatible change; a Worker refuses
/// any other value and keeps the embedded default theme.
pub const THEME_API: u32 = 1;

/// `theme.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushedTheme {
    pub api: u32,
    pub name: String,
    /// `theme.toml` `version`, for the log and the admin.
    pub version: String,
    /// SHA-256 hex of the file map: the theme's identity in page cache keys.
    pub hash: String,
    /// Relative path (`/`-separated) to UTF-8 text.
    pub files: BTreeMap<String, String>,
    /// Path below `assets/` to the SHA-256 hex of its bytes. Absent in documents written
    /// before it existed.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub assets: BTreeMap<String, String>,
}

/// Largest `theme.json` a Worker parses (docs/ARCHITECTURE.md, Budgets): about 3x the
/// default theme's 77 KB, and a cold load of about 2.5 ms natively, so it stays within one
/// request's CPU budget in wasm. `blog theme check` and `push` refuse bigger themes.
pub const MAX_DOCUMENT: usize = 256 * 1024;

/// Prefix of the theme folder whose files ship as separate static assets.
pub const ASSETS_DIR: &str = "assets/";

/// Static asset path of `theme.json` for theme `name`.
#[must_use]
pub fn document_path(name: &str) -> String {
    format!("/_theme/{name}/theme.json")
}

/// Static asset path of a file below the theme's `assets/` (`path` without that prefix).
#[must_use]
pub fn asset_path(name: &str, path: &str) -> String {
    format!("/_theme/{name}/assets/{path}")
}

/// URL of a file below the theme's `assets/` on the site: [`asset_path`] with each segment
/// percent-encoded (all but `A-Z a-z 0-9 - . _ ~`), so a name with a space, `#`, `%` or `?`
/// reaches the file it names. `blog theme check` refuses such names in new themes; themes
/// pushed before still have to come back.
#[must_use]
pub fn asset_url(name: &str, path: &str) -> String {
    let mut out = format!("/_theme/{name}/assets");
    for seg in path.split('/') {
        out.push('/');
        for b in seg.bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
                out.push(char::from(b));
            } else {
                const HEX: &[u8; 16] = b"0123456789ABCDEF";
                out.push('%');
                out.push(char::from(HEX[usize::from(b >> 4)]));
                out.push(char::from(HEX[usize::from(b & 15)]));
            }
        }
    }
    out
}

/// Response header on reader pages rendered with a pushed theme: [`theme_tag`] of its hash.
/// Absent when the embedded default rendered the page, so `blog deploy`'s smoke check can
/// tell a version that serves the pushed theme from one that fell back.
pub const THEME_HEADER: &str = "x-plumblog-theme";

/// The first 16 hex digits of a theme hash: what pages, cache keys and owners show.
#[must_use]
pub fn theme_tag(hash: &str) -> &str {
    hash.get(..16).unwrap_or(hash)
}

/// Theme names are URL path segments: lowercase letters, digits, `_` and `-`, starting with a
/// letter, at most 64 bytes (the Worker's `theme_runtime` rule).
#[must_use]
pub fn valid_name(n: &str) -> bool {
    n.len() <= 64
        && n.starts_with(|c: char| c.is_ascii_lowercase())
        && n.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

impl PushedTheme {
    /// What `hash` is the SHA-256 of (this crate has no hasher; the CLI and the Worker do):
    /// a version line, then per text file `f <path len> <path> <text len>\n<text>\n` and per
    /// asset `a <path len> <path> <sha256>\n`, each list in path order. Lengths make the
    /// encoding unambiguous; `name` and `version` are not part of it, so the hash names the
    /// content only.
    #[must_use]
    pub fn hash_input(&self) -> Vec<u8> {
        let mut out = b"plumblog-theme 1\n".to_vec();
        for (p, t) in &self.files {
            out.extend_from_slice(format!("f {} {p} {}\n", p.len(), t.len()).as_bytes());
            out.extend_from_slice(t.as_bytes());
            out.push(b'\n');
        }
        for (p, h) in &self.assets {
            out.extend_from_slice(format!("a {} {p} {h}\n", p.len()).as_bytes());
        }
        out
    }

    /// `theme.json` as `blog theme push` writes it.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// Parses `theme.json` for the theme the Worker was told to use.
    ///
    /// # Errors
    /// Not JSON of this shape, another `api`, another `name`, a hash that is not 64 lowercase
    /// hex digits, or an unsafe file path.
    pub fn parse(json: &str, expected_name: &str) -> Result<Self, String> {
        let t: Self = serde_json::from_str(json).map_err(|e| format!("theme.json: {e}"))?;
        if t.api != THEME_API {
            return Err(format!(
                "theme.json: api {} is not {THEME_API}; push the theme again with a matching blog",
                t.api
            ));
        }
        if t.name != expected_name {
            return Err(format!(
                "theme.json: name {:?} is not the active theme {expected_name:?}",
                t.name
            ));
        }
        if !is_hex64(&t.hash) {
            return Err("theme.json: hash is not 64 lowercase hex digits".to_owned());
        }
        if let Some(p) = t
            .files
            .keys()
            .chain(t.assets.keys())
            .find(|p| !safe_path(p))
        {
            return Err(format!(
                "theme.json: file path {p:?} is not a plain relative path"
            ));
        }
        if let Some(p) = t.assets.iter().find(|(_, h)| !is_hex64(h)).map(|(p, _)| p) {
            return Err(format!("theme.json: asset {p:?} has no SHA-256"));
        }
        Ok(t)
    }
}

fn is_hex64(h: &str) -> bool {
    h.len() == 64
        && h.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `a/b.css`: no empty, `.` or `..` segment, no backslash or leading `/`.
fn safe_path(p: &str) -> bool {
    !p.contains('\\') && p.split('/').all(|s| !s.is_empty() && s != "." && s != "..")
}

impl ThemeFiles for PushedTheme {
    fn read(&self, path: &str) -> Option<&str> {
        self.files.get(path).map(String::as_str)
    }
    fn paths(&self) -> Vec<&str> {
        self.files.keys().map(String::as_str).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{PushedTheme, THEME_API};

    fn doc(api: u32, name: &str, hash: &str, path: &str) -> String {
        serde_json::json!({
            "api": api, "name": name, "version": "1.0.0", "hash": hash,
            "files": { path: "x" }
        })
        .to_string()
    }

    const HASH: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn parses_the_contract_and_refuses_anything_else() {
        let ok = PushedTheme::parse(&doc(THEME_API, "paper", HASH, "theme.toml"), "paper").unwrap();
        assert_eq!(ok.files["theme.toml"], "x");
        let bad = [
            (doc(THEME_API + 1, "paper", HASH, "theme.toml"), "api"),
            (doc(THEME_API, "other", HASH, "theme.toml"), "name"),
            (doc(THEME_API, "paper", "ABC", "theme.toml"), "hash"),
            (doc(THEME_API, "paper", &HASH.to_uppercase(), "a"), "hash"),
            (doc(THEME_API, "paper", HASH, "../x"), "path"),
            (doc(THEME_API, "paper", HASH, "/x"), "path"),
            (doc(THEME_API, "paper", HASH, "a//b"), "path"),
            ("{".to_owned(), "theme.json"),
            (r#"{"api":1}"#.to_owned(), "missing"),
        ];
        for (json, why) in bad {
            let e = PushedTheme::parse(&json, "paper").unwrap_err();
            assert!(e.contains(why), "{json}: {e}");
        }
        // Fields added later stay readable by this Worker (only `api` breaks compatibility).
        let extra = doc(THEME_API, "paper", HASH, "a").replace("{\"api\"", "{\"x\":1,\"api\"");
        assert!(
            PushedTheme::parse(&extra, "paper").is_ok(),
            "unknown fields"
        );
        let asset = |path: &str, sha: &str| {
            let mut v: serde_json::Value =
                serde_json::from_str(&doc(THEME_API, "paper", HASH, "a")).unwrap();
            v["assets"] = serde_json::json!({ path: sha });
            v.to_string()
        };
        assert!(PushedTheme::parse(&asset("fonts/a.woff2", HASH), "paper").is_ok());
        assert!(
            PushedTheme::parse(&asset("../a.woff2", HASH), "paper")
                .unwrap_err()
                .contains("path")
        );
        assert!(
            PushedTheme::parse(&asset("a.woff2", "x"), "paper")
                .unwrap_err()
                .contains("SHA-256")
        );
    }

    #[test]
    fn asset_urls_encode_each_segment() {
        assert_eq!(
            super::asset_url("paper", "fonts/inter-latin.woff2"),
            "/_theme/paper/assets/fonts/inter-latin.woff2"
        );
        assert_eq!(
            super::asset_url("paper", "my dir/a#1 100%?.png"),
            "/_theme/paper/assets/my%20dir/a%231%20100%25%3F.png"
        );
        assert_eq!(
            super::asset_url("paper", "caf\u{e9}.png"),
            "/_theme/paper/assets/caf%C3%A9.png"
        );
    }

    #[test]
    fn hash_input_tells_every_split_apart() {
        let t = |files: &[(&str, &str)], assets: &[(&str, &str)]| {
            PushedTheme {
                api: THEME_API,
                name: "paper".into(),
                version: "1".into(),
                hash: HASH.into(),
                files: files
                    .iter()
                    .map(|(a, b)| ((*a).into(), (*b).into()))
                    .collect(),
                assets: assets
                    .iter()
                    .map(|(a, b)| ((*a).into(), (*b).into()))
                    .collect(),
            }
            .hash_input()
        };
        let base = t(&[("a", "xy")], &[]);
        assert_ne!(base, t(&[("a", "x"), ("y", "")], &[]));
        assert_ne!(base, t(&[("a", "xy")], &[("f", HASH)]));
        assert_ne!(t(&[], &[("f", HASH)]), t(&[], &[("g", HASH)]));
        // Name and version are labels, not content.
        let mut renamed = PushedTheme::parse(&doc(THEME_API, "paper", HASH, "a"), "paper").unwrap();
        let before = renamed.hash_input();
        renamed.name = "other".into();
        renamed.version = "9".into();
        assert_eq!(before, renamed.hash_input());
    }
}
