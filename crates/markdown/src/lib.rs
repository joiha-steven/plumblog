//! Markdown to HTML on save (docs/ARCHITECTURE.md, Content > Markdown). Reading a post never
//! parses Markdown: the worker stores `Rendered` and serves it.
//!
//! The HTML follows the theme renderer contract (docs/ARCHITECTURE.md, Themes). Raw HTML in
//! the source is dropped, every URL is checked against a scheme allowlist, and no element
//! makes a third-party request until the reader clicks it.

mod comment;
mod html;
mod link;
mod mathml;
mod text;

pub use comment::{COMMENT_LINK_REL, CommentHtml, render_comment};
pub use text::{heading_id, word_count};

/// A TeX formula that did not render.
pub const MATH_INVALID: &str = "markdown.math_invalid";

/// Every [`Warning::code`] the renderer can emit; the API reference and admin types list them.
pub const WARNING_CODES: &[&str] = &[MATH_INVALID];

/// Knobs that change the output. The excerpt length is the theme option `excerpt_words`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Options {
    pub excerpt_words: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self { excerpt_words: 55 }
    }
}

/// What saving a post stores.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rendered {
    pub html: String,
    pub toc: Vec<TocEntry>,
    /// Plain text cut at a word boundary, ending in an ellipsis when cut. Escape before output.
    pub excerpt: String,
    /// 0 for an empty post, else at least 1.
    pub reading_minutes: u32,
    /// Pages load extra assets only when a post needs them.
    pub has_code: bool,
    pub has_math: bool,
    pub has_embed: bool,
    /// Problems the author should fix; the post still renders. Shown in the editor.
    pub warnings: Vec<Warning>,
}

/// `code` is stable and maps to the locale key `warning.<code>`; `detail` is for logs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning {
    pub code: &'static str,
    pub detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TocEntry {
    pub level: u8,
    pub id: String,
    pub text: String,
}

/// Renders a post. Never fails: any input is valid Markdown. Invalid `LaTeX` keeps its source
/// visible in place and is reported in `warnings`.
#[must_use]
pub fn render(src: &str, options: Options) -> Rendered {
    html::render(src, options)
}
