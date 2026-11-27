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

/// Keys passed as the first literal argument of `t(...)`; the bool marks a dynamic prefix
/// (`t("a.b_" ~ x)`).
fn template_keys(src: &str) -> Vec<(String, bool)> {
    let mut out = Vec::new();
    let bytes = src.as_bytes();
    for (i, _) in src.match_indices("t(\"") {
        if i > 0 && (bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_') {
            continue; // slot(", list(" ...
        }
        let rest = &src[i + 3..];
        let Some(end) = rest.find('"') else { continue };
        let dynamic = rest[end + 1..].trim_start().starts_with('~');
        out.push((rest[..end].to_owned(), dynamic));
    }
    out
}

/// Literal keys in Rust calls `t("key"` or `.text(<lang>, "key"`.
fn rust_keys(src: &str) -> Vec<String> {
    let mut out: Vec<String> = template_keys(src)
        .into_iter()
        .filter(|(_, dynamic)| !dynamic)
        .map(|(k, _)| k)
        .collect();
    for (i, _) in src.match_indices(".text(") {
        let args = &src[i + 6..];
        let call = &args[..args.find(')').unwrap_or(args.len())];
        if let Some((_, after)) = call.split_once(", \"")
            && let Some(end) = after.find('"')
        {
            out.push(after[..end].to_owned());
        }
    }
    out.retain(|k| k.contains('.'));
    out
}

fn check_used(
    used: &[(String, bool, String)],
    en: Option<&Messages>,
    en_file: &str,
    problems: &mut Vec<String>,
) {
    let Some(en) = en else { return };
    for (key, dynamic, at) in used {
        let found = if *dynamic {
            en.keys().any(|k| k.starts_with(key.as_str()))
        } else {
            en.contains_key(key)
        };
        if !found {
            problems.push(format!("{at}: uses `{key}`, missing from {en_file}"));
        }
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
        let mut used = Vec::new();
        for tpl in files(&dir.join("templates"), "html") {
            let src = fs::read_to_string(&tpl).unwrap_or_default();
            for (key, dynamic) in template_keys(&src) {
                used.push((key, dynamic, rel(&tpl)));
            }
        }
        let en_file = format!("{}/en.toml", rel(&loc_dir));
        check_used(&used, langs.get("en"), &en_file, &mut problems);
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
        let mut used = Vec::new();
        for file in files(&crate_dir.path().join("src"), "rs") {
            let src = fs::read_to_string(&file).unwrap_or_default();
            for key in rust_keys(&src) {
                used.push((key, false, rel(&file)));
            }
        }
        if name == "render" {
            if let Some((en, file)) = &default_theme_en {
                check_used(&used, Some(en), file, &mut problems);
            }
        } else {
            check_used(&used, core.get("en"), "locales/en.toml", &mut problems);
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
    let tpl = r#"{{ t("post.next") }} {{ slot("after_post") }} {{ t("c.notice_" ~ kind) }}"#;
    assert_eq!(
        template_keys(tpl),
        [
            ("post.next".to_owned(), false),
            ("c.notice_".to_owned(), true)
        ]
    );
    let rs = r#"let a = cat.t("site.title", &[]); let b = self.text(&lang, "date.today", &[]); let c = list("x.y");"#;
    assert_eq!(rust_keys(rs), ["site.title", "date.today"]);
}
