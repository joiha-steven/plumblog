//! UI text: locale files, lookup with English fallback, CLDR plurals and `{name}` arguments.
//!
//! Locale files are TOML. Sections nest keys (`[post] next = "Next"` is `post.next`). A
//! plural message is a table of CLDR categories and must have `other`:
//! `reading_time = { one = "{count} minute", other = "{count} minutes" }`; it is chosen by
//! the `count` argument. Output is plain text: escaping belongs to the template layer.

mod plural;

pub use plural::{Plural, categories, plural};

use plumblog_model::{Error, ErrorCode, Result};
use std::collections::HashMap;

/// Flattens a locale file into `section.key -> text`; plural messages become
/// `key.<category>`. Anything but text, sections and plural tables is an error with the key,
/// because a number or list where text belongs is a mistake the author must see.
///
/// # Errors
///
/// [`ErrorCode::InvalidInput`] for invalid TOML, a non-text value, or a plural table
/// without `other`.
pub fn parse(src: &str) -> Result<HashMap<String, String>> {
    let table: toml::Table =
        toml::from_str(src).map_err(|e| Error::new(ErrorCode::InvalidInput, e.to_string()))?;
    let mut out = HashMap::new();
    walk("", &table, &mut out)?;
    Ok(out)
}

fn walk(prefix: &str, table: &toml::Table, out: &mut HashMap<String, String>) -> Result<()> {
    for (k, v) in table {
        let key = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match v {
            toml::Value::String(s) => {
                out.insert(key, s.clone());
            }
            toml::Value::Table(t) if is_plural_table(t) => {
                if !t.contains_key("other") {
                    return Err(Error::new(
                        ErrorCode::InvalidInput,
                        format!("{key}: plural message needs an `other` form"),
                    ));
                }
                walk(&key, t, out)?;
            }
            toml::Value::Table(t) => walk(&key, t, out)?,
            _ => {
                return Err(Error::new(
                    ErrorCode::InvalidInput,
                    format!("{key}: locale values must be text"),
                ));
            }
        }
    }
    Ok(())
}

/// A table whose keys are all plural categories is one message, not a section.
fn is_plural_table(t: &toml::Table) -> bool {
    !t.is_empty()
        && t.iter()
            .all(|(k, v)| Plural::from_name(k).is_some() && v.is_str())
}

/// One language's strings with English behind it.
#[derive(Debug, Default)]
pub struct Catalog {
    lang: String,
    strings: HashMap<String, String>,
    fallback: HashMap<String, String>,
}

impl Catalog {
    /// `fallback` is the English catalog's strings. Plural rules default to English; call
    /// [`Catalog::with_lang`] for any other language.
    #[must_use]
    pub fn new(strings: HashMap<String, String>, fallback: HashMap<String, String>) -> Self {
        Self {
            lang: "en".to_owned(),
            strings,
            fallback,
        }
    }

    /// Sets the language whose plural rules pick a message's form (`ru`, `pt-BR`...).
    #[must_use]
    pub fn with_lang(mut self, lang: &str) -> Self {
        lang.clone_into(&mut self.lang);
        self
    }

    /// Raw text for `key`. A missing key returns the key itself, so a gap is visible on
    /// screen and in review instead of rendering as empty text.
    #[must_use]
    pub fn get<'a>(&'a self, key: &'a str) -> &'a str {
        self.strings
            .get(key)
            .or_else(|| self.fallback.get(key))
            .map_or(key, String::as_str)
    }

    /// Text for `key` with `{name}` placeholders filled from `args`. A plural message picks
    /// its form from the `count` argument: this language's form, else its `other`, then the
    /// same in English.
    #[must_use]
    pub fn t(&self, key: &str, args: &[(&str, String)]) -> String {
        let count = args
            .iter()
            .find(|(k, _)| *k == "count")
            .and_then(|(_, v)| v.parse::<u64>().ok());
        let text = count
            .and_then(|n| {
                Self::plural_form(&self.strings, &self.lang, key, n)
                    .or_else(|| Self::plural_form(&self.fallback, "en", key, n))
            })
            .unwrap_or_else(|| self.get(key));
        fill(text, args)
    }

    fn plural_form<'a>(
        map: &'a HashMap<String, String>,
        lang: &str,
        key: &str,
        n: u64,
    ) -> Option<&'a str> {
        let form = plural(lang, n).name();
        map.get(&format!("{key}.{form}"))
            .or_else(|| map.get(&format!("{key}.other")))
            .map(String::as_str)
    }
}

/// Replaces each `{name}` with its argument. Unknown placeholders stay as written, so a
/// missing argument shows on the page instead of vanishing.
#[must_use]
pub fn fill(text: &str, args: &[(&str, String)]) -> String {
    if !text.contains('{') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len() + 16);
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let name = &after[..end];
        if let Some((_, v)) = args.iter().find(|(k, _)| *k == name) {
            out.push_str(v);
        } else {
            out.push('{');
            out.push_str(name);
            out.push('}');
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::{Catalog, fill, parse};
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
    }

    #[test]
    fn parse_flattens_sections_and_plural_tables() {
        let m = parse(
            "top = \"a\"\n[post]\nnext = \"Next\"\nmins = { one = \"{count} min\", other = \"{count} mins\" }\n[x.y]\nz = \"deep\"\n",
        )
        .unwrap();
        assert_eq!(m["top"], "a");
        assert_eq!(m["post.next"], "Next");
        assert_eq!(m["post.mins.one"], "{count} min");
        assert_eq!(m["x.y.z"], "deep");
        assert!(
            parse("[post]\nn = 3\n")
                .unwrap_err()
                .detail
                .contains("post.n")
        );
        assert!(
            parse("[post]\nm = { one = \"x\" }\n")
                .unwrap_err()
                .detail
                .contains("other")
        );
        assert!(parse("not toml =").is_err());
    }

    #[test]
    fn plural_messages_pick_the_language_form_then_fall_back() {
        let en =
            parse("[c]\nposts = { one = \"{count} post\", other = \"{count} posts\" }\n").unwrap();
        let ru = parse(
            "[c]\nposts = { one = \"{count} \u{437}\u{430}\u{43f}\u{438}\u{441}\u{44c}\", few = \"{count} \u{437}\u{430}\u{43f}\u{438}\u{441}\u{438}\", many = \"{count} \u{437}\u{430}\u{43f}\u{438}\u{441}\u{435}\u{439}\", other = \"{count} \u{437}\u{430}\u{43f}\u{438}\u{441}\u{438}\" }\n",
        )
        .unwrap();
        let ru = Catalog::new(ru, en.clone()).with_lang("ru");
        let n = |c: u64| [("count", c.to_string())];
        assert!(
            ru.t("c.posts", &n(1))
                .ends_with("\u{437}\u{430}\u{43f}\u{438}\u{441}\u{44c}")
        );
        assert!(
            ru.t("c.posts", &n(3))
                .ends_with("\u{437}\u{430}\u{43f}\u{438}\u{441}\u{438}")
        );
        assert!(
            ru.t("c.posts", &n(11))
                .ends_with("\u{437}\u{430}\u{43f}\u{438}\u{441}\u{435}\u{439}")
        );
        let vi =
            Catalog::new(map(&[("c.posts.other", "{count} b\u{e0}i")]), en.clone()).with_lang("vi");
        assert_eq!(vi.t("c.posts", &n(1)), "1 b\u{e0}i");
        // A language without the message uses English forms.
        let fr = Catalog::new(HashMap::new(), en).with_lang("fr");
        assert_eq!(fr.t("c.posts", &n(1)), "1 post");
        assert_eq!(fr.t("c.posts", &n(2)), "2 posts");
        // No count: the plain key path, visible when missing.
        assert_eq!(fr.t("c.posts", &[]), "c.posts");
    }

    #[test]
    fn fill_keeps_unknown_placeholders_and_does_not_escape() {
        let args = [("minutes", "4".to_owned()), ("t", "<b>".to_owned())];
        assert_eq!(fill("{minutes} min read", &args), "4 min read");
        assert_eq!(fill("{missing} x", &args), "{missing} x");
        assert_eq!(fill("open { brace", &args), "open { brace");
        assert_eq!(fill("{t}", &args), "<b>");
    }
}
