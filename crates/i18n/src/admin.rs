//! The admin client's UI text, built into its bundle rather than the Worker
//! (docs/ARCHITECTURE.md, Admin and editor): one ES module per language, written by
//! `scripts/admin-build` through `examples/admin_locales.rs`.

use crate::parse;
use plumblog_model::Result;
use std::collections::{BTreeMap, HashMap};
use std::fmt::Write;

/// Locale sections the admin client uses: its own text and the error codes the API answers
/// with.
pub const ADMIN_SECTIONS: &[&str] = &["admin.", "error."];

/// One language's admin text with English behind it, as a module:
/// `export default {"messages":{...},"fallback":{...}};`. `fallback` holds the English keys
/// the language lacks; a plural message the language has in any form takes no English forms.
///
/// # Errors
///
/// A locale file that does not parse.
pub fn admin_module(own_src: &str, en_src: &str, is_en: bool) -> Result<String> {
    let (own, fallback) = admin_text(own_src, en_src, is_en)?;
    let map = |m: &BTreeMap<String, String>| {
        let items: Vec<String> = m
            .iter()
            .map(|(k, v)| format!("{}:{}", json_string(k), json_string(v)))
            .collect();
        format!("{{{}}}", items.join(","))
    };
    Ok(format!(
        "export default {{\"messages\":{},\"fallback\":{}}};\n",
        map(&own),
        map(&fallback)
    ))
}

/// The messages and English fallback [`admin_module`] writes.
///
/// # Errors
///
/// A locale file that does not parse.
pub fn admin_text(
    own_src: &str,
    en_src: &str,
    is_en: bool,
) -> Result<(BTreeMap<String, String>, BTreeMap<String, String>)> {
    let own = sections(parse(own_src)?);
    let fallback = if is_en {
        BTreeMap::new()
    } else {
        sections(parse(en_src)?)
            .into_iter()
            .filter(|(k, _)| !has_base(&own, k))
            .collect()
    };
    Ok((own, fallback))
}

fn sections(all: HashMap<String, String>) -> BTreeMap<String, String> {
    all.into_iter()
        .filter(|(k, _)| ADMIN_SECTIONS.iter().any(|s| k.starts_with(s)))
        .collect()
}

/// Whether `own` has `key`, counting a plural message in any of its forms (`a.n.one` vs
/// `a.n.other`), so a language with fewer forms does not get English forms mixed in.
fn has_base(own: &BTreeMap<String, String>, key: &str) -> bool {
    if own.contains_key(key) {
        return true;
    }
    match key.rsplit_once('.') {
        Some((base, form)) if is_plural_form(form) => own.keys().any(|k| {
            k.strip_prefix(base)
                .and_then(|r| r.strip_prefix('.'))
                .is_some_and(is_plural_form)
        }),
        _ => false,
    }
}

fn is_plural_form(s: &str) -> bool {
    matches!(s, "zero" | "one" | "two" | "few" | "many" | "other")
}

/// A JSON string literal, also safe as a JS string (line and paragraph separators escaped).
#[must_use]
pub fn json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            // Line and paragraph separators end a JS line in older engines.
            '\u{2028}' => out.push_str("\\u2028"),
            '\u{2029}' => out.push_str("\\u2029"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::{admin_module, admin_text, json_string};

    const EN: &str = "[site]\nname = \"S\"\n[admin]\ntitle = \"T\"\n[admin.nav]\nposts = \"Posts\"\npages = \"Pages\"\n[admin.ed]\nwords = { one = \"{count} word\", other = \"{count} words\" }\n[error]\nnot_found = \"Gone\"\n";
    const VI: &str = "[admin.nav]\nposts = \"B\u{e0}i\"\n[admin.ed]\nwords = { other = \"{count} ch\u{1eef}\" }\n";

    #[test]
    fn a_language_gets_english_only_for_what_it_lacks() {
        let (own, fb) = admin_text(VI, EN, false).unwrap();
        assert_eq!(
            own.keys().collect::<Vec<_>>(),
            ["admin.ed.words.other", "admin.nav.posts"]
        );
        // `admin.ed.words` exists in Vietnamese (one form): no English `one` mixed in. Other
        // sections (`site`) are not the admin's.
        assert_eq!(
            fb.keys().collect::<Vec<_>>(),
            ["admin.nav.pages", "admin.title", "error.not_found"]
        );
        let (_, none) = admin_text(EN, EN, true).unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn the_module_is_one_default_export() {
        let m = admin_module(VI, EN, false).unwrap();
        assert!(m.starts_with("export default {\"messages\":{"), "{m}");
        assert!(m.contains("\"admin.nav.pages\":\"Pages\""), "{m}");
        assert!(m.ends_with("};\n"));
        assert!(admin_module("[x", EN, false).is_err());
    }

    #[test]
    fn json_strings_escape_controls_and_separators() {
        assert_eq!(
            json_string("a\"b\\c\n\u{1}\u{2028}"),
            "\"a\\\"b\\\\c\\n\\u0001\\u2028\""
        );
    }
}
