//! UI text lookup. Locale file parsing lands with its own issue; this fixes the lookup rules.

use std::collections::HashMap;

/// All six CLDR plural categories, so adding a language (Russian, Arabic...) never changes
/// this API; each language's rules pick the subset it needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Plural {
    Zero,
    One,
    Two,
    Few,
    Many,
    Other,
}

/// Plural category for `n` in `lang`. Unknown languages use English rules, which at worst
/// shows "1 posts"-style text rather than nothing.
#[must_use]
pub fn plural(lang: &str, n: u64) -> Plural {
    match lang {
        // Vietnamese has no grammatical plural.
        "vi" => Plural::Other,
        _ if n == 1 => Plural::One,
        _ => Plural::Other,
    }
}

/// One language's strings with a fallback catalog (English) behind it.
#[derive(Debug, Default)]
pub struct Catalog {
    strings: HashMap<String, String>,
    fallback: HashMap<String, String>,
}

impl Catalog {
    #[must_use]
    pub fn new(strings: HashMap<String, String>, fallback: HashMap<String, String>) -> Self {
        Self { strings, fallback }
    }

    /// A missing key returns the key itself, so a gap is visible on screen and in review
    /// instead of rendering as empty text.
    #[must_use]
    pub fn get<'a>(&'a self, key: &'a str) -> &'a str {
        self.strings
            .get(key)
            .or_else(|| self.fallback.get(key))
            .map_or(key, String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::{Catalog, Plural, plural};
    use std::collections::HashMap;

    fn map(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn lookup_falls_back_to_english_then_to_the_key() {
        let c = Catalog::new(
            map(&[("post.read_more", "\u{110}\u{1ecd}c ti\u{1ebf}p")]),
            map(&[("post.read_more", "Read more"), ("post.share", "Share")]),
        );
        assert_eq!(c.get("post.read_more"), "\u{110}\u{1ecd}c ti\u{1ebf}p");
        assert_eq!(c.get("post.share"), "Share");
        assert_eq!(c.get("post.missing"), "post.missing");
        assert_eq!(plural("en", 1), Plural::One);
        assert_eq!(plural("vi", 1), Plural::Other);
    }
}
