//! A theme pushed as a static asset: `/_theme/<name>/theme.json`, every text file of the
//! theme in one JSON document, so the Worker loads it with a single fetch. `blog theme push`
//! writes it with these types; the Worker reads it with [`PushedTheme::parse`].

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
}

impl PushedTheme {
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
        if t.hash.len() != 64
            || !t
                .hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err("theme.json: hash is not 64 lowercase hex digits".to_owned());
        }
        if let Some(p) = t.files.keys().find(|p| !safe_path(p)) {
            return Err(format!(
                "theme.json: file path {p:?} is not a plain relative path"
            ));
        }
        Ok(t)
    }
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
    }
}
