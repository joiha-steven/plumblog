//! UI text: locale files, lookup with English fallback, CLDR plurals and `{name}` arguments.
//!
//! Locale files are TOML. Sections nest keys (`[post] next = "Next"` is `post.next`). A
//! plural message is a table of CLDR categories that includes `other` (a table without
//! `other` is an ordinary section): `reading_time = { one = "{count} minute", other =
//! "{count} minutes" }`, chosen by the `count` argument. Output is plain text: escaping
//! belongs to the template layer.
//!
//! Rust code names keys only through [`Key`] constants in its crate's `keys.rs`, which the
//! repository locale check reads; templates pass literal keys to `t()`.

pub mod admin;
mod plural;

pub use plural::{Plural, categories, plural};

use plumblog_model::{Error, ErrorCode, Result};
use std::collections::{BTreeSet, HashMap};
use std::sync::Mutex;

/// A locale key used from Rust, made only by [`key!`] from a string literal. The field is
/// private, so no constant, array element or `format!` can become a key: every key Rust can
/// ask for is a literal the repository locale check reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key(&'static str);

impl Key {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.0
    }

    /// Only for [`key!`]; `scripts/check` fails if any other code calls it.
    #[doc(hidden)]
    #[must_use]
    pub const fn __from_literal(key: &'static str) -> Self {
        Self(key)
    }
}

/// A [`Key`] from a plain string literal: `key!("date.short")`.
#[macro_export]
macro_rules! key {
    ($key:literal) => {
        $crate::Key::__from_literal($key)
    };
}

/// Keys a page asked for that exist in neither its language nor English. Shared by the
/// lookups of one render; the caller drains it and logs `i18n.missing_key` once per key.
#[derive(Debug, Default)]
pub struct MissingKeys(Mutex<BTreeSet<(String, String)>>);

impl MissingKeys {
    fn record(&self, lang: &str, key: &str) {
        // A poisoned lock only means another lookup panicked; the set is still usable.
        let mut set = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        set.insert((lang.to_owned(), key.to_owned()));
    }

    /// `(lang, key)` pairs seen since the last call, each once.
    #[must_use]
    pub fn take(&self) -> Vec<(String, String)> {
        let mut set = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::take(&mut *set).into_iter().collect()
    }
}

/// Flattens a locale file into `section.key -> text`; a plural message's forms become
/// `key.<category>`. Anything but text and tables is an error naming the key, because a
/// number or list where text belongs is a mistake the author must see.
///
/// # Errors
///
/// [`ErrorCode::InvalidInput`] for invalid TOML or a non-text value.
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

    /// Stored text for `key` in this language, else English, else the key itself (so a gap
    /// is visible on screen and in review instead of rendering as empty text).
    fn raw<'a>(&'a self, key: &'a str) -> &'a str {
        self.strings
            .get(key)
            .or_else(|| self.fallback.get(key))
            .map_or(key, String::as_str)
    }

    /// Text for `key` with `{name}` placeholders filled from `args`. A plural message picks
    /// its form from the `count` argument: this language's form, else its `other`, then the
    /// same in English. Without a whole-number `count` it uses `other`.
    #[must_use]
    pub fn t(&self, key: Key, args: &[(&str, String)]) -> String {
        self.lookup(key.0, args, None)
    }

    /// Like [`Catalog::t`], recording a key found nowhere into `missing`.
    #[must_use]
    pub fn t_reporting(&self, key: Key, args: &[(&str, String)], missing: &MissingKeys) -> String {
        self.lookup(key.0, args, Some(missing))
    }

    /// The template bridge: templates name keys as runtime strings. Only the theme runtime
    /// (`crates/render`) may call this; `scripts/check` fails on any other caller, so Rust
    /// code keeps going through [`Key`].
    #[must_use]
    pub fn t_template(&self, key: &str, args: &[(&str, String)], missing: &MissingKeys) -> String {
        self.lookup(key, args, Some(missing))
    }

    fn lookup(&self, key: &str, args: &[(&str, String)], missing: Option<&MissingKeys>) -> String {
        let count = args
            .iter()
            .find(|(k, _)| *k == "count")
            .and_then(|(_, v)| v.parse::<u64>().ok());
        let plural_text = Self::plural_form(&self.strings, &self.lang, key, count)
            .or_else(|| Self::plural_form(&self.fallback, "en", key, count));
        let text = plural_text.unwrap_or_else(|| {
            let text = self.raw(key);
            if let Some(m) = missing
                && std::ptr::eq(text, key)
            {
                m.record(&self.lang, key);
            }
            text
        });
        fill(text, args)
    }

    fn plural_form<'a>(
        map: &'a HashMap<String, String>,
        lang: &str,
        key: &str,
        n: Option<u64>,
    ) -> Option<&'a str> {
        // Only a table with `other` is a plural message (see `parse`).
        let other = map.get(&format!("{key}.other"))?;
        let form = n.map_or("other", |n| plural(lang, n).name());
        Some(map.get(&format!("{key}.{form}")).unwrap_or(other).as_str())
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
    use super::{Catalog, MissingKeys, fill, parse};
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
        assert_eq!(c.raw("post.read_more"), "\u{110}\u{1ecd}c ti\u{1ebf}p");
        assert_eq!(c.raw("post.share"), "Share");
        assert_eq!(c.raw("post.missing"), "post.missing");
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
        // Category-named keys without `other` are a plain section, not a plural message.
        let rank = parse(
            "[rank]
one = \"First\"
two = \"Second\"
",
        )
        .unwrap();
        let c = Catalog::new(rank, HashMap::new());
        assert_eq!(c.t(key!("rank.one"), &[]), "First");
        assert_eq!(c.t(key!("rank"), &[("count", "1".into())]), "rank");
        let m = MissingKeys::default();
        assert_eq!(c.t_template("rank.two", &[], &m), "Second");
        assert!(m.take().is_empty());
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
            ru.t(key!("c.posts"), &n(1))
                .ends_with("\u{437}\u{430}\u{43f}\u{438}\u{441}\u{44c}")
        );
        assert!(
            ru.t(key!("c.posts"), &n(3))
                .ends_with("\u{437}\u{430}\u{43f}\u{438}\u{441}\u{438}")
        );
        assert!(
            ru.t(key!("c.posts"), &n(11))
                .ends_with("\u{437}\u{430}\u{43f}\u{438}\u{441}\u{435}\u{439}")
        );
        let vi =
            Catalog::new(map(&[("c.posts.other", "{count} b\u{e0}i")]), en.clone()).with_lang("vi");
        assert_eq!(vi.t(key!("c.posts"), &n(1)), "1 b\u{e0}i");
        // A language without the message uses English forms.
        let fr = Catalog::new(HashMap::new(), en).with_lang("fr");
        assert_eq!(fr.t(key!("c.posts"), &n(1)), "1 post");
        assert_eq!(fr.t(key!("c.posts"), &n(2)), "2 posts");
        // A count that is not a whole number uses `other`.
        assert_eq!(
            fr.t(key!("c.posts"), &[("count", "1.5".into())]),
            "1.5 posts"
        );
        assert_eq!(fr.t(key!("c.posts"), &[("count", "-1".into())]), "-1 posts");
        // No count: a plural message still shows its `other` form.
        assert_eq!(fr.t(key!("c.posts"), &[]), "{count} posts");
    }

    #[test]
    fn missing_keys_are_reported_once_per_key() {
        let c = Catalog::new(map(&[("a", "A")]), map(&[("b", "B")])).with_lang("vi");
        let missing = MissingKeys::default();
        assert_eq!(c.t_reporting(key!("a"), &[], &missing), "A");
        assert_eq!(c.t_reporting(key!("b"), &[], &missing), "B");
        assert_eq!(c.t_reporting(key!("nope"), &[], &missing), "nope");
        assert_eq!(c.t_reporting(key!("nope"), &[], &missing), "nope");
        assert_eq!(missing.take(), [("vi".to_owned(), "nope".to_owned())]);
        assert!(missing.take().is_empty());
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
