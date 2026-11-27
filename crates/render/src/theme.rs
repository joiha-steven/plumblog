//! Loading a theme and rendering pages with it.

use crate::data::{Body, EntryData, LinkData, PageInfo, Post, PostData, Site, civil, cut_words};
use crate::keys;
use crate::{PageKind, SLOTS, css};
use minijinja::value::{Kwargs, Serde, Value};
use minijinja::{Environment, State};
use plumblog_i18n::{Catalog, Key, MissingKeys};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::fmt::{self, Write as _};
use std::sync::Arc;

/// Read-only access to one theme folder (paths relative to it, `/`-separated).
pub trait ThemeFiles {
    fn read(&self, path: &str) -> Option<&str>;
    /// Every file path, so templates and locales can be discovered.
    fn paths(&self) -> Vec<&str>;
}

/// `(path, text)` pairs, e.g. files embedded with `include_str!`.
impl ThemeFiles for &[(&str, &str)] {
    fn read(&self, path: &str) -> Option<&str> {
        self.iter().find(|(p, _)| *p == path).map(|(_, s)| *s)
    }
    fn paths(&self) -> Vec<&str> {
        self.iter().map(|(p, _)| *p).collect()
    }
}

/// A theme that cannot load or a page that cannot render. `detail` names the file (and line,
/// for templates) so an AI fixing the theme knows where to look.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeError {
    pub detail: String,
}

impl ThemeError {
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl fmt::Display for ThemeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.detail)
    }
}

impl std::error::Error for ThemeError {}

impl From<minijinja::Error> for ThemeError {
    fn from(e: minijinja::Error) -> Self {
        // `{:#}` includes the template name, line and source excerpt.
        Self::new(format!("{e:#}"))
    }
}

#[derive(Deserialize)]
struct Manifest {
    name: String,
    #[serde(default)]
    fonts: Fonts,
    #[serde(default)]
    js: Js,
    #[serde(default)]
    options: BTreeMap<String, OptionDecl>,
    colors: Colors,
}

#[derive(Default, Deserialize)]
struct Fonts {
    faces: Option<String>,
    #[serde(default)]
    preload: Vec<String>,
    #[serde(default)]
    preload_by_lang: BTreeMap<String, Vec<String>>,
}

#[derive(Default, Deserialize)]
struct Js {
    #[serde(default)]
    files: Vec<String>,
}

/// The admin's fixed field types (docs/ARCHITECTURE.md, Plugins and options). Parsing them here
/// means a theme with an unknown type fails to load instead of rendering half its options.
#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
enum FieldType {
    ShortText,
    LongText,
    Number,
    Toggle,
    Choice,
    Color,
    Image,
    List,
}

#[derive(Deserialize)]
struct OptionDecl {
    #[serde(rename = "type")]
    kind: FieldType,
    default: toml::Value,
    /// For `color`: the token it overrides.
    token: Option<String>,
    #[serde(default)]
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    value: String,
    css: Option<String>,
}

#[derive(Deserialize, Serialize, Clone)]
struct Colors {
    light: BTreeMap<String, String>,
    dark: BTreeMap<String, String>,
}

/// A loaded theme. Built once per Worker isolate and reused, so per-request work is only the
/// template render.
pub struct Theme {
    name: String,
    env: Environment<'static>,
    css: String,
    colors: Colors,
    options: BTreeMap<String, toml::Value>,
    preload: Vec<String>,
    preload_by_lang: BTreeMap<String, Vec<String>>,
    js: Vec<String>,
    catalogs: Arc<Catalogs>,
    missing: Arc<MissingKeys>,
}

struct Catalogs {
    by_lang: HashMap<String, Catalog>,
    english: Catalog,
}

impl Catalogs {
    fn get(&self, lang: &str) -> &Catalog {
        self.by_lang.get(lang).unwrap_or(&self.english)
    }
}

const FALLBACK_LANG: &str = "en";

impl Theme {
    /// Loads `theme`, taking any template or locale key it lacks from `default` (the default
    /// theme; `None` when loading the default theme itself). `chosen` holds the owner's option
    /// values; anything missing uses the declared default. `assets_url` is where the theme's
    /// `assets/` folder is served from, without a trailing slash.
    ///
    /// # Errors
    /// A missing or invalid `theme.toml`, locale file, stylesheet or template.
    pub fn load(
        theme: &dyn ThemeFiles,
        default: Option<&dyn ThemeFiles>,
        chosen: &BTreeMap<String, toml::Value>,
        assets_url: &str,
    ) -> Result<Self, ThemeError> {
        let manifest_src = theme
            .read("theme.toml")
            .ok_or_else(|| ThemeError::new("theme.toml is missing"))?;
        let manifest: Manifest = toml::from_str(manifest_src)
            .map_err(|e| ThemeError::new(format!("theme.toml: {e}")))?;
        if manifest.colors.light.keys().ne(manifest.colors.dark.keys()) {
            return Err(ThemeError::new(
                "theme.toml: [colors.light] and [colors.dark] must define the same keys",
            ));
        }
        let options = resolve_options(&manifest, chosen)?;
        let mut colors = manifest.colors.clone();
        apply_color_options(&manifest, &options, &mut colors)?;

        let asset = |path: &str| -> String {
            path.strip_prefix("assets/")
                .map_or_else(|| path.to_owned(), |p| format!("{assets_url}/{p}"))
        };
        let css = build_css(theme, &manifest, &options, &colors, &asset)?;

        let mut env = Environment::new();
        env.set_formatter(html_formatter);
        if let Some(fallback) = default {
            add_templates(&mut env, fallback, |p| theme.read(p).is_none())?;
        }
        add_templates(&mut env, theme, |_| true)?;

        let catalogs = Arc::new(load_catalogs(theme, default)?);
        let missing = Arc::new(MissingKeys::default());
        let for_t = Arc::clone(&catalogs);
        let missing_t = Arc::clone(&missing);
        env.add_function(
            "t",
            move |state: &State, key: &str, kwargs: Kwargs| -> Result<String, minijinja::Error> {
                let lang = page_lang(state);
                let args = kwargs_strings(&kwargs)?;
                let args: Vec<(&str, String)> =
                    args.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
                Ok(for_t.get(&lang).t_reporting(key, &args, &missing_t))
            },
        );
        env.add_function("slot", |name: &str| -> Result<Value, minijinja::Error> {
            if SLOTS.contains(&name) {
                // Plugins land with issue #15; until then every slot is empty.
                Ok(Value::from_safe_string(String::new()))
            } else {
                Err(minijinja::Error::new(
                    minijinja::ErrorKind::InvalidOperation,
                    format!("unknown slot {name:?}; known: {}", SLOTS.join(", ")),
                ))
            }
        });

        Ok(Self {
            name: manifest.name,
            env,
            css,
            colors,
            options,
            preload: manifest.fonts.preload.iter().map(|p| asset(p)).collect(),
            preload_by_lang: manifest
                .fonts
                .preload_by_lang
                .iter()
                .map(|(l, v)| (l.clone(), v.iter().map(|p| asset(p)).collect()))
                .collect(),
            js: manifest.js.files.iter().map(|p| asset(p)).collect(),
            catalogs,
            missing,
        })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The inlined stylesheet, for the size budget.
    #[must_use]
    pub fn css(&self) -> &str {
        &self.css
    }

    /// Text for `key` in `lang` (theme locale, then English, then the key itself).
    #[must_use]
    pub fn text(&self, lang: &str, key: Key, args: &[(&str, String)]) -> String {
        self.catalogs
            .get(lang)
            .t_reporting(key.0, args, &self.missing)
    }

    /// Keys rendering asked for that exist in neither the page language nor English, as
    /// `(lang, key)`, each once since the last call. The Worker logs each as
    /// `i18n.missing_key` after a render.
    #[must_use]
    pub fn take_missing_keys(&self) -> Vec<(String, String)> {
        self.missing.take()
    }

    /// Renders one reader page.
    ///
    /// # Errors
    /// A template error, with template name and line.
    pub fn render<'b>(
        &self,
        site: &Site,
        page: &PageInfo,
        body: &Body<'b>,
    ) -> Result<String, ThemeError> {
        let lang = page.lang.as_str();
        let entries = |posts: &[&'b Post]| self.entries(posts);
        let (kind, description, data) = match body {
            Body::Home {
                posts,
                older_url,
                newer_url,
            } => (
                PageKind::Home,
                site.description.clone(),
                ser(HomeData {
                    posts: entries(posts),
                    older_url: older_url.as_deref(),
                    newer_url: newer_url.as_deref(),
                }),
            ),
            Body::Post { post, prev, next } => {
                let entry = self.entry(post);
                let description = entry.excerpt.clone();
                let data = PostData {
                    entry,
                    html: &post.body.html,
                    has_code: post.body.has_code,
                    has_math: post.body.has_math,
                    has_embed: post.body.has_embed,
                    tags: &post.tags,
                    prev: link(*prev),
                    next: link(*next),
                };
                (PageKind::Post, description, ser(PostPage { post: data }))
            }
            Body::Tag {
                tag,
                posts,
                older_url,
                newer_url,
            } => (
                PageKind::Tag,
                site.description.clone(),
                ser(TagData {
                    tag,
                    posts: entries(posts),
                    older_url: older_url.as_deref(),
                    newer_url: newer_url.as_deref(),
                }),
            ),
            Body::Archive {
                year,
                posts,
                years,
                older_url,
                newer_url,
            } => (
                PageKind::Archive,
                site.description.clone(),
                ser(ArchiveData {
                    year: *year,
                    posts: entries(posts),
                    years,
                    older_url: older_url.as_deref(),
                    newer_url: newer_url.as_deref(),
                }),
            ),
            Body::NotFound { recent } => (
                PageKind::NotFound,
                site.description.clone(),
                ser(NotFoundData {
                    posts: entries(recent),
                }),
            ),
        };
        let mut preload: Vec<&str> = self.preload.iter().map(String::as_str).collect();
        if let Some(extra) = self.preload_by_lang.get(lang) {
            preload.extend(extra.iter().map(String::as_str));
        }
        let ctx = Context {
            site,
            page: PageData {
                kind: kind.as_str(),
                lang,
                dir: if is_rtl(lang) { "rtl" } else { "ltr" },
                description: &description,
                canonical: &page.canonical,
                alternates: &[],
            },
            theme: ThemeData {
                css: &self.css,
                js: &self.js,
                preload,
                colors: &self.colors,
                options: &self.options,
            },
        };
        let ctx = minijinja::context! { ..ser(&ctx), ..data };
        let tmpl = self.env.get_template(kind.template())?;
        Ok(tmpl.render(ctx)?)
    }

    fn entries<'a>(&self, posts: &[&'a Post]) -> Vec<EntryData<'a>> {
        posts.iter().map(|p| self.entry(p)).collect()
    }

    fn entry<'a>(&self, post: &'a Post) -> EntryData<'a> {
        let (year, month, day) = civil(post.published_at);
        let excerpt_words = self
            .options
            .get("excerpt_words")
            .and_then(toml::Value::as_integer)
            .map_or(55, |n| usize::try_from(n).unwrap_or(0));
        let excerpt = match &post.excerpt {
            Some(e) if !e.trim().is_empty() => e.trim().to_owned(),
            _ => cut_words(&post.body.excerpt, excerpt_words),
        };
        // Computed on save by the markdown crate; reading never counts words.
        let minutes = post.body.reading_minutes;
        let date_display = self.text(
            &post.lang,
            keys::DATE_SHORT,
            &[
                ("day", day.to_string()),
                (
                    "month",
                    self.text(&post.lang, keys::MONTHS[month_index(month)], &[]),
                ),
                ("year", year.to_string()),
            ],
        );
        EntryData {
            title: &post.title,
            url: &post.url,
            lang: &post.lang,
            pinned: post.pinned,
            excerpt,
            date_iso: format!("{year:04}-{month:02}-{day:02}"),
            date_display,
            year,
            reading_minutes: minutes,
            cover: post.cover.as_ref(),
            thumb: post.thumb.as_ref(),
        }
    }
}

#[derive(Serialize)]
struct Context<'a> {
    site: &'a Site,
    page: PageData<'a>,
    theme: ThemeData<'a>,
}

#[derive(Serialize)]
struct PageData<'a> {
    kind: &'static str,
    lang: &'a str,
    dir: &'static str,
    description: &'a str,
    canonical: &'a str,
    /// Translations land with the translations issue; the template already loops over them.
    alternates: &'a [crate::Link],
}

#[derive(Serialize)]
struct ThemeData<'a> {
    css: &'a str,
    js: &'a [String],
    preload: Vec<&'a str>,
    colors: &'a Colors,
    options: &'a BTreeMap<String, toml::Value>,
}

#[derive(Serialize)]
struct HomeData<'a> {
    posts: Vec<EntryData<'a>>,
    older_url: Option<&'a str>,
    newer_url: Option<&'a str>,
}

#[derive(Serialize)]
struct PostPage<'a> {
    post: PostData<'a>,
}

#[derive(Serialize)]
struct TagData<'a> {
    tag: &'a crate::Link,
    posts: Vec<EntryData<'a>>,
    older_url: Option<&'a str>,
    newer_url: Option<&'a str>,
}

#[derive(Serialize)]
struct ArchiveData<'a> {
    year: i32,
    posts: Vec<EntryData<'a>>,
    years: &'a [crate::Link],
    older_url: Option<&'a str>,
    newer_url: Option<&'a str>,
}

#[derive(Serialize)]
struct NotFoundData<'a> {
    posts: Vec<EntryData<'a>>,
}

/// minijinja's HTML escaping also escapes `/`, which turns every URL into `&#x2f;` noise and
/// adds bytes to each page. Escaping `& < > " '` is what HTML needs (same as `MarkupSafe`).
fn html_formatter(
    out: &mut minijinja::Output,
    state: &mut State,
    value: &Value,
) -> Result<(), minijinja::Error> {
    let text = match value.as_str() {
        Some(s) if !value.is_safe() && *state.auto_escape() == minijinja::AutoEscape::Html => s,
        _ => return minijinja::escape_formatter(out, state, value),
    };
    let mut start = 0;
    for (i, b) in text.bytes().enumerate() {
        let rep = match b {
            b'&' => "&amp;",
            b'<' => "&lt;",
            b'>' => "&gt;",
            b'"' => "&quot;",
            b'\'' => "&#39;",
            _ => continue,
        };
        out.write_str(&text[start..i]).map_err(write_error)?;
        out.write_str(rep).map_err(write_error)?;
        start = i + 1;
    }
    out.write_str(&text[start..]).map_err(write_error)
}

fn write_error(e: std::fmt::Error) -> minijinja::Error {
    minijinja::Error::new(minijinja::ErrorKind::WriteFailure, e.to_string())
}

fn link(p: Option<&Post>) -> Option<LinkData<'_>> {
    p.map(|p| LinkData {
        title: &p.title,
        url: &p.url,
    })
}

fn ser<T: Serialize>(v: T) -> Value {
    Value::from(Serde(v))
}

/// `civil` months are 1-12; clamp so a bad timestamp can never index out of bounds.
fn month_index(month: u32) -> usize {
    usize::try_from(month.clamp(1, 12) - 1).unwrap_or(0)
}

fn page_lang(state: &State) -> String {
    state
        .lookup("page")
        .and_then(|p| p.get_attr("lang").ok())
        .and_then(|l| l.as_str().map(str::to_owned))
        .unwrap_or_else(|| FALLBACK_LANG.to_owned())
}

fn kwargs_strings(kwargs: &Kwargs) -> Result<Vec<(String, String)>, minijinja::Error> {
    kwargs
        .args()
        .map(|k| Ok((k.to_owned(), kwargs.get::<Value>(k)?.to_string())))
        .collect()
}

fn is_rtl(lang: &str) -> bool {
    matches!(
        lang.split('-').next().unwrap_or(lang),
        "ar" | "he" | "fa" | "ur" | "ps" | "yi"
    )
}

fn add_templates(
    env: &mut Environment<'static>,
    files: &dyn ThemeFiles,
    include: impl Fn(&str) -> bool,
) -> Result<(), ThemeError> {
    for path in files.paths() {
        let Some(name) = path.strip_prefix("templates/") else {
            continue;
        };
        if !include(path) {
            continue;
        }
        let src = files.read(path).unwrap_or_default();
        env.add_template_owned(name.to_owned(), src.to_owned())?;
    }
    Ok(())
}

fn load_catalogs(
    theme: &dyn ThemeFiles,
    default: Option<&dyn ThemeFiles>,
) -> Result<Catalogs, ThemeError> {
    // Default theme strings first, the theme's own on top: templates that fall back to the
    // default theme still find their keys.
    let mut merged: BTreeMap<String, HashMap<String, String>> = BTreeMap::new();
    for files in default.into_iter().chain([theme]) {
        for path in files.paths() {
            let Some(lang) = path
                .strip_prefix("locales/")
                .and_then(|f| f.strip_suffix(".toml"))
            else {
                continue;
            };
            let strings = plumblog_i18n::parse(files.read(path).unwrap_or_default())
                .map_err(|e| ThemeError::new(format!("{path}: {}", e.detail)))?;
            merged.entry(lang.to_owned()).or_default().extend(strings);
        }
    }
    let english_strings = merged.get(FALLBACK_LANG).cloned().unwrap_or_default();
    let by_lang = merged
        .into_iter()
        .map(|(lang, strings)| {
            let fallback = english_strings.clone();
            let catalog = Catalog::new(strings, fallback).with_lang(&lang);
            (lang, catalog)
        })
        .collect();
    Ok(Catalogs {
        by_lang,
        english: Catalog::new(english_strings, HashMap::new()),
    })
}

fn resolve_options(
    manifest: &Manifest,
    chosen: &BTreeMap<String, toml::Value>,
) -> Result<BTreeMap<String, toml::Value>, ThemeError> {
    let mut out = BTreeMap::new();
    for (key, decl) in &manifest.options {
        if decl.kind == FieldType::Choice && decl.choices.is_empty() {
            return Err(ThemeError::new(format!(
                "theme.toml: options.{key} is a choice without choices"
            )));
        }
        // A stored value that no longer fits the declaration (type changed, choice removed in a
        // theme update) falls back to the default instead of breaking the site.
        let value = chosen
            .get(key)
            .filter(|v| v.same_type(&decl.default))
            .filter(|v| {
                decl.kind != FieldType::Choice
                    || decl
                        .choices
                        .iter()
                        .any(|c| v.as_str() == Some(c.value.as_str()))
            })
            .unwrap_or(&decl.default);
        out.insert(key.clone(), value.clone());
    }
    Ok(out)
}

fn apply_color_options(
    manifest: &Manifest,
    options: &BTreeMap<String, toml::Value>,
    colors: &mut Colors,
) -> Result<(), ThemeError> {
    for (key, decl) in &manifest.options {
        if decl.kind != FieldType::Color {
            continue;
        }
        let token = decl.token.as_deref().unwrap_or(key);
        let value = &options[key];
        for (mode, map) in [("light", &mut colors.light), ("dark", &mut colors.dark)] {
            let Some(c) = value.get(mode).and_then(toml::Value::as_str) else {
                return Err(ThemeError::new(format!(
                    "theme.toml: options.{key} needs a {mode} color"
                )));
            };
            if !map.contains_key(token) {
                return Err(ThemeError::new(format!(
                    "theme.toml: options.{key} overrides unknown color token {token:?}"
                )));
            }
            map.insert(token.to_owned(), c.to_owned());
        }
    }
    Ok(())
}

fn build_css(
    files: &dyn ThemeFiles,
    manifest: &Manifest,
    options: &BTreeMap<String, toml::Value>,
    colors: &Colors,
    asset: &dyn Fn(&str) -> String,
) -> Result<String, ThemeError> {
    let read = |path: &str| {
        files
            .read(path)
            .ok_or_else(|| ThemeError::new(format!("{path} is missing")))
    };
    let mut out = String::new();
    if let Some(faces) = &manifest.fonts.faces {
        let dir = faces.rsplit_once('/').map_or("", |(d, _)| d);
        out.push_str(&read(faces)?.replace("{{fonts}}", &asset(dir)));
        out.push('\n');
    }
    // One block of tokens; `light-dark()` lets `color-scheme` switch the palette
    // (docs/ARCHITECTURE.md, renderer contract).
    out.push_str(":root{");
    for (k, light) in &colors.light {
        let dark = &colors.dark[k];
        // Writing to a String cannot fail.
        let _ = write!(out, "--c-{k}:light-dark({light},{dark});");
    }
    out.push_str("}\n");
    out.push_str(read("style.css")?);
    for (key, decl) in &manifest.options {
        if decl.kind != FieldType::Choice {
            continue;
        }
        let picked = options[key].as_str().unwrap_or_default();
        if let Some(path) = decl
            .choices
            .iter()
            .find(|c| c.value == picked)
            .and_then(|c| c.css.as_deref())
        {
            out.push('\n');
            out.push_str(read(path)?);
        }
    }
    Ok(css::minify(&out))
}
