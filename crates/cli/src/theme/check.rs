//! `blog theme check` and `blog plugin check`: load the folder with the loader the Worker
//! uses (never a second copy of its rules), then what only a folder check can see: contrast,
//! size, rendering every page kind, files that must not be public.

use super::contrast;
use super::files::{DirFiles, Embedded, MAX_FILE, MAX_TEXT};
use super::finding::{Finding, Kind, Level};
use super::hardtext;
use super::sample::Sample;
use super::secrets;
use plumblog_render::pushed::MAX_DOCUMENT;
use plumblog_render::{Plugin, Theme, ThemeFiles};
use std::collections::BTreeMap;

/// Reader page CSS budget, compressed as it travels (docs/ARCHITECTURE.md, Budgets).
const CSS_BUDGET: usize = 15 * 1024;
/// Reader page JS budget with the theme toggle and TOC.
const JS_BUDGET: usize = 5 * 1024;
const ASSETS_URL: &str = "/_theme";

/// Top-level names a theme folder may hold. `styles/` holds the CSS of a `choice` option.
pub const THEME_LAYOUT: &[&str] = &[
    "theme.toml",
    "style.css",
    "templates",
    "styles",
    "assets",
    "locales",
];
pub const PLUGIN_LAYOUT: &[&str] = &["plugin.toml", "partials", "assets", "locales"];

/// Bytes gzip would send (deflate plus the 18-byte gzip frame).
#[must_use]
pub fn gzip_len(text: &str) -> usize {
    miniz_oxide::deflate::compress_to_vec(text.as_bytes(), 6).len() + 18
}

/// The loader's view: the folder without files already reported, so one stray file does not
/// hide every other finding behind the loader's first error.
pub struct View<'a> {
    files: &'a DirFiles,
    skip: Vec<String>,
}

impl View<'_> {
    /// A file is hidden by its own name, and its JSON form by the TOML file's name.
    fn hidden(&self, path: &str) -> bool {
        let source = path.strip_suffix(".json").unwrap_or(path);
        self.skip.iter().any(|s| s == path || s == source)
    }
}

impl ThemeFiles for View<'_> {
    fn read(&self, path: &str) -> Option<&str> {
        if self.hidden(path) {
            return None;
        }
        ThemeFiles::read(self.files, path)
    }
    fn paths(&self) -> Vec<&str> {
        let mut v = ThemeFiles::paths(self.files);
        v.retain(|p| !self.hidden(p));
        v
    }
}

/// TOML files the loader cannot read because they are not valid TOML (or, for a manifest, not
/// the shape it expects): one finding each, naming the file. Files already hidden from the
/// loader (stray, secret) are reported for that instead.
fn conversion_findings(
    files: &DirFiles,
    skip: &[String],
    kind: Kind,
    plugin: &str,
) -> Vec<Finding> {
    files
        .convert_errors
        .iter()
        .filter(|(path, _)| !skip.contains(path))
        .map(|(_, msg)| load_failure(files, msg, kind, plugin))
        .collect()
}

/// Layout and security findings shared by themes and plugins, and the paths to hide from
/// the loader.
#[must_use]
pub fn files_findings(files: &DirFiles, layout: &[&str]) -> (Vec<Finding>, Vec<String>) {
    let err = |k, file: &str, line| Finding::new(Level::Error, k, file, line);
    let mut out = Vec::new();
    let mut skip = Vec::new();
    for link in &files.symlinks {
        out.push(err(Kind::FileSymlink, link, None));
        skip.push(link.clone());
    }
    if files.too_many {
        out.push(err(Kind::FileTooMany, "", None));
    }
    for (path, size) in &files.sizes {
        let text_file = !path.starts_with("assets/");
        let max = if text_file { MAX_TEXT } else { MAX_FILE };
        if *size > max {
            out.push(err(Kind::FileTooLarge, path, None).arg("max", max.to_string()));
            skip.push(path.clone());
        }
    }
    for path in &files.not_text {
        out.push(err(Kind::FileNotText, path, None));
        skip.push(path.clone());
    }
    for (path, text) in files.scanned() {
        let top = path.split('/').next().unwrap_or_default();
        // Folders hold files; `theme.toml` and the like are files at the top only.
        let allowed = if path.contains('/') {
            layout.contains(&top) && !top.contains('.')
        } else {
            layout.contains(&path)
        };
        if let Some(why) = secrets::bad_name(path) {
            out.push(err(Kind::FileSecretName, path, None).arg("why", why));
            skip.push(path.to_owned());
        } else if !allowed {
            out.push(err(Kind::FileStray, path, None).arg("layout", layout.join(", ")));
            skip.push(path.to_owned());
        }
        if path
            .split('/')
            .any(|c| c.to_ascii_lowercase().starts_with("admin"))
        {
            out.push(err(Kind::FileAdmin, path, None));
            skip.push(path.to_owned());
        }
        for (line, kind) in secrets::in_text(text) {
            out.push(err(Kind::FileSecretContent, path, Some(line)).arg("name", kind));
        }
    }
    skip.sort();
    skip.dedup();
    (out, skip)
}

/// `(file, line)` guessed from a loader or template error text.
fn locate(detail: &str) -> (String, Option<usize>) {
    // minijinja: "... (in templates/x.html:3)" or "... in home.html line 3".
    let num = |s: &str| s.trim_end_matches([',', ':']).parse::<usize>().ok();
    let mut file = String::new();
    let mut line = None;
    let words: Vec<&str> = detail
        .split(|c: char| c.is_whitespace() || c == '(' || c == ')')
        .collect();
    for (i, w) in words.iter().enumerate() {
        if let Some((f, n)) = w
            .rsplit_once(':')
            .filter(|(f, n)| f.contains('.') && num(n).is_some())
        {
            return (f.to_owned(), num(n));
        }
        if *w == "line" {
            line = words.get(i + 1).and_then(|n| num(n));
        }
        if ["html", "toml", "css"].iter().any(|e| has_ext(w, e)) {
            (*w).clone_into(&mut file);
        }
    }
    let head = detail
        .split_once(": ")
        .map(|(h, _)| h)
        .filter(|h| h.contains('.') && !h.contains(' '));
    if let (true, Some(head)) = (file.is_empty(), head) {
        head.clone_into(&mut file);
    }
    (file, line)
}

fn has_ext(path: &str, ext: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

/// Kilobytes with one decimal, for messages.
fn kb(bytes: usize) -> String {
    format!(
        "{:.1}",
        f64::from(u32::try_from(bytes).unwrap_or(u32::MAX)) / 1024.0
    )
}

/// Where a loader error says the problem is, with `templates/` added to bare template names.
fn locate_in(files: &DirFiles, detail: &str) -> (String, Option<usize>) {
    let (mut file, line) = locate(detail);
    if has_ext(&file, "html") && !file.contains('/') {
        let t = format!("templates/{file}");
        if files.text(&t).is_empty() && files.iter().all(|(p, _)| p != t) {
            // Not in the folder: it comes from the default theme.
        } else {
            file = t;
        }
    }
    (file, line)
}

fn line_of(src: &str, section: &str, key: &str) -> Option<usize> {
    let mut in_section = false;
    for (n, l) in src.lines().enumerate() {
        let t = l.trim();
        if t.starts_with('[') {
            in_section = t.trim_start_matches('[').trim_end_matches(']') == section;
        } else if in_section && t.split(['=', ' ']).next() == Some(key) {
            return Some(n + 1);
        }
    }
    None
}

fn tokens(manifest: &toml::Table, mode: &str) -> BTreeMap<String, String> {
    manifest
        .get("colors")
        .and_then(|c| c.get(mode))
        .and_then(toml::Value::as_table)
        .map(|t| {
            t.iter()
                .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                .collect()
        })
        .unwrap_or_default()
}

/// Text pairs a theme's palette must keep readable (WCAG AA, 4.5:1): `(text, background)`.
fn pairs(palette: &BTreeMap<String, String>) -> Vec<(String, String)> {
    let mut v = vec![
        ("text".to_owned(), "bg".to_owned()),
        ("text-2".to_owned(), "bg".to_owned()),
        ("accent".to_owned(), "bg".to_owned()),
        ("on-accent".to_owned(), "accent".to_owned()),
        ("text".to_owned(), "surface".to_owned()),
        ("code".to_owned(), "surface".to_owned()),
    ];
    v.extend(
        palette
            .keys()
            .filter(|k| k.starts_with("syn-"))
            .map(|k| (k.clone(), "surface".to_owned())),
    );
    v
}

fn contrast_findings(src: &str, manifest: &toml::Table) -> Vec<Finding> {
    let mut out = Vec::new();
    for mode in ["light", "dark"] {
        let mut palette = tokens(manifest, mode);
        // A `color` option names the token it overrides; its default is what ships.
        let mut origin: BTreeMap<String, String> = BTreeMap::new();
        if let Some(opts) = manifest.get("options").and_then(toml::Value::as_table) {
            for (name, o) in opts {
                let (Some(token), Some(v)) = (
                    o.get("token").and_then(toml::Value::as_str),
                    o.get("default")
                        .and_then(|d| d.get(mode))
                        .and_then(toml::Value::as_str),
                ) else {
                    continue;
                };
                palette.insert(token.to_owned(), v.to_owned());
                origin.insert(token.to_owned(), name.clone());
            }
        }
        for (fg, bg) in pairs(&palette) {
            let (Some(a), Some(b)) = (
                palette.get(&fg).and_then(|s| contrast::parse_hex(s)),
                palette.get(&bg).and_then(|s| contrast::parse_hex(s)),
            ) else {
                continue;
            };
            let r = contrast::ratio(a, b);
            if r < 4.5 {
                let line = match origin.get(&fg).or_else(|| origin.get(&bg)) {
                    Some(opt) => line_of(src, &format!("options.{opt}"), "default")
                        .or_else(|| line_of(src, &format!("options.{opt}"), "token")),
                    None => line_of(src, &format!("colors.{mode}"), &fg),
                };
                out.push(
                    Finding::new(Level::Error, Kind::ThemeContrast, "theme.toml", line)
                        .arg("mode", mode)
                        .arg("fg", fg)
                        .arg("bg", bg)
                        .arg("ratio", format!("{r:.2}"))
                        .arg("need", "4.5"),
                );
            }
        }
    }
    out
}

fn dark_token_findings(src: &str, manifest: &toml::Table) -> Vec<Finding> {
    let (light, dark) = (tokens(manifest, "light"), tokens(manifest, "dark"));
    let mut out = Vec::new();
    for (have, other, from, to) in [
        ("light", "dark", &light, &dark),
        ("dark", "light", &dark, &light),
    ] {
        for k in from.keys().filter(|k| !to.contains_key(*k)) {
            out.push(
                Finding::new(
                    Level::Error,
                    Kind::ThemeDarkToken,
                    "theme.toml",
                    line_of(src, &format!("colors.{have}"), k),
                )
                .arg("name", k.clone())
                .arg("have", have)
                .arg("missing", other),
            );
        }
    }
    out
}

fn hard_colors(path: &str, css: &str) -> Vec<Finding> {
    let mut out = Vec::new();
    let mut in_comment = false;
    for (n, line) in css.lines().enumerate() {
        let mut rest = line;
        let mut code = String::new();
        while !rest.is_empty() {
            if in_comment {
                match rest.find("*/") {
                    Some(e) => {
                        in_comment = false;
                        rest = &rest[e + 2..];
                    }
                    None => rest = "",
                }
            } else if let Some(s) = rest.find("/*") {
                code.push_str(&rest[..s]);
                in_comment = true;
                rest = &rest[s + 2..];
            } else {
                code.push_str(rest);
                rest = "";
            }
        }
        if code.contains("url(") {
            continue;
        }
        let b = code.as_bytes();
        let hit = (0..b.len()).find(|&i| {
            b[i] == b'#'
                && matches!(
                    b.get(i + 1..)
                        .map(|r| r.iter().take_while(|c| c.is_ascii_hexdigit()).count()),
                    Some(3 | 4 | 6 | 8)
                )
                && !b.get(i + 1..).is_some_and(|r| {
                    r.get(r.iter().take_while(|c| c.is_ascii_hexdigit()).count())
                        .is_some_and(u8::is_ascii_alphanumeric)
                })
        });
        if let Some(i) = hit {
            let color: String = code[i..]
                .chars()
                .take_while(|c| *c == '#' || c.is_ascii_hexdigit())
                .collect();
            out.push(
                Finding::new(Level::Warning, Kind::ThemeHardColor, path, Some(n + 1))
                    .arg("text", color),
            );
        }
    }
    out
}

fn hard_text(path: &str, src: &str, level: Level) -> Vec<Finding> {
    hardtext::find(src)
        .into_iter()
        .map(|(line, text)| Finding::new(level, Kind::TextHard, path, Some(line)).arg("text", text))
        .collect()
}

fn load_failure(files: &DirFiles, detail: &str, kind: Kind, plugin: &str) -> Finding {
    let (mut file, line) = locate_in(files, detail);
    // Plugin errors name `plugins/<name>/<file>`.
    if let Some(rest) = file.strip_prefix(&format!("plugins/{plugin}/")) {
        file = rest.to_owned();
    }
    let kind =
        if kind != Kind::PluginLoad && (file.starts_with("templates/") || has_ext(&file, "html")) {
            Kind::ThemeTemplate
        } else {
            kind
        };
    Finding::new(Level::Error, kind, &file, line).arg("detail", detail.to_owned())
}

/// Everything wrong with the theme folder in `files`.
#[must_use]
pub fn check_theme(files: &DirFiles) -> Vec<Finding> {
    let (mut out, skip) = files_findings(files, THEME_LAYOUT);
    let broken = conversion_findings(files, &skip, Kind::ThemeLoad, "");
    if !broken.is_empty() {
        out.extend(broken);
        return out;
    }
    let view = View { files, skip };
    let src = if view.hidden("theme.toml") {
        ""
    } else {
        files.text("theme.toml")
    };
    let manifest: Option<toml::Table> = toml::from_str(src).ok();
    if let Some(m) = &manifest {
        let dark = dark_token_findings(src, m);
        let mismatch = !dark.is_empty();
        out.extend(dark);
        if mismatch {
            // The loader would stop at the same problem; the rest needs equal token sets.
            return out;
        }
    }
    let default = Embedded::default_theme();
    let theme = match Theme::load(&view, Some(&default), Vec::new(), ASSETS_URL) {
        Ok(t) => t,
        Err(e) => {
            out.push(load_failure(files, &e.detail, Kind::ThemeLoad, ""));
            return out;
        }
    };
    if let Some(m) = &manifest {
        out.extend(contrast_findings(src, m));
    }
    out.extend(size_findings(&theme, &view, manifest.as_ref()));
    out.extend(asset_name_findings(files));
    out.extend(document_size_finding(files));
    out.extend(missing_asset_findings(files, src, manifest.as_ref()));
    out.extend(render_findings(&theme, files));
    for (path, text) in files.iter() {
        if has_ext(path, "css") && !path.starts_with("assets/") {
            out.extend(hard_colors(path, text));
        }
        if path.starts_with("templates/") && has_ext(path, "html") {
            out.extend(hard_text(path, text, Level::Warning));
        }
    }
    out
}

/// Asset names are URL path segments as they are: a push serves them at the same path, and
/// a later deploy fetches them back by it. Anything else would need encoding somewhere.
fn plain_segment(c: &str) -> bool {
    !c.is_empty()
        && c.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn asset_name_findings(files: &DirFiles) -> Vec<Finding> {
    files
        .sizes
        .keys()
        .filter(|p| {
            p.strip_prefix("assets/")
                .is_some_and(|rest| !rest.split('/').all(plain_segment))
        })
        .map(|p| Finding::new(Level::Error, Kind::FileAssetName, p, None))
        .collect()
}

/// `theme.json` over [`MAX_DOCUMENT`]: the Worker would not load it. Measured as a push
/// writes it (the text files as JSON, plus about a line per asset), so check and push agree.
fn document_size_finding(files: &DirFiles) -> Option<Finding> {
    let text = files.document();
    let assets = files
        .sizes
        .keys()
        .filter(|p| p.starts_with("assets/"))
        .count();
    let size = serde_json::to_string(&text).map_or(0, |j| j.len()) + assets * 96;
    (size > MAX_DOCUMENT).then(|| {
        Finding::new(Level::Error, Kind::ThemeTooLarge, "", None)
            .arg("size", kb(size))
            .arg("max", (MAX_DOCUMENT / 1024).to_string())
    })
}

/// Line (1-based) of byte offset `at` in `src`.
fn line_at(src: &str, at: usize) -> usize {
    src[..at].matches('\n').count() + 1
}

/// `asset("x")` and `asset('x')` calls with a literal path, as `(line, x)`.
fn asset_calls(src: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    for (at, _) in src.match_indices("asset(") {
        // `asset(` inside a longer name (`my_asset(`) is another function.
        if src[..at]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            continue;
        }
        let rest = src[at + 6..].trim_start();
        let Some(q) = rest.chars().next().filter(|q| matches!(q, '"' | '\'')) else {
            continue;
        };
        if let Some((path, _)) = rest[1..].split_once(q) {
            out.push((line_at(src, at), path.to_owned()));
        }
    }
    out
}

/// `path` resolved against the folder `base` (`/`-separated), `None` when it leaves the root.
fn join_rel(base: &str, path: &str) -> Option<String> {
    let mut parts: Vec<&str> = base.split('/').filter(|c| !c.is_empty()).collect();
    for c in path.split('/') {
        match c {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            c => parts.push(c),
        }
    }
    Some(parts.join("/"))
}

/// Relative `url(...)` targets of a stylesheet under `assets/` as `(line, path in the
/// folder)`. In the fonts file, `{{fonts}}` is the file's own folder.
fn css_urls(path: &str, css: &str) -> Vec<(usize, String)> {
    let dir = path.rsplit_once('/').map_or("", |(d, _)| d);
    let mut out = Vec::new();
    for (at, _) in css.match_indices("url(") {
        let raw = css[at + 4..].split(')').next().unwrap_or_default();
        let url = raw.trim().trim_matches(['"', '\'']).trim();
        let url = url.strip_prefix("{{fonts}}/").unwrap_or(url);
        let skip = url.is_empty()
            || url.starts_with(['/', '#'])
            || url.contains(':')
            || url.contains("{{");
        if skip {
            continue;
        }
        let url = url.split(['?', '#']).next().unwrap_or_default();
        if let Some(p) = join_rel(dir, url) {
            out.push((line_at(css, at), p));
        }
    }
    out
}

/// Paths under `assets/` named in `theme.toml` (`[fonts] preload`, `preload_by_lang`,
/// `[js] files`), as `(line, path)`.
fn manifest_assets(src: &str, manifest: &toml::Table) -> Vec<(Option<usize>, String)> {
    let strings = |v: Option<&toml::Value>| -> Vec<String> {
        v.and_then(toml::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    };
    let fonts = manifest.get("fonts");
    let mut paths = strings(fonts.and_then(|f| f.get("preload")));
    if let Some(by) = fonts
        .and_then(|f| f.get("preload_by_lang"))
        .and_then(toml::Value::as_table)
    {
        for v in by.values() {
            paths.extend(strings(Some(v)));
        }
    }
    paths.extend(strings(manifest.get("js").and_then(|j| j.get("files"))));
    paths
        .into_iter()
        .filter(|p| p.starts_with("assets/"))
        .map(|p| {
            let line = src.find(&format!("\"{p}\"")).map(|at| line_at(src, at));
            (line, p)
        })
        .collect()
}

/// Files the theme uses from its `assets/` that are not there: `asset()` in its templates
/// and in the default templates it borrows, `url()` in its stylesheets under `assets/`, and
/// paths in `theme.toml`. The Worker serves a pushed theme's own `assets/` only, so each would
/// be a 404 on the live site.
fn missing_asset_findings(
    files: &DirFiles,
    src: &str,
    manifest: Option<&toml::Table>,
) -> Vec<Finding> {
    let mut refs: Vec<(String, Option<usize>, String)> = Vec::new();
    let default = Embedded::default_theme();
    let own: Vec<(&str, &str)> = files.iter().collect();
    let borrowed = ThemeFiles::paths(&default)
        .into_iter()
        .filter(|p| p.starts_with("templates/") && !files.sizes.contains_key(*p))
        .filter_map(|p| Some((p, default.read(p)?)));
    for (path, text) in own
        .iter()
        .copied()
        .filter(|(p, _)| p.starts_with("templates/"))
        .chain(borrowed)
        .filter(|(p, _)| has_ext(p, "html"))
    {
        for (line, a) in asset_calls(text) {
            refs.push((path.to_owned(), Some(line), format!("assets/{a}")));
        }
    }
    for (path, text) in &own {
        if path.starts_with("assets/") && has_ext(path, "css") {
            for (line, p) in css_urls(path, text) {
                refs.push(((*path).to_owned(), Some(line), p));
            }
        }
    }
    if let Some(m) = manifest {
        for (line, p) in manifest_assets(src, m) {
            refs.push(("theme.toml".to_owned(), line, p));
        }
    }
    let mut seen = std::collections::BTreeSet::new();
    refs.into_iter()
        .filter(|(_, _, a)| !files.sizes.contains_key(a))
        .filter(|(file, _, a)| seen.insert((file.clone(), a.clone())))
        .map(|(file, line, a)| {
            Finding::new(Level::Error, Kind::ThemeAssetMissing, &file, line).arg("asset", a)
        })
        .collect()
}

fn size_findings(
    theme: &Theme,
    files: &dyn ThemeFiles,
    manifest: Option<&toml::Table>,
) -> Vec<Finding> {
    let mut out = Vec::new();
    let css = gzip_len(theme.css());
    if css > CSS_BUDGET {
        out.push(
            Finding::new(Level::Error, Kind::ThemeCssSize, "style.css", None)
                .arg("size", kb(css))
                .arg("max", (CSS_BUDGET / 1024).to_string()),
        );
    }
    let js: Vec<String> = manifest
        .and_then(|m| m.get("js"))
        .and_then(|j| j.get("files"))
        .and_then(toml::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let total: usize = js
        .iter()
        .map(|p| gzip_len(files.read(p).unwrap_or_default()))
        .sum();
    if total > JS_BUDGET {
        out.push(
            Finding::new(Level::Warning, Kind::ThemeJsSize, "theme.toml", None)
                .arg("size", kb(total))
                .arg("max", (JS_BUDGET / 1024).to_string()),
        );
    }
    out
}

/// Renders every page kind over the sample blog; each failure or missing text key is a
/// finding. Template errors are reported once per file and line.
fn render_findings(theme: &Theme, files: &DirFiles) -> Vec<Finding> {
    let sample = Sample::load();
    let mut out: Vec<Finding> = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for target in sample.check_targets() {
        let (_, result) = sample.render(theme, &target, sample.lang());
        match result {
            Err(e) => {
                let f = load_failure(files, &e.detail, Kind::ThemeTemplate, "");
                if seen.insert((f.file.clone(), f.line, e.detail.clone())) {
                    out.push(f.arg("page", target));
                }
            }
            Ok(r) => {
                for (lang, key) in r.missing {
                    if seen.insert((format!("key {lang}"), None, key.clone())) {
                        out.push(
                            Finding::new(Level::Warning, Kind::ThemeLocaleKey, "", None)
                                .arg("key", key)
                                .arg("lang", lang),
                        );
                    }
                }
                for p in r.problems {
                    if seen.insert((String::new(), None, p.clone())) {
                        out.push(
                            Finding::new(Level::Warning, Kind::ThemePageProblem, "", None)
                                .arg("detail", p),
                        );
                    }
                }
            }
        }
    }
    out
}

/// Everything wrong with the plugin folder in `files`; `name` is the folder's name.
#[must_use]
pub fn check_plugin(files: &DirFiles, name: &str) -> Vec<Finding> {
    let (mut out, skip) = files_findings(files, PLUGIN_LAYOUT);
    let broken = conversion_findings(files, &skip, Kind::PluginLoad, name);
    if !broken.is_empty() {
        out.extend(broken);
        return out;
    }
    let view = View { files, skip };
    let url = format!("/plugin/{name}");
    let plugin = match Plugin::load(name, &view, &url) {
        Ok(p) => p,
        Err(e) => {
            out.push(load_failure(files, &e.detail, Kind::PluginLoad, name));
            return out;
        }
    };
    for (path, text) in files.iter() {
        if path.starts_with("partials/") && has_ext(path, "html") {
            out.extend(hard_text(path, text, Level::Error));
        }
    }
    // Enabled on the default theme: the page must render with its partials, within budget.
    let default = Embedded::default_theme();
    let theme = match Theme::load(&default, None, vec![plugin], ASSETS_URL) {
        Ok(t) => t,
        Err(e) => {
            out.push(
                Finding::new(Level::Error, Kind::PluginLoad, "", None).arg("detail", e.detail),
            );
            return out;
        }
    };
    let (on, problems) = theme.with_options(&[], &[(name.to_owned(), Vec::new())]);
    for p in problems {
        out.push(Finding::new(Level::Error, Kind::PluginBroken, "", None).arg("detail", p));
    }
    let sample = Sample::load();
    let mut seen = std::collections::BTreeSet::new();
    for target in sample.check_targets().into_iter().take(4) {
        match sample.render(&on, &target, sample.lang()).1 {
            Ok(r) => {
                for p in r.problems.into_iter().filter(|p| seen.insert(p.clone())) {
                    out.push(
                        Finding::new(Level::Error, Kind::PluginBroken, "", None).arg("detail", p),
                    );
                }
            }
            Err(e) => {
                if seen.insert(e.detail.clone()) {
                    out.push(
                        Finding::new(Level::Error, Kind::PluginBroken, "", None)
                            .arg("detail", e.detail),
                    );
                }
            }
        }
    }
    let with = gzip_len(on.css());
    let alone = gzip_len(theme.css());
    if with > CSS_BUDGET && alone <= CSS_BUDGET {
        out.push(
            Finding::new(Level::Error, Kind::PluginCssSize, "", None)
                .arg("size", kb(with))
                .arg("max", (CSS_BUDGET / 1024).to_string()),
        );
    }
    out
}
