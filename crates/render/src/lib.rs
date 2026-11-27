//! Theme runtime: loads a theme (theme.toml, templates, locales, CSS) and renders reader pages
//! from template data. No I/O: theme files come in through [`ThemeFiles`], so the Worker can
//! feed embedded files today and R2 bundles later, and tests feed plain maps.

mod css;
mod data;
pub mod feeds;
mod json;
mod keys;
pub mod options;
mod plugin;
mod seo;
mod theme;

pub use data::{Alternate, Body, Image, Link, NavItem, PageInfo, Post, Site};
pub use plugin::Plugin;
pub use theme::{Report, Theme, ThemeError, ThemeFiles};

/// Flattens a locale TOML file into `section.key` strings (core `locales/` use the same shape).
///
/// # Errors
/// Invalid TOML or a value that is not text, naming the key.
pub fn parse_locale(src: &str) -> Result<std::collections::HashMap<String, String>, String> {
    plumblog_i18n::parse(src).map_err(|e| e.detail)
}

/// Reader page kinds; each maps to one theme template.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageKind {
    Home,
    Post,
    Tag,
    Archive,
    Search,
    NotFound,
}

impl PageKind {
    #[must_use]
    pub const fn template(self) -> &'static str {
        match self {
            Self::Home => "home.html",
            Self::Post => "post.html",
            Self::Tag => "tag.html",
            Self::Archive => "archive.html",
            Self::Search => "search.html",
            Self::NotFound => "404.html",
        }
    }

    /// Exposed to templates as `page.kind`, so a shared layout can mark the current page.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Home => "home",
            Self::Post => "post",
            Self::Tag => "tag",
            Self::Archive => "archive",
            Self::Search => "search",
            Self::NotFound => "404",
        }
    }
}

/// Plugin slots a theme exposes with `{{ slot("<name>") }}` (docs/ARCHITECTURE.md, Themes).
pub const SLOTS: &[&str] = &[
    "head",
    "body_start",
    "body_end",
    "before_post",
    "after_post",
    "footer",
];
