//! Repository-wide locale check, run by `scripts/check`:
//! - every key a theme template or Rust code uses exists in that layer's `en.toml`;
//! - every language file has exactly the messages of `en.toml`;
//! - every plural message has exactly the forms its language needs.
//!
//! Layers: `locales/` serves core and admin code; each `themes/<name>/locales/` serves that
//! theme's templates, and the default theme (`minimal`) also serves `crates/render`.
//! Dynamic keys (`t("comments.notice_" ~ kind)`) only need one key with that prefix; keys
//! built with `format!` in Rust cannot be seen and are not checked.

use plumblog_i18n::{categories, parse};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

const DEFAULT_THEME: &str = "minimal";

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn files(dir: &Path, ext: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(files(&p, ext));
        } else if p.extension().is_some_and(|x| x == ext) {
            out.push(p);
        }
    }
    out.sort();
    out
}

/// A language file as messages: key -> plural forms (empty for plain text).
type Messages = BTreeMap<String, BTreeSet<String>>;

fn messages(flat: &BTreeMap<String, String>) -> Messages {
    let mut out = Messages::new();
    for key in flat.keys() {
        let plural = key.rsplit_once('.').filter(|(base, form)| {
            plumblog_i18n::Plural::from_name(form).is_some()
                && flat.contains_key(&format!("{base}.other"))
        });
        match plural {
            Some((base, form)) => {
                out.entry(base.to_owned())
                    .or_default()
                    .insert(form.to_owned());
            }
            None => {
                out.entry(key.clone()).or_default();
            }
        }
    }
    out
}

/// Loads one layer; returns lang -> messages, recording parse problems.
fn layer(dir: &Path, problems: &mut Vec<String>) -> BTreeMap<String, Messages> {
    let mut langs = BTreeMap::new();
    for path in files(dir, "toml") {
        let lang = path.file_stem().unwrap().to_string_lossy().into_owned();
        let src = fs::read_to_string(&path).unwrap_or_default();
        match parse(&src) {
            Ok(flat) => {
                langs.insert(lang, messages(&flat.into_iter().collect()));
            }
            Err(e) => problems.push(format!("{}: {}", rel(&path), e.detail)),
        }
    }
    if !langs.contains_key("en") {
        problems.push(format!("{}: en.toml is missing", rel(dir)));
    }
    langs
}

fn rel(p: &Path) -> String {
    let root = root().canonicalize().unwrap();
    let p = p.canonicalize().unwrap_or_else(|_| p.to_owned());
    p.strip_prefix(&root)
        .map_or_else(|_| p.display().to_string(), |r| r.display().to_string())
}

fn compare_languages(dir: &Path, langs: &BTreeMap<String, Messages>, problems: &mut Vec<String>) {
    let Some(en) = langs.get("en") else { return };
    for (lang, msgs) in langs {
        let file = format!("{}/{lang}.toml", rel(dir));
        for key in en.keys().filter(|k| !msgs.contains_key(*k)) {
            problems.push(format!("{file}: missing `{key}` (present in en.toml)"));
        }
        for key in msgs.keys().filter(|k| !en.contains_key(*k)) {
            problems.push(format!("{file}: `{key}` is not in en.toml"));
        }
        let want: BTreeSet<String> = categories(lang)
            .iter()
            .map(|c| c.name().to_owned())
            .collect();
        for (key, forms) in msgs {
            let en_plural = en.get(key).is_some_and(|f| !f.is_empty());
            if forms.is_empty() && en_plural {
                problems.push(format!(
                    "{file}: `{key}` must be a plural message like in en.toml"
                ));
            } else if !forms.is_empty() && forms != &want {
                problems.push(format!(
                    "{file}: `{key}` has forms {forms:?}, {lang} needs {want:?}"
                ));
            }
        }
    }
}

/// One `t(...)` call in a template.
#[derive(Debug, PartialEq, Eq)]
enum Use {
    /// `t("a.b")`.
    Literal(String),
    /// `t("a.b_" ~ x)` or `t(x)`: the possible keys come from an `{# i18n: ... #}`
    /// declaration on the same or the previous line; `None` when there is none.
    Dynamic {
        prefix: String,
        declared: Option<Vec<String>>,
    },
}

/// `{# i18n: k1 k2 #}` (or `{#- ... #}`) on a line.
fn declaration(line: &str) -> Option<Vec<String>> {
    let start = line.find("{#")?;
    let body = &line[start + 2..];
    let body = &body[..body.find("#}")?];
    let body = body.trim_start_matches('-').trim();
    let keys = body.strip_prefix("i18n:")?;
    Some(keys.split_whitespace().map(str::to_owned).collect())
}

/// Every `t(...)` call: quotes `"` or `'`, spaces after `(` allowed. Calls such as
/// `slot("x")` are not `t` calls.
fn template_uses(src: &str) -> Vec<(Use, usize)> {
    let lines: Vec<&str> = src.lines().collect();
    let mut out = Vec::new();
    for (n, line) in lines.iter().enumerate() {
        let bytes = line.as_bytes();
        for (i, _) in line.match_indices("t(") {
            if i > 0
                && (bytes[i - 1].is_ascii_alphanumeric() || matches!(bytes[i - 1], b'_' | b'.'))
            {
                continue;
            }
            let rest = line[i + 2..].trim_start();
            let literal = rest
                .chars()
                .next()
                .filter(|q| *q == '"' || *q == '\'')
                .and_then(|q| {
                    let body = &rest[1..];
                    body.find(q)
                        .map(|end| (body[..end].to_owned(), body[end + 1..].trim_start()))
                });
            let used = match literal {
                Some((key, after)) if !after.starts_with('~') => Use::Literal(key),
                other => Use::Dynamic {
                    prefix: other.map(|(k, _)| k).unwrap_or_default(),
                    declared: declaration(line)
                        .or_else(|| n.checked_sub(1).and_then(|p| declaration(lines[p]))),
                },
            };
            out.push((used, n + 1));
        }
    }
    out
}

/// Keys declared in a crate's `keys.rs` (`Key("...")`).
fn declared_rust_keys(src: &str) -> Vec<String> {
    src.match_indices("Key(\"")
        .filter_map(|(i, _)| {
            let body = &src[i + 5..];
            body.find('"').map(|end| body[..end].to_owned())
        })
        .collect()
}

/// Ways Rust code could name a key outside `keys.rs`: constructing `Key`, or passing a
/// string literal to a lookup function.
fn rust_bypasses(src: &str) -> Vec<String> {
    // Comments may mention calls; drop them, keep line breaks for line numbers.
    let code: String = src
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    let line_of = |i: usize| code[..i].matches('\n').count() + 1;
    let mut out = Vec::new();
    for (i, _) in code.match_indices("Key(\"") {
        out.push(format!("line {}: `Key(` outside keys.rs", line_of(i)));
    }
    // The key is the first argument of `.t(` / `.t_reporting(`, the second of `.text(`.
    for (call, arg) in [(".t(", 0), (".t_reporting(", 0), (".text(", 1)] {
        for (i, _) in code.match_indices(call) {
            let key = nth_arg(&code[i + call.len()..], arg);
            if key.starts_with('"') || key.contains("format!") {
                out.push(format!(
                    "line {}: key `{key}` passed to `{call}` is not a Key constant",
                    line_of(i)
                ));
            }
        }
    }
    out
}

/// The `n`th top-level argument of a call whose `(` was just consumed, trimmed.
fn nth_arg(args: &str, n: usize) -> &str {
    let (mut depth, mut start, mut index, mut quoted) = (0i32, 0, 0, false);
    for (i, c) in args.char_indices() {
        match c {
            '"' => quoted = !quoted,
            _ if quoted => {}
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' if depth == 0 => {
                return if index == n {
                    args[start..i].trim()
                } else {
                    ""
                };
            }
            ')' | ']' | '}' => depth -= 1,
            ',' if depth == 0 => {
                if index == n {
                    return args[start..i].trim();
                }
                index += 1;
                start = i + 1;
            }
            _ => {}
        }
    }
    ""
}

fn check_template(
    uses: &[(Use, usize)],
    at: &str,
    en: Option<&Messages>,
    en_file: &str,
    problems: &mut Vec<String>,
) {
    let Some(en) = en else { return };
    for (used, line) in uses {
        let at = format!("{at}:{line}");
        match used {
            Use::Literal(key) => {
                if !en.contains_key(key) {
                    problems.push(format!("{at}: uses `{key}`, missing from {en_file}"));
                }
            }
            Use::Dynamic { prefix, declared: None } => problems.push(format!(
                "{at}: dynamic t(\"{prefix}\" ~ ...) needs `{{# i18n: <every key> #}}` on the line above"
            )),
            Use::Dynamic { prefix, declared: Some(keys) } => {
                for key in keys {
                    if !key.starts_with(prefix.as_str()) {
                        problems.push(format!("{at}: declared `{key}` does not start with `{prefix}`"));
                    } else if !en.contains_key(key) {
                        problems.push(format!("{at}: declared `{key}`, missing from {en_file}"));
                    }
                }
                if !prefix.is_empty() {
                    for key in en.keys().filter(|k| k.starts_with(prefix.as_str())) {
                        if !keys.contains(key) {
                            problems.push(format!(
                                "{at}: {en_file} has `{key}`, not in the i18n declaration"
                            ));
                        }
                    }
                }
            }
        }
    }
}

fn check_rust_keys(
    keys: &[String],
    at: &str,
    en: Option<&Messages>,
    en_file: &str,
    problems: &mut Vec<String>,
) {
    let Some(en) = en else { return };
    for key in keys.iter().filter(|k| !en.contains_key(*k)) {
        problems.push(format!("{at}: declares `{key}`, missing from {en_file}"));
    }
}

#[test]
fn locale_keys_are_complete_and_consistent() {
    let root = root();
    let mut problems = Vec::new();

    let core_dir = root.join("locales");
    let core = layer(&core_dir, &mut problems);
    compare_languages(&core_dir, &core, &mut problems);

    let mut default_theme_en = None;
    for theme in fs::read_dir(root.join("themes"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let dir = theme.path();
        if !dir.is_dir() {
            continue;
        }
        let loc_dir = dir.join("locales");
        let langs = layer(&loc_dir, &mut problems);
        compare_languages(&loc_dir, &langs, &mut problems);
        let en_file = format!("{}/en.toml", rel(&loc_dir));
        for tpl in files(&dir.join("templates"), "html") {
            let src = fs::read_to_string(&tpl).unwrap_or_default();
            check_template(
                &template_uses(&src),
                &rel(&tpl),
                langs.get("en"),
                &en_file,
                &mut problems,
            );
        }
        if dir.file_name().is_some_and(|n| n == DEFAULT_THEME) {
            default_theme_en = langs.get("en").cloned().map(|m| (m, en_file));
        }
    }

    for crate_dir in fs::read_dir(root.join("crates"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = crate_dir.file_name().to_string_lossy().into_owned();
        // This crate's own tests use sample keys on purpose.
        if name == "i18n" {
            continue;
        }
        for file in files(&crate_dir.path().join("src"), "rs") {
            let src = fs::read_to_string(&file).unwrap_or_default();
            let at = rel(&file);
            if file.file_name().is_some_and(|n| n == "keys.rs") {
                let keys = declared_rust_keys(&src);
                if name == "render" {
                    let (en, en_file) = default_theme_en
                        .as_ref()
                        .map_or((None, ""), |(m, f)| (Some(m), f.as_str()));
                    check_rust_keys(&keys, &at, en, en_file, &mut problems);
                } else {
                    check_rust_keys(&keys, &at, core.get("en"), "locales/en.toml", &mut problems);
                }
            } else {
                for p in rust_bypasses(&src) {
                    problems.push(format!("{at}: {p}; name keys in keys.rs"));
                }
            }
        }
    }

    assert!(
        problems.is_empty(),
        "locale problems:\n  {}",
        problems.join("\n  ")
    );
}

#[test]
fn key_scanners_find_what_templates_and_code_use() {
    let tpl = "{{ t(\"post.next\") }} {{ slot(\"after_post\") }} {{ t( 'a.b' ) }} {{ x.t(\"no\") }}\n{#- i18n: c.notice_ok c.notice_error #}\n{{ t(\"c.notice_\" ~ kind) }} {{ t(name) }}";
    let uses: Vec<Use> = template_uses(tpl).into_iter().map(|(u, _)| u).collect();
    assert_eq!(
        uses,
        [
            Use::Literal("post.next".into()),
            Use::Literal("a.b".into()),
            Use::Dynamic {
                prefix: "c.notice_".into(),
                declared: Some(vec!["c.notice_ok".into(), "c.notice_error".into()]),
            },
            Use::Dynamic {
                prefix: String::new(),
                declared: Some(vec!["c.notice_ok".into(), "c.notice_error".into()]),
            },
        ]
    );
    assert_eq!(
        declared_rust_keys("pub const A: Key = Key(\"a.b\");"),
        ["a.b"]
    );
    let bad = "let k = Key(\"x.y\");\nlet s = cat.t(\"site.title\", &[]);\nlet d = self.text(\n&lang, keys::A, &[(\"day\", x)]);\nlet f = self.text(&lang, &format!(\"date.month_{m}\"), &[]);\nlet e = self.text(\n    &lang,\n    \"date.short\",\n);";
    assert_eq!(rust_bypasses(bad).len(), 4, "{:?}", rust_bypasses(bad));
}
