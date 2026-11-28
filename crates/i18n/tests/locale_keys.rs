//! Repository-wide locale check, run by `scripts/check`:
//! - every key a theme template or Rust code uses exists in that layer's `en.toml`;
//! - every language file has exactly the messages of `en.toml`;
//! - every plural message has exactly the forms its language needs.
//!
//! Layers: `locales/` serves core and admin code; each `themes/<name>/locales/` serves that
//! theme's templates, and the default theme (`minimal`) also serves `crates/render`.
//!
//! Rust: the compiler makes every key a `key!("literal")` (`Key` has a private field and
//! the lookup functions take `Key`), so this check reads every `key!(` in every crate. It
//! also fails on the hidden constructor or the `&str` template bridge used anywhere else.
//! Templates: `t` may only appear as a direct call `t(...)`; a key built at runtime
//! (`t("a_" ~ x)`, `t(var)`) needs `{# i18n: <every key> #}` on the same or previous line,
//! checked both ways against `en.toml`.
//! Admin TypeScript (`web/admin/src`): the same rules for `t("key")`; a key built at runtime is
//! a template string (`` t(`a.${x}`) ``) with `// i18n: <every key>` on the line above. Text
//! must not bypass `t`: no string with letters as an `h()` child or as an `aria-label`,
//! `title`, `placeholder` or `alt` value, and `t` may not be imported under another name.
//! The `blog` CLI (`crates/cli/src`): no letters in a `print!`/`eprint!` format string, a
//! clap help setter or attribute, and no doc comment on a clap derive (clap prints those as
//! help). Its text comes from `key!` like the rest; test modules are not user text.
//! The admin's per-language text modules (`plumblog_i18n::admin`, built by admin-build) are
//! checked too: every English admin key exactly once, plurals in the language's own forms.

use plumblog_i18n::admin::admin_text;
use plumblog_i18n::{categories, parse_toml as parse};
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
    /// `t("a.b_" ~ x)` or `t(x)`: the keys come from an `{# i18n: ... #}` declaration on the
    /// same or the previous line; `None` when there is none.
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
    let keys = body.trim_start_matches('-').trim().strip_prefix("i18n:")?;
    Some(keys.split_whitespace().map(str::to_owned).collect())
}

fn is_ident(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// Byte ranges of `{{ ... }}` and `{% ... %}` (where template code lives), with the
/// delimiters' inner text only.
fn code_regions(src: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(i) = src[pos..].find('{').map(|i| pos + i) {
        let close = match src[i + 1..].chars().next() {
            Some('{') => "}}",
            Some('%') => "%}",
            _ => {
                pos = i + 1;
                continue;
            }
        };
        let Some(end) = src[i + 2..].find(close).map(|e| i + 2 + e) else {
            break;
        };
        out.push((i + 2, end));
        pos = end + 2;
    }
    out
}

/// Calls found, and misuses as (message, line).
type Scan = (Vec<(Use, usize)>, Vec<(String, usize)>);

/// Every use of `t` in template code: direct calls become [`Use`]s; any other use of `t` as
/// a value (`{% set f = t %}`, `[t][0](...)`) is a problem, because the check could not see
/// which keys it is called with. Quotes may be `"` or `'`; whitespace and newlines are
/// allowed before `(` and around the key.
fn template_uses(src: &str) -> Scan {
    let lines: Vec<&str> = src.lines().collect();
    let line_of = |i: usize| src[..i].matches('\n').count() + 1;
    let declared_near = |line: usize| {
        declaration(lines.get(line - 1).unwrap_or(&""))
            .or_else(|| line.checked_sub(2).and_then(|p| declaration(lines[p])))
    };
    let mut uses = Vec::new();
    let mut misuse = Vec::new();
    for (start, end) in code_regions(src) {
        let code = &src[start..end];
        let mut chars = code.char_indices().peekable();
        let mut prev_sig: Option<char> = None;
        while let Some((i, c)) = chars.next() {
            if c == '"' || c == '\'' {
                // Skip string literals, so `"t"` in text is not a use.
                for (_, n) in chars.by_ref() {
                    if n == c {
                        break;
                    }
                }
                prev_sig = Some(c);
                continue;
            }
            if !is_ident(c) {
                if !c.is_whitespace() {
                    prev_sig = Some(c);
                }
                continue;
            }
            let mut word_end = i + c.len_utf8();
            while let Some(&(j, n)) = chars.peek() {
                if !is_ident(n) {
                    break;
                }
                word_end = j + n.len_utf8();
                chars.next();
            }
            let word = &code[i..word_end];
            let after_dot = prev_sig == Some('.');
            prev_sig = Some('a');
            if word != "t" || after_dot {
                continue;
            }
            let line = line_of(start + i);
            let rest = code[word_end..].trim_start();
            let Some(args) = rest.strip_prefix('(') else {
                misuse.push((
                    "`t` used other than as a direct call `t(...)`".to_owned(),
                    line,
                ));
                continue;
            };
            let args = args.trim_start();
            let literal = args
                .chars()
                .next()
                .filter(|q| *q == '"' || *q == '\'')
                .and_then(|q| {
                    let body = &args[1..];
                    body.find(q)
                        .map(|e| (body[..e].to_owned(), body[e + 1..].trim_start()))
                });
            let call = match literal {
                Some((key, after)) if after.starts_with(',') || after.starts_with(')') => {
                    Use::Literal(key)
                }
                // `"a_" ~ x` has a known prefix; anything else (`x`, `"a" if c else "b"`) none.
                other => Use::Dynamic {
                    prefix: other
                        .filter(|(_, after)| after.starts_with('~'))
                        .map(|(k, _)| k)
                        .unwrap_or_default(),
                    declared: declared_near(line),
                },
            };
            uses.push((call, line));
        }
    }
    (uses, misuse)
}

/// Problems in Rust source: `key!` without a plain string literal, the hidden `Key`
/// constructor, or the template bridge outside the theme runtime. Returns the keys found.
fn rust_keys(src: &str, bridge_allowed: bool, problems: &mut Vec<String>, at: &str) -> Vec<String> {
    let code: String = src
        .lines()
        .map(|l| l.split("//").next().unwrap_or(""))
        .collect::<Vec<_>>()
        .join("\n");
    let line_of = |i: usize| code[..i].matches('\n').count() + 1;
    let mut keys = Vec::new();
    for (i, _) in code.match_indices("key!") {
        let args = code[i + 4..].trim_start();
        let Some(args) = args.strip_prefix('(') else {
            continue;
        };
        let args = args.trim_start();
        let key = args.strip_prefix('"').and_then(|b| {
            b.find(['"', '\\'])
                .filter(|e| b[*e..].starts_with('"'))
                .map(|e| &b[..e])
        });
        match key {
            Some(k) => keys.push(k.to_owned()),
            None => problems.push(format!(
                "{at}:{}: key! takes a plain string literal (no raw strings or escapes)",
                line_of(i)
            )),
        }
    }
    for (i, _) in code.match_indices("__from_literal") {
        problems.push(format!(
            "{at}:{}: use key!(\"...\"), not Key::__from_literal",
            line_of(i)
        ));
    }
    if !bridge_allowed {
        for (i, _) in code.match_indices("t_template") {
            problems.push(format!(
                "{at}:{}: t_template is only for the theme runtime; Rust code uses key!",
                line_of(i)
            ));
        }
    }
    keys
}

/// Hard-coded user text in CLI source: see the module comment. Returns `(problem, line)`.
fn cli_hardcoded(src: &str) -> Vec<(String, usize)> {
    // Test modules sit at the end of a file and are not user text.
    let src = src.split("#[cfg(test)]").next().unwrap_or("");
    let code: String = src
        .lines()
        .map(|l| {
            if l.trim_start().starts_with("//") && !l.trim_start().starts_with("///") {
                ""
            } else {
                l
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let line_of = |i: usize| code[..i].matches('\n').count() + 1;
    let mut out = Vec::new();
    let sinks = [
        "print!(",
        "println!(",
        "eprint!(",
        "eprintln!(",
        ".about(",
        ".long_about(",
        ".help(",
        ".long_help(",
        ".before_help(",
        ".after_help(",
        "about =",
        "help =",
    ];
    for sink in sinks {
        for (i, _) in code.match_indices(sink) {
            let ident_before = code[..i]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_');
            if !sink.starts_with('.') && ident_before {
                continue; // `eprint!(` also contains `print!(`.
            }
            let rest = code[i + sink.len()..].trim_start();
            let Some(lit) = rest.strip_prefix('"') else {
                continue;
            };
            let mut text = String::new();
            let mut chars = lit.chars();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => {
                        chars.next();
                    }
                    '"' => break,
                    c => text.push(c),
                }
            }
            // Placeholders (`{name}`, `{}`) are not text.
            let mut depth = 0;
            let letters = text.chars().any(|c| {
                match c {
                    '{' => depth += 1,
                    '}' => depth -= 1,
                    _ => {}
                }
                depth == 0 && c.is_ascii_alphabetic()
            });
            if letters {
                out.push((
                    format!(
                        "hard-coded text in `{}`: use a locale key",
                        sink.trim_end_matches('(')
                    ),
                    line_of(i),
                ));
            }
        }
    }
    let mut in_clap = false;
    for (n, line) in code.lines().enumerate() {
        if line.contains("derive(")
            && ["Parser", "Subcommand", "Args"]
                .iter()
                .any(|d| line.contains(d))
        {
            in_clap = true;
        } else if line == "}" {
            in_clap = false;
        } else if in_clap && line.trim_start().starts_with("///") {
            out.push((
                "doc comment on a clap item becomes English help: use a locale key".to_owned(),
                n + 1,
            ));
        }
    }
    out
}

/// Every `t(...)` call in admin TypeScript. Comment lines and imports are skipped; any other
/// `t` not called directly is a problem, like in templates.
fn ts_uses(src: &str) -> Scan {
    let lines: Vec<&str> = src.lines().collect();
    let mut uses = Vec::new();
    let mut misuse = Vec::new();
    for (n, line) in lines.iter().enumerate() {
        let lead = line.trim_start();
        if lead.starts_with("//")
            || lead.starts_with('*')
            || lead.starts_with("/*")
            || lead.starts_with("import ")
        {
            continue;
        }
        let declared = n
            .checked_sub(1)
            .and_then(|p| lines.get(p))
            .and_then(|l| l.trim_start().strip_prefix("// i18n:"))
            .map(|k| k.split_whitespace().map(str::to_owned).collect::<Vec<_>>());
        let bytes = line.as_bytes();
        for (i, _) in line.match_indices('t') {
            let before = line[..i].chars().next_back();
            let after = line[i + 1..].chars().next();
            if before.is_some_and(|c| is_ident(c) || c == '.' || c == '$')
                || after.is_some_and(|c| is_ident(c) || c == '$')
            {
                continue;
            }
            let rest = line[i + 1..].trim_start();
            let Some(args) = rest.strip_prefix('(') else {
                // `t` inside a string or a type is not a use; a bare `t` value is.
                if !in_string(bytes, i) {
                    misuse.push((
                        "t used as a value; call it directly: t(\"key\")".to_owned(),
                        n + 1,
                    ));
                }
                continue;
            };
            let args = args.trim_start();
            let quote = args.chars().next();
            match quote {
                Some(q @ ('"' | '\'')) => {
                    let body = &args[1..];
                    let end = body.find(q).unwrap_or(body.len());
                    uses.push((Use::Literal(body[..end].to_owned()), n + 1));
                }
                Some('`') => {
                    let body = &args[1..];
                    let end = body.find('`').unwrap_or(body.len());
                    let tpl = &body[..end];
                    match tpl.find("${") {
                        Some(dollar) => uses.push((
                            Use::Dynamic {
                                prefix: tpl[..dollar].to_owned(),
                                declared: declared.clone(),
                            },
                            n + 1,
                        )),
                        None => uses.push((Use::Literal(tpl.to_owned()), n + 1)),
                    }
                }
                _ => misuse.push((
                    "t() takes a string literal, or a template string with `// i18n:` above"
                        .to_owned(),
                    n + 1,
                )),
            }
        }
    }
    (uses, misuse)
}

/// Admin source with comments blanked (newlines kept) and, per byte, whether it is inside a
/// string literal. Template literals end at the next backtick; `${...}` nesting is not needed
/// for the rules that use this.
fn ts_lex(src: &str) -> (Vec<u8>, Vec<bool>) {
    let b = src.as_bytes();
    let mut code = b.to_vec();
    let mut in_str = vec![false; b.len()];
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    code[i] = b' ';
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                while i < b.len() && !(b[i] == b'*' && b.get(i + 1) == Some(&b'/')) {
                    if b[i] != b'\n' {
                        code[i] = b' ';
                    }
                    i += 1;
                }
                for _ in 0..2 {
                    if i < b.len() {
                        code[i] = b' ';
                        i += 1;
                    }
                }
            }
            q @ (b'"' | b'\'' | b'`') => {
                i += 1;
                while i < b.len() && b[i] != q {
                    in_str[i] = true;
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    in_str.truncate(b.len());
    (code, in_str)
}

/// The string literal starting at `i` (a quote), if it holds a letter: user-facing text.
fn text_literal(code: &[u8], i: usize) -> Option<String> {
    let q = *code.get(i)?;
    if !matches!(q, b'"' | b'\'' | b'`') {
        return None;
    }
    let end = code[i + 1..].iter().position(|&c| c == q)? + i + 1;
    let s = String::from_utf8_lossy(&code[i + 1..end]).into_owned();
    // Escapes (`\u00b7`, `\n`) are not words: drop them before looking for letters.
    let mut plain = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            plain.push(c);
            continue;
        }
        let skip = match chars.next() {
            Some('u') => 4,
            Some('x') => 2,
            _ => 0,
        };
        for _ in 0..skip {
            chars.next();
        }
    }
    plain.chars().any(char::is_alphabetic).then_some(s)
}

/// Text in admin TypeScript that bypasses `t`: string literals with letters as `h()`
/// children or as accessible-text attribute values, and `t` imported under another name.
fn ts_hardcoded(src: &str) -> Vec<(String, usize)> {
    let (code, in_str) = ts_lex(src);
    let line_of = |i: usize| code[..i].split(|&c| c == b'\n').count();
    let ident =
        |c: Option<&u8>| c.is_some_and(|&c| c.is_ascii_alphanumeric() || c == b'_' || c == b'$');
    let skip_ws = |mut j: usize| {
        while code.get(j).is_some_and(u8::is_ascii_whitespace) {
            j += 1;
        }
        j
    };
    let mut out = Vec::new();
    for i in 0..code.len() {
        if in_str[i] || (i > 0 && ident(code.get(i - 1))) {
            continue;
        }
        // `import { t as tr }`: the renamed function would escape every rule above.
        if code[i..].starts_with(b"t as ") && !ident(code.get(i + 1).filter(|&&c| c != b' ')) {
            let line_start = code[..i]
                .iter()
                .rposition(|&c| c == b'\n')
                .map_or(0, |p| p + 1);
            if code[line_start..].starts_with(b"import") {
                out.push((
                    "t imported under another name; import it as t".to_owned(),
                    line_of(i),
                ));
            }
        }
        // `"aria-label": "Close"`, `title: "Save"` and the like.
        for attr in ["aria-label", "title", "placeholder", "alt"] {
            let quoted = code.get(i).is_some_and(|&c| c == b'"' || c == b'\'');
            let start = if quoted { i + 1 } else { i };
            if !code[start..].starts_with(attr.as_bytes())
                || ident(code.get(start + attr.len())) && !quoted
            {
                continue;
            }
            let mut j = start + attr.len();
            if quoted {
                if code.get(j) != code.get(i) {
                    continue;
                }
                j += 1;
            }
            let j = skip_ws(j);
            if code.get(j) != Some(&b':') {
                continue;
            }
            if let Some(s) = text_literal(&code, skip_ws(j + 1)) {
                out.push((
                    format!("{attr} is fixed text {s:?}; use t(\"key\")"),
                    line_of(i),
                ));
            }
        }
        // `h("button", {...}, "Save")`: every argument after the attributes.
        if code[i..].starts_with(b"h(") {
            let mut depth = 0usize;
            let mut arg = 0;
            let mut j = i + 2;
            let mut at_arg_start = true;
            while j < code.len() {
                if in_str[j] {
                    j += 1;
                    continue;
                }
                if at_arg_start && arg >= 2 && depth == 0 {
                    let k = skip_ws(j);
                    if let Some(s) = text_literal(&code, k) {
                        out.push((
                            format!("h() child is fixed text {s:?}; use t(\"key\")"),
                            line_of(k),
                        ));
                    }
                }
                at_arg_start = false;
                match code[j] {
                    b'(' | b'[' | b'{' => depth += 1,
                    b')' | b']' | b'}' if depth == 0 => break,
                    b')' | b']' | b'}' => depth -= 1,
                    b',' if depth == 0 => {
                        arg += 1;
                        at_arg_start = true;
                    }
                    _ => {}
                }
                j += 1;
            }
        }
    }
    out
}

/// Whether byte `i` sits inside a quoted string on its line.
fn in_string(line: &[u8], i: usize) -> bool {
    let mut quote = None;
    for &b in &line[..i] {
        match quote {
            None if matches!(b, b'"' | b'\'' | b'`') => quote = Some(b),
            Some(q) if b == q => quote = None,
            _ => {}
        }
    }
    quote.is_some()
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
            let (uses, misuse) = template_uses(&src);
            for (p, line) in misuse {
                problems.push(format!("{}:{line}: {p}", rel(&tpl)));
            }
            check_template(&uses, &rel(&tpl), langs.get("en"), &en_file, &mut problems);
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
        // This crate defines the API and tests it with sample keys.
        if name == "i18n" {
            continue;
        }
        let (en, en_file) = if name == "render" {
            default_theme_en
                .as_ref()
                .map_or((None, String::new()), |(m, f)| (Some(m), f.clone()))
        } else {
            (core.get("en"), "locales/en.toml".to_owned())
        };
        for file in files(&crate_dir.path(), "rs") {
            let at = rel(&file);
            let src = fs::read_to_string(&file).unwrap_or_default();
            let bridge = at == "crates/render/src/theme.rs";
            if at.starts_with("crates/cli/src/") {
                for (p, line) in cli_hardcoded(&src) {
                    problems.push(format!("{at}:{line}: {p}"));
                }
            }
            for key in rust_keys(&src, bridge, &mut problems, &at) {
                if en.is_some_and(|en| !en.contains_key(&key)) {
                    problems.push(format!("{at}: uses `{key}`, missing from {en_file}"));
                }
            }
        }
    }

    let admin = root.join("web/admin/src");
    for file in files(&admin, "ts") {
        let at = rel(&file);
        // The lookup itself, and code generated from Rust types.
        if at.ends_with("/i18n.ts") || at.ends_with(".gen.ts") {
            continue;
        }
        let src = fs::read_to_string(&file).unwrap_or_default();
        let (uses, misuse) = ts_uses(&src);
        for (p, line) in misuse.into_iter().chain(ts_hardcoded(&src)) {
            problems.push(format!("{at}:{line}: {p}"));
        }
        check_template(&uses, &at, core.get("en"), "locales/en.toml", &mut problems);
    }

    assert!(
        problems.is_empty(),
        "locale problems:\n  {}",
        problems.join("\n  ")
    );
}

/// Problems in the admin text module for `lang`: a key of English missing or extra, a plural
/// message without exactly the language's forms, or English forms mixed into it.
fn admin_module_problems(lang: &str, own_src: &str, en_src: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let (own, fallback) = match admin_text(own_src, en_src, lang == "en") {
        Ok(t) => t,
        Err(e) => return vec![format!("{lang}: {e}")],
    };
    let forms = ["zero", "one", "two", "few", "many", "other"];
    let base = |k: &str| match k.rsplit_once('.') {
        Some((b, f)) if forms.contains(&f) => (b.to_owned(), Some(f.to_owned())),
        _ => (k.to_owned(), None),
    };
    let bases = |m: &BTreeMap<String, String>| -> BTreeMap<String, BTreeSet<String>> {
        let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for k in m.keys() {
            let (b, f) = base(k);
            out.entry(b).or_default().extend(f);
        }
        out
    };
    let (en, _) = admin_text(en_src, en_src, true).unwrap_or_default();
    let want = bases(&en);
    let mine = bases(&own);
    let theirs = bases(&fallback);
    for (b, en_forms) in &want {
        match (mine.get(b), theirs.get(b)) {
            (None, None) => problems.push(format!("{lang}: {b} missing from the admin text")),
            (Some(_), Some(_)) => problems.push(format!(
                "{lang}: {b} in both the language and its English fallback"
            )),
            (Some(f), None) if !en_forms.is_empty() => {
                let need: BTreeSet<String> = categories(lang)
                    .iter()
                    .map(|c| c.name().to_owned())
                    .collect();
                if *f != need {
                    problems.push(format!(
                        "{lang}: {b} has forms {f:?}, the language needs {need:?}"
                    ));
                }
            }
            _ => {}
        }
    }
    for b in mine.keys().chain(theirs.keys()) {
        if !want.contains_key(b) {
            problems.push(format!("{lang}: {b} is not an English admin key"));
        }
    }
    problems
}

#[test]
fn admin_text_modules_have_every_key_once_in_the_right_forms() {
    let dir = root().join("locales");
    let en = fs::read_to_string(dir.join("en.toml")).unwrap();
    let mut problems = Vec::new();
    for file in files(&dir, "toml") {
        let lang = file.file_stem().unwrap().to_string_lossy().into_owned();
        problems.extend(admin_module_problems(
            &lang,
            &fs::read_to_string(&file).unwrap(),
            &en,
        ));
    }
    assert!(
        problems.is_empty(),
        "admin text modules:\n  {}",
        problems.join("\n  ")
    );
}

#[test]
fn admin_text_check_catches_missing_extra_and_wrong_forms() {
    let en = "[admin.a]\nx = \"X\"\nn = { one = \"1\", other = \"N\" }\n";
    // Vietnamese: `n` in English forms, an extra key; `x` comes from English, as it should.
    let vi = "[admin.a]\nn = { one = \"1\", other = \"N\" }\nz = \"Z\"\n";
    let got = admin_module_problems("vi", vi, en);
    assert_eq!(got.len(), 2, "{got:?}");
    assert!(got[0].contains("admin.a.n has forms"), "{got:?}");
    assert!(
        got[1].contains("admin.a.z is not an English admin key"),
        "{got:?}"
    );
    assert!(
        admin_module_problems("vi", "[admin.a]\nx = \"X\"\nn = { other = \"N\" }\n", en).is_empty()
    );
}

#[test]
fn ts_scanner_reads_literal_and_declared_calls() {
    let src = "h(\"b\", {}, t(\"a.b\"));\nconst x = t( 'c.d', { n: 1 });\n// i18n: e.one e.two\nt(`e.${k}`);\nt(`f.${k}`);\nconst g = t;\nt(key);\n// t(\"in.comment\")\nimport { t } from \"./i18n\";\nconst s = \"don't t\"; at(\"no\"); x.t(\"no\"); let tt = 1;";
    let (uses, misuse) = ts_uses(src);
    let got: Vec<String> = uses
        .iter()
        .map(|(u, l)| match u {
            Use::Literal(k) => format!("{l}:{k}"),
            Use::Dynamic { prefix, declared } => format!("{l}:{prefix}*{declared:?}"),
        })
        .collect();
    assert_eq!(
        got,
        [
            "1:a.b",
            "2:c.d",
            "4:e.*Some([\"e.one\", \"e.two\"])",
            "5:f.*None"
        ]
    );
    let lines: Vec<usize> = misuse.iter().map(|(_, l)| *l).collect();
    assert_eq!(lines, [6, 7], "{misuse:?}");
}

#[test]
fn ts_scanner_rejects_text_that_bypasses_t() {
    let src = "import { t as tr } from \"./i18n\";\n\
        h(\"button\", {}, \"Save\");\n\
        h(\"a\", { \"aria-label\": \"Close\", title: 'Hi' }, icon(\"x\"));\n\
        const f = { placeholder: `Type`, alt: \"Pic\" };\n\
        h(\"p\", {},\n  t(\"ok.key\"),\n  \"Late\");\n\
        h(\"span\", { class: \"k\" }, \"\u{b7}\", n, \"\", \" \\u00b7 \\n\");\n\
        // h(\"b\", {}, \"in a comment\")\n\
        const s = \"h(\\\"b\\\", {}, \\\"in a string\\\")\";\n\
        h(\"i\", { title: t(\"k\"), \"aria-label\": label });\n\
        import { t } from \"./i18n\"; const at = { title: x };";
    let got: Vec<usize> = ts_hardcoded(src).iter().map(|(_, l)| *l).collect();
    assert_eq!(got, [1, 2, 3, 3, 4, 4, 7], "{:?}", ts_hardcoded(src));
}

#[test]
fn template_scanner_allows_only_direct_calls() {
    let tpl = "{{ t(\"post.next\") }} {{ slot(\"after_post\") }} {{ t( 'a.b' ) }} {{ x.t(\"no\") }} <p>t(\"html text\")</p>\n{{ t (\"sp.ace\") }} {{ t\n(\"new.line\", n=1) }}\n{#- i18n: c.notice_ok c.notice_error #}\n{{ t(\"c.notice_\" ~ kind) }} {{ t(name) }}\n{% set f = t %}{{ [t][0](\"x\") }}";
    let (uses, misuse) = template_uses(tpl);
    let uses: Vec<Use> = uses.into_iter().map(|(u, _)| u).collect();
    let declared = Some(vec!["c.notice_ok".into(), "c.notice_error".into()]);
    assert_eq!(
        uses,
        [
            Use::Literal("post.next".into()),
            Use::Literal("a.b".into()),
            Use::Literal("sp.ace".into()),
            Use::Literal("new.line".into()),
            Use::Dynamic {
                prefix: "c.notice_".into(),
                declared: declared.clone()
            },
            Use::Dynamic {
                prefix: String::new(),
                declared
            },
        ]
    );
    assert_eq!(misuse.len(), 2, "{misuse:?}");
}

#[test]
fn rust_scanner_reads_key_macros_and_rejects_bypasses() {
    let mut problems = Vec::new();
    let src = "const A: Key = key!(\"a.b\");\nconst B: Key = key! ( \"c.d\" );\n// key!(\"in.comment\")\nconst C: Key = key!(r\"raw\");\nlet k = Key::__from_literal(S);\nlet t = c.t_template(\"x\", &[], &m);";
    let keys = rust_keys(src, false, &mut problems, "f.rs");
    assert_eq!(keys, ["a.b", "c.d"]);
    assert_eq!(problems.len(), 3, "{problems:?}");
    let mut ok = Vec::new();
    rust_keys("c.t_template(k, &a, &m)", true, &mut ok, "theme.rs");
    assert!(ok.is_empty());
}

#[test]
fn cli_scanner_rejects_hard_coded_text() {
    let src = "fn main() {\n    eprintln!(\"error: {msg}\");\n    print!(\"{text}\");\n    eprint!(\n        \"Bad {x}\"\n    );\n    // println!(\"comment\");\n    cmd.about(\"Some help\").help(t(k));\n}\n#[derive(Parser)]\nstruct Cli {\n    /// Help from a doc comment\n    #[arg(long, help = \"English\")]\n    x: bool,\n}\n/// Fine: not a clap item.\nfn f() {}\n#[cfg(test)]\nmod tests { fn t() { println!(\"test output\"); } }\n";
    let mut found: Vec<usize> = cli_hardcoded(src).into_iter().map(|(_, l)| l).collect();
    found.sort_unstable();
    assert_eq!(found, [2, 4, 8, 12, 13], "{:?}", cli_hardcoded(src));
}
