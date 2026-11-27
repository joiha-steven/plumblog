//! Markdown to HTML on save. For now only the output shape and heading ids; the renderer
//! (pulldown-cmark + own extensions) lands with its own issue.

/// What saving a post stores, so reading never parses Markdown.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Rendered {
    pub html: String,
    pub toc: Vec<TocEntry>,
    pub excerpt: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TocEntry {
    pub level: u8,
    pub id: String,
    pub text: String,
}

use unicode_normalization::UnicodeNormalization;

/// Anchor id for a heading. Letters of any script are kept (HTML ids allow them), so
/// Vietnamese headings stay readable in links instead of collapsing to dashes. Input is
/// NFC-normalised first: otherwise decomposed text (macOS input) loses its combining marks.
#[must_use]
pub fn heading_id(text: &str) -> String {
    let mut id = String::with_capacity(text.len());
    for c in text.nfc().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            id.push(c);
        } else if !id.is_empty() && !id.ends_with('-') {
            id.push('-');
        }
    }
    while id.ends_with('-') {
        id.pop();
    }
    if id.is_empty() {
        id.push_str("section");
    }
    id
}

#[cfg(test)]
mod tests {
    use super::heading_id;

    #[test]
    fn heading_ids_are_stable_and_keep_letters() {
        assert_eq!(heading_id("Hello, World!"), "hello-world");
        assert_eq!(heading_id("  Rust  &  wasm  "), "rust-wasm");
        assert_eq!(
            heading_id("Ti\u{1ebf}ng Vi\u{1ec7}t"),
            "ti\u{1ebf}ng-vi\u{1ec7}t"
        );
        // NFD "Ti\u{1ebf}ng": e + circumflex + acute as separate code points.
        assert_eq!(
            heading_id("Tie\u{302}\u{301}ng"),
            heading_id("Ti\u{1ebf}ng")
        );
        assert_eq!(heading_id("!!!"), "section");
    }
}
