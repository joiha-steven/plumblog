//! Authored TOML to the JSON the loaders read (feature `toml`, off for the Worker).
//!
//! The Worker links no TOML parser, so `theme.toml`, `plugin.toml` and `locales/*.toml` reach
//! it as JSON under the same name plus `.json` (`theme.toml.json`). `blog` converts when it
//! checks and pushes a theme; the Worker's build script converts what it embeds. Both use
//! [`json_name`] and [`file`], so there is one rule and one converter.

use crate::{plugin, theme};

/// The key a TOML file has at runtime: its path plus `.json`. `None` for any other file.
#[must_use]
pub fn json_name(path: &str) -> Option<String> {
    // Exactly `.toml`: the loaders look for these names, so a differently cased one is not TOML.
    path.strip_suffix(".toml").map(|_| format!("{path}.json"))
}

/// The JSON for the TOML file `path` (relative to its theme or plugin folder).
///
/// A manifest is also read as the type the loader expects, so a misspelt or mistyped key is
/// reported with the TOML parser's line and column, naming the file, as an author expects.
///
/// # Errors
/// What is wrong with the file, prefixed with its path.
pub fn file(path: &str, src: &str) -> Result<String, String> {
    let shape = match path {
        "theme.toml" => toml::from_str::<theme::Manifest>(src).map(drop),
        "plugin.toml" => toml::from_str::<plugin::Manifest>(src).map(drop),
        _ => Ok(()),
    };
    shape.map_err(|e| format!("{path}: {e}"))?;
    plumblog_i18n::toml_to_json(src).map_err(|e| format!("{path}: {}", e.detail))
}
