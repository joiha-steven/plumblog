//! Loading a theme and rendering pages with it.

use crate::data::{
    Alternate, Body, CommentItemData, Comments, CommentsData, EntryData, LinkData, PageInfo, Post,
    PostData, Site, civil, cut_words,
};
use crate::keys;
use crate::options::{self, FieldType, Group, OptionDecl, Owner, Schema};
use crate::plugin::Plugin;
use crate::{PageKind, SLOTS, css, seo};
use minijinja::value::{Kwargs, Serde, Value};
use minijinja::{Environment, State};
use plumblog_i18n::{Catalog, Key, MissingKeys};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::{self, Write as _};
use std::sync::{Arc, Mutex};

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
    pub(crate) fn new(detail: impl Into<String>) -> Self {
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

/// The manifest as the loader reads it: `theme.toml` converted to JSON, because the Worker
/// links no TOML parser. Locale files follow the same rule ([`LOCALE_SUFFIX`]).
pub(crate) const MANIFEST: &str = "theme.toml.json";
/// A locale file is `locales/<lang>` + this.
pub(crate) const LOCALE_SUFFIX: &str = ".toml.json";

#[derive(Deserialize)]
pub(crate) struct Manifest {
    name: String,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    fonts: Fonts,
    #[serde(default)]
    js: Js,
    /// Plugin slots this theme's templates fill with `slot()`; a plugin partial for any
    /// other slot is not shown.
    #[serde(default)]
    slots: Vec<String>,
    #[serde(default)]
    groups: BTreeMap<String, Group>,
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

#[derive(Deserialize, Serialize, Clone)]
struct Colors {
    light: BTreeMap<String, String>,
    dark: BTreeMap<String, String>,
}

/// Everything loaded once per Worker isolate: parsing and compiling cost far more than a
/// render, so per-request work is [`Theme::with_options`] (cheap) and the template render.
struct Shared {
    name: String,
    version: Option<String>,
    env: Environment<'static>,
    schema: Schema,
    slots: Vec<String>,
    colors: Colors,
    /// Minified `faces.css` (font URLs filled in) and `style.css`.
    faces_css: String,
    style_css: String,
    /// Minified stylesheet of each style choice, by path.
    styles: BTreeMap<String, String>,
    preload: Vec<String>,
    preload_by_lang: BTreeMap<String, Vec<String>>,
    js: Vec<String>,
    catalogs: Arc<Catalogs>,
    plugins: Vec<PluginInfo>,
}

/// What rendering needs of a loaded plugin (its templates and locales live in `env` and the
/// `t` function).
struct PluginInfo {
    name: String,
    version: String,
    schema: Schema,
    /// Shared with the partials' `t()`, for the option labels the admin shows.
    catalogs: Arc<Catalogs>,
    slots: Vec<String>,
    css: String,
    js: Vec<String>,
}

/// One site's option values applied to the theme.
#[derive(Clone)]
struct Look {
    css: String,
    colors: Colors,
    /// What templates read as `theme.options` (see `options::template_values`).
    options: BTreeMap<String, Json>,
    /// Option values as stored or defaulted, for Rust lookups.
    values: BTreeMap<String, Json>,
    js: Vec<String>,
    plugins: Vec<ActivePlugin>,
}

/// An enabled plugin as its partials see it (`plugin.name`, `plugin.options`).
#[derive(Clone, Serialize)]
struct ActivePlugin {
    name: String,
    options: BTreeMap<String, Json>,
    slots: Vec<String>,
}

/// A loaded theme with one set of option values. [`Theme::load`] applies the defaults;
/// [`Theme::with_options`] gives a copy with a site's stored values.
#[derive(Clone)]
pub struct Theme {
    shared: Arc<Shared>,
    look: Look,
}

/// Keys one render asked for that exist nowhere, passed to the template `t` function through
/// the render context (`Theme` is shared across requests, so it must not hold them).
#[derive(Debug, Default)]
struct RenderMissing(MissingKeys);

impl minijinja::value::Object for RenderMissing {}

/// Context variable holding the render's [`RenderMissing`].
const MISSING_VAR: &str = "__i18n_missing";

/// Plugin partials that failed during one render; the page is served without them.
#[derive(Debug, Default)]
struct RenderProblems(Mutex<Vec<String>>);

impl minijinja::value::Object for RenderProblems {}

impl RenderProblems {
    fn push(&self, p: String) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(p);
    }
}

const PROBLEMS_VAR: &str = "__render_problems";
/// The enabled plugins, for `slot()`.
const PLUGINS_VAR: &str = "__plugins";
/// Set while a plugin partial renders, so `t()` reads that plugin's locales.
const PLUGIN_VAR: &str = "__plugin";

pub(crate) struct Catalogs {
    by_lang: HashMap<String, Catalog>,
    english: Catalog,
}

impl Catalogs {
    fn get(&self, lang: &str) -> &Catalog {
        self.by_lang.get(lang).unwrap_or(&self.english)
    }

    /// The catalog for `lang` and the language it is: `lang` itself, else its primary
    /// subtag (`vi-VN` -> `vi`), else English.
    fn resolve<'a>(&'a self, lang: &'a str) -> (&'a str, &'a Catalog) {
        let primary = lang.split('-').next().unwrap_or_default();
        [lang, primary]
            .into_iter()
            .find_map(|l| self.by_lang.get_key_value(l))
            .map_or((FALLBACK_LANG, &self.english), |(l, c)| (l.as_str(), c))
    }
}

const FALLBACK_LANG: &str = "en";

impl Theme {
    /// Loads `theme`, taking any template or locale key it lacks from `default` (the default
    /// theme; `None` when loading the default theme itself), with every option at its
    /// default. `plugins` are the bundled plugins that loaded; none is enabled until
    /// [`Theme::with_options`] says so. `assets_url` is where the theme's `assets/` folder
    /// is served from, without a trailing slash.
    ///
    /// # Errors
    /// A missing or invalid `theme.toml`, option declaration, locale file, stylesheet or
    /// template.
    pub fn load(
        theme: &dyn ThemeFiles,
        default: Option<&dyn ThemeFiles>,
        plugins: Vec<Plugin>,
        assets_url: &str,
    ) -> Result<Self, ThemeError> {
        let manifest_src = theme
            .read(MANIFEST)
            .ok_or_else(|| ThemeError::new(format!("{MANIFEST} is missing")))?;
        let manifest: Manifest = serde_json::from_str(manifest_src)
            .map_err(|e| ThemeError::new(format!("{MANIFEST}: {e}")))?;
        if manifest.colors.light.keys().ne(manifest.colors.dark.keys()) {
            return Err(ThemeError::new(
                "theme.toml: [colors.light] and [colors.dark] must define the same keys",
            ));
        }
        for slot in &manifest.slots {
            if !SLOTS.contains(&slot.as_str()) {
                return Err(ThemeError::new(format!(
                    "theme.toml: unknown slot {slot:?}; known: {}",
                    SLOTS.join(", ")
                )));
            }
        }
        let strings = locale_strings(theme, default)?;
        let english = strings.get(FALLBACK_LANG).cloned().unwrap_or_default();
        let tokens: BTreeSet<String> = manifest.colors.light.keys().cloned().collect();
        let schema = Schema::check(
            "theme.toml",
            manifest.options,
            manifest.groups,
            &english,
            Owner::Theme(&tokens),
        )
        .map_err(ThemeError::new)?;

        let asset = |path: &str| -> String {
            path.strip_prefix("assets/")
                .map_or_else(|| path.to_owned(), |p| format!("{assets_url}/{p}"))
        };
        let (faces_css, style_css, styles) =
            load_css(theme, manifest.fonts.faces.as_deref(), &schema, &asset)?;

        let mut env = Environment::new();
        env.set_formatter(html_formatter);
        if let Some(fallback) = default {
            add_templates(&mut env, fallback, |p| theme.read(p).is_none())?;
        }
        add_templates(&mut env, theme, |_| true)?;
        let (partials, infos) = partial_env(plugins)?;

        let catalogs = Arc::new(catalogs(strings));
        add_functions(
            &mut env,
            Arc::clone(&catalogs),
            Arc::new(partials),
            assets_url,
            manifest.slots.clone(),
        );

        let shared = Arc::new(Shared {
            name: manifest.name,
            env,
            version: manifest.version,
            schema,
            slots: manifest.slots,
            colors: manifest.colors,
            faces_css,
            style_css,
            styles,
            preload: manifest.fonts.preload.iter().map(|p| asset(p)).collect(),
            preload_by_lang: manifest
                .fonts
                .preload_by_lang
                .iter()
                .map(|(l, v)| (l.clone(), v.iter().map(|p| asset(p)).collect()))
                .collect(),
            js: manifest.js.files.iter().map(|p| asset(p)).collect(),
            catalogs,
            plugins: infos,
        });
        let (look, _) = look(&shared, &[], &[]);
        Ok(Self { shared, look })
    }

    /// This theme with a site's stored option values (`(key, value_json)`, as the store
    /// keeps them) and enabled plugins (`(name, stored values)`). Never fails: a value that
    /// does not fit, an unknown plugin or a slot this theme lacks is left out and reported,
    /// one line each, for the Worker log.
    #[must_use]
    pub fn with_options(
        &self,
        stored: &[(String, String)],
        plugins: &[(String, Vec<(String, String)>)],
    ) -> (Self, Vec<String>) {
        let (look, problems) = look(&self.shared, stored, plugins);
        (
            Self {
                shared: Arc::clone(&self.shared),
                look,
            },
            problems,
        )
    }

    /// Checks one option value (JSON as stored) against this theme's declaration, or bundled
    /// plugin `plugin`'s. `None`: no such plugin or key, so nothing to check against (such
    /// values are kept and simply not read).
    #[must_use]
    pub fn check_option(
        &self,
        plugin: Option<&str>,
        key: &str,
        raw: &str,
    ) -> Option<Result<(), String>> {
        let schema = match plugin {
            None => &self.shared.schema,
            Some(name) => &self.shared.plugins.iter().find(|p| p.name == name)?.schema,
        };
        schema.check_one(key, raw)
    }

    /// Names of the bundled plugins that loaded, for listing and enabling.
    #[must_use]
    pub fn plugin_names(&self) -> Vec<&str> {
        self.shared
            .plugins
            .iter()
            .map(|p| p.name.as_str())
            .collect()
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.shared.name
    }

    /// `version` from `theme.toml`, when it says one.
    #[must_use]
    pub fn version(&self) -> Option<&str> {
        self.shared.version.as_deref()
    }

    /// The option declarations of this theme, or of bundled plugin `plugin`, as the JSON the
    /// admin builds its forms from, with labels and help in `lang` (see
    /// `Catalogs::resolve`). Built from the schema loaded once per isolate: no parsing.
    /// `None`: no such plugin.
    #[must_use]
    pub fn declarations(&self, plugin: Option<&str>, lang: &str) -> Option<Result<String, String>> {
        let (name, version, schema, catalogs) = match plugin {
            None => (
                self.shared.name.as_str(),
                self.shared.version.as_deref(),
                &self.shared.schema,
                &self.shared.catalogs,
            ),
            Some(n) => {
                let p = self.shared.plugins.iter().find(|p| p.name == n)?;
                (
                    p.name.as_str(),
                    Some(p.version.as_str()),
                    &p.schema,
                    &p.catalogs,
                )
            }
        };
        let (lang, catalog) = catalogs.resolve(lang);
        let missing = MissingKeys::default();
        let text = |key: &str| catalog.t_template(key, &[], &missing);
        Some(schema.declarations(name, version, lang, &text))
    }

    /// The inlined stylesheet, for the size budget.
    #[must_use]
    pub fn css(&self) -> &str {
        &self.look.css
    }

    /// Text for `key` in `lang` (theme locale, then English, then the key itself). Rust keys
    /// are checked against the default theme, which every theme's catalog includes.
    #[must_use]
    pub fn text(&self, lang: &str, key: Key, args: &[(&str, String)]) -> String {
        self.shared.catalogs.get(lang).t(key, args)
    }

    /// Renders one reader page.
    ///
    /// # Errors
    /// A template error, with template name and line.
    pub fn render(
        &self,
        site: &Site,
        page: &PageInfo,
        body: &Body<'_>,
    ) -> Result<String, ThemeError> {
        self.render_reporting(site, page, body).map(|r| r.html)
    }

    /// Like [`Theme::render`], also reporting what the Worker should log: locale keys that
    /// exist nowhere and plugin partials that failed (the page is served without them).
    ///
    /// # Errors
    /// A template error, with template name and line.
    pub fn render_reporting(
        &self,
        site: &Site,
        page: &PageInfo,
        body: &Body<'_>,
    ) -> Result<Report, ThemeError> {
        self.render_reporting_with(site, page, body, &[])
    }

    /// [`Theme::render_reporting`] for a home, tag or series page that has language versions:
    /// `home_alternates` (site-relative hreflang set, this page included) go in its head.
    /// A post page takes its own from its translations and ignores it.
    ///
    /// # Errors
    /// A template error, with template name and line.
    pub fn render_reporting_with(
        &self,
        site: &Site,
        page: &PageInfo,
        body: &Body<'_>,
        home_alternates: &[Alternate],
    ) -> Result<Report, ThemeError> {
        let lang = page.lang.as_str();
        let (kind, description, data) = self.body_data(site, body);
        let mut preload: Vec<&str> = self.shared.preload.iter().map(String::as_str).collect();
        if let Some(extra) = self.shared.preload_by_lang.get(lang) {
            preload.extend(extra.iter().map(String::as_str));
        }
        let list_title = self.list_title(lang, body);
        let seo = seo::build(
            site,
            page,
            body,
            list_title.as_deref(),
            &description,
            home_alternates,
        );
        let ctx = Context {
            site,
            page: PageData {
                kind: kind.as_str(),
                lang,
                dir: if is_rtl(lang) { "rtl" } else { "ltr" },
                description: &description,
                canonical: &page.canonical,
                seo,
            },
            theme: ThemeData {
                css: &self.look.css,
                js: &self.look.js,
                preload,
                colors: &self.look.colors,
                options: &self.look.options,
            },
        };
        self.run(kind, ser(&ctx), data)
    }

    /// Template kind, meta description and template data for one body.
    fn body_data<'b>(&self, site: &Site, body: &Body<'b>) -> (PageKind, String, Value) {
        let entries = |posts: &[&'b Post]| self.entries(posts);
        match body {
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
            Body::Post {
                post,
                prev,
                next,
                comments,
            } => self.post_data(post, *prev, *next, *comments),
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
            Body::Series {
                series,
                description,
                posts,
                prev_url,
                next_url,
            } => {
                let data = SeriesData {
                    series,
                    description,
                    posts: entries(posts),
                    prev_url: prev_url.as_deref(),
                    next_url: next_url.as_deref(),
                };
                // A series' own description is the page's, else the site's.
                let meta = if description.trim().is_empty() {
                    site.description.clone()
                } else {
                    (*description).to_owned()
                };
                (PageKind::Series, meta, ser(data))
            }
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
            Body::Search { query, posts, more } => (
                PageKind::Search,
                site.description.clone(),
                ser(SearchData {
                    query,
                    results: entries(posts),
                    more: *more,
                }),
            ),
        }
    }

    /// The post page's template data and meta description.
    fn post_data(
        &self,
        post: &Post,
        prev: Option<&Post>,
        next: Option<&Post>,
        comments: Option<&Comments>,
    ) -> (PageKind, String, Value) {
        let entry = self.entry(post);
        let description = entry.excerpt.clone();
        let data = PostData {
            entry,
            html: &post.body.html,
            has_code: post.body.has_code,
            has_math: post.body.has_math,
            has_embed: post.body.has_embed,
            tags: &post.tags,
            series: post.series.as_ref(),
            prev: link(prev),
            next: link(next),
        };
        let comments = comments.map(|c| self.comments(&post.lang, c));
        (
            PageKind::Post,
            description,
            ser(PostPage {
                post: data,
                comments,
            }),
        )
    }

    /// Renders `kind`'s template with a fresh missing-key collector for this render only.
    fn run(&self, kind: PageKind, ctx: Value, data: Value) -> Result<Report, ThemeError> {
        let missing = Arc::new(RenderMissing::default());
        let problems = Arc::new(RenderProblems::default());
        let ctx = minijinja::context! {
            ..minijinja::context! {
                __i18n_missing => Value::from_dyn_object(Arc::clone(&missing)),
                __render_problems => Value::from_dyn_object(Arc::clone(&problems)),
                __plugins => ser(&self.look.plugins),
            },
            ..ctx,
            ..data
        };
        let html = self.shared.env.get_template(kind.template())?.render(ctx)?;
        let problems = std::mem::take(
            &mut *problems
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        );
        Ok(Report {
            html,
            missing: missing.0.take(),
            problems,
        })
    }

    /// The visible title of a tag or archive page, for its social and JSON-LD name.
    fn list_title(&self, lang: &str, body: &Body<'_>) -> Option<String> {
        match body {
            Body::Tag { tag, .. } => {
                Some(self.text(lang, keys::TAG_TITLE, &[("name", tag.name.clone())]))
            }
            Body::Series { series, .. } => Some(series.name.clone()),
            Body::Archive { year, .. } => {
                Some(self.text(lang, keys::ARCHIVE_TITLE, &[("year", year.to_string())]))
            }
            _ => None,
        }
    }

    fn entries<'a>(&self, posts: &[&'a Post]) -> Vec<EntryData<'a>> {
        posts.iter().map(|p| self.entry(p)).collect()
    }

    fn date_display(&self, lang: &str, (year, month, day): (i32, u32, u32)) -> String {
        self.text(
            lang,
            keys::DATE_SHORT,
            &[
                ("day", day.to_string()),
                (
                    "month",
                    self.text(lang, keys::MONTHS[month_index(month)], &[]),
                ),
                ("year", year.to_string()),
            ],
        )
    }

    fn comments<'a>(&self, lang: &str, c: &'a Comments) -> CommentsData<'a> {
        CommentsData {
            open: c.open,
            items: c
                .items
                .iter()
                .map(|i| {
                    let (year, month, day) = civil(i.created_at);
                    CommentItemData {
                        id: i.id,
                        author: &i.author,
                        owner: i.owner,
                        date_iso: format!("{year:04}-{month:02}-{day:02}"),
                        date_display: self.date_display(lang, (year, month, day)),
                        html: &i.html,
                    }
                })
                .collect(),
            action_url: &c.action_url,
            turnstile_key: &c.turnstile_key,
            max_length: c.max_length,
            name_max_length: c.name_max_length,
            max_links: c.max_links,
            more: c.more,
        }
    }

    fn entry<'a>(&self, post: &'a Post) -> EntryData<'a> {
        let (year, month, day) = civil(post.published_at);
        let excerpt_words = self
            .look
            .values
            .get("excerpt_words")
            .and_then(Json::as_u64)
            .map_or(55, |n| usize::try_from(n).unwrap_or(0));
        let excerpt = match &post.excerpt {
            Some(e) if !e.trim().is_empty() => e.trim().to_owned(),
            _ => cut_words(&post.body.excerpt, excerpt_words),
        };
        // Computed on save by the markdown crate; reading never counts words.
        let minutes = post.body.reading_minutes;
        let date_display = self.date_display(&post.lang, (year, month, day));
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
    /// `page.seo.title`, `og_type`, `image`, `published`, `alternates`, `json_ld`.
    seo: crate::seo::Seo,
}

#[derive(Serialize)]
struct ThemeData<'a> {
    css: &'a str,
    js: &'a [String],
    preload: Vec<&'a str>,
    colors: &'a Colors,
    options: &'a BTreeMap<String, Json>,
}

/// One render and what the Worker should log about it.
#[derive(Debug)]
pub struct Report {
    pub html: String,
    /// `(lang, key)` asked for and found in no locale, each once (`i18n.missing_key`).
    pub missing: Vec<(String, String)>,
    /// Plugin partials left out of this page, one line each (`plugin.broken ...`).
    pub problems: Vec<String>,
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
    /// Absent (not null) when comments are off, so `{% if comments %}` reads naturally.
    #[serde(skip_serializing_if = "Option::is_none")]
    comments: Option<CommentsData<'a>>,
}

#[derive(Serialize)]
struct TagData<'a> {
    tag: &'a crate::Link,
    posts: Vec<EntryData<'a>>,
    older_url: Option<&'a str>,
    newer_url: Option<&'a str>,
}

#[derive(Serialize)]
struct SeriesData<'a> {
    series: &'a crate::Link,
    description: &'a str,
    posts: Vec<EntryData<'a>>,
    prev_url: Option<&'a str>,
    next_url: Option<&'a str>,
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

#[derive(Serialize)]
struct SearchData<'a> {
    query: &'a str,
    results: Vec<EntryData<'a>>,
    more: bool,
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

/// Minified `faces.css` (font URLs filled in), `style.css` and each offered style's sheet.
fn load_css(
    theme: &dyn ThemeFiles,
    faces: Option<&str>,
    schema: &Schema,
    asset: &dyn Fn(&str) -> String,
) -> Result<(String, String, BTreeMap<String, String>), ThemeError> {
    let read = |path: &str| {
        theme
            .read(path)
            .ok_or_else(|| ThemeError::new(format!("{path} is missing")))
    };
    let faces_css = match faces {
        Some(faces) => {
            let dir = faces.rsplit_once('/').map_or("", |(d, _)| d);
            css::minify(&read(faces)?.replace("{{fonts}}", &asset(dir)))
        }
        None => String::new(),
    };
    let style_css = css::minify(read("style.css")?);
    let mut styles = BTreeMap::new();
    for decl in schema.decls.values() {
        // Drafts are never chosen (a stored draft falls back), so their files may not
        // exist yet.
        for path in decl
            .choices
            .iter()
            .filter(|c| !c.draft)
            .filter_map(|c| c.css.as_deref())
        {
            styles.insert(path.to_owned(), css::minify(read(path)?));
        }
    }

    Ok((faces_css, style_css, styles))
}

/// Filters whose output is marked safe, so a partial could print stored text unescaped
/// (`tojson` is safe for `<`, `>` and `&` but not inside an attribute).
const UNSAFE_FILTERS: &[&str] = &["safe", "tojson"];

/// `t("section.key", name=value)` against `catalogs` in the page language, recording keys
/// that exist nowhere.
fn translate(
    state: &State,
    catalogs: &Catalogs,
    key: &str,
    kwargs: &Kwargs,
) -> Result<String, minijinja::Error> {
    let lang = page_lang(state);
    let args = kwargs_strings(kwargs)?;
    let args: Vec<(&str, String)> = args.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
    let fallback = MissingKeys::default();
    let missing = state.lookup(MISSING_VAR);
    let missing = missing
        .as_ref()
        .and_then(|v| v.downcast_object_ref::<RenderMissing>())
        .map_or(&fallback, |m| &m.0);
    Ok(catalogs.get(&lang).t_template(key, &args, missing))
}

/// Every `locales/<lang>.toml` of `files`, flattened, on top of `default`'s (templates that
/// fall back to the default theme still find their keys).
pub(crate) fn locale_strings(
    files: &dyn ThemeFiles,
    default: Option<&dyn ThemeFiles>,
) -> Result<BTreeMap<String, HashMap<String, String>>, ThemeError> {
    let mut merged: BTreeMap<String, HashMap<String, String>> = BTreeMap::new();
    for files in default.into_iter().chain([files]) {
        for path in files.paths() {
            let Some(lang) = path
                .strip_prefix("locales/")
                .and_then(|f| f.strip_suffix(LOCALE_SUFFIX))
            else {
                continue;
            };
            let strings = plumblog_i18n::parse(files.read(path).unwrap_or_default())
                .map_err(|e| ThemeError::new(format!("{path}: {}", e.detail)))?;
            merged.entry(lang.to_owned()).or_default().extend(strings);
        }
    }
    Ok(merged)
}

/// Catalogs per language, each falling back to English.
pub(crate) fn catalogs(merged: BTreeMap<String, HashMap<String, String>>) -> Catalogs {
    let english_strings = merged.get(FALLBACK_LANG).cloned().unwrap_or_default();
    let by_lang = merged
        .into_iter()
        .map(|(lang, strings)| {
            let fallback = english_strings.clone();
            let catalog = Catalog::new(strings, fallback).with_lang(&lang);
            (lang, catalog)
        })
        .collect();
    Catalogs {
        by_lang,
        english: Catalog::new(english_strings, HashMap::new()),
    }
}

/// Partials get their own environment: same escaping, but without the filters that mark
/// text safe, and without `slot()` (a partial cannot open another slot).
fn partial_env(
    plugins: Vec<Plugin>,
) -> Result<(Environment<'static>, Vec<PluginInfo>), ThemeError> {
    let mut partials = Environment::new();
    partials.set_formatter(html_formatter);
    for f in UNSAFE_FILTERS {
        partials.remove_filter(f);
    }
    let mut plugin_catalogs = HashMap::new();
    let mut infos = Vec::new();
    for p in plugins {
        for (slot, src) in &p.partials {
            partials.add_template_owned(p.template_name(slot), src.clone())?;
        }
        let catalogs = Arc::new(p.catalogs);
        infos.push(PluginInfo {
            slots: p.partials.iter().map(|(s, _)| s.clone()).collect(),
            name: p.name.clone(),
            version: p.version,
            schema: p.schema,
            catalogs: Arc::clone(&catalogs),
            css: p.css,
            js: p.js,
        });
        plugin_catalogs.insert(p.name, catalogs);
    }

    partials.add_function(
        "t",
        move |state: &State, key: &str, kwargs: Kwargs| -> Result<String, minijinja::Error> {
            let name = state.lookup(PLUGIN_VAR);
            match name
                .as_ref()
                .and_then(Value::as_str)
                .and_then(|n| plugin_catalogs.get(n))
            {
                Some(c) => translate(state, c, key, &kwargs),
                None => Err(minijinja::Error::new(
                    minijinja::ErrorKind::InvalidOperation,
                    "t() outside a plugin partial",
                )),
            }
        },
    );
    Ok((partials, infos))
}

/// `t`, `asset` and `slot`, the functions every template may call.
fn add_functions(
    env: &mut Environment<'static>,
    theme_catalogs: Arc<Catalogs>,
    partials: Arc<Environment<'static>>,
    assets_url: &str,
    slots: Vec<String>,
) {
    env.add_function(
        "t",
        move |state: &State, key: &str, kwargs: Kwargs| -> Result<String, minijinja::Error> {
            translate(state, &theme_catalogs, key, &kwargs)
        },
    );
    // Templates link theme files that only some pages need (the code highlighter) by name.
    let assets_base = assets_url.to_owned();
    env.add_function(
        "asset",
        move |path: &str| -> Result<String, minijinja::Error> {
            if path.is_empty()
                || path.starts_with('/')
                || path.contains("..")
                || path.contains("//")
            {
                return Err(minijinja::Error::new(
                    minijinja::ErrorKind::InvalidOperation,
                    format!("asset({path:?}): give a path inside the theme's assets/ folder"),
                ));
            }
            Ok(format!("{assets_base}/{path}"))
        },
    );
    env.add_function(
        "slot",
        move |state: &State, name: &str| -> Result<Value, minijinja::Error> {
            if !slots.iter().any(|s| s == name) {
                let known = if SLOTS.contains(&name) {
                    "add it to `slots` in theme.toml"
                } else {
                    "unknown slot"
                };
                return Err(minijinja::Error::new(
                    minijinja::ErrorKind::InvalidOperation,
                    format!("slot({name:?}): {known}; declared: {}", slots.join(", ")),
                ));
            }
            Ok(Value::from_safe_string(fill_slot(state, &partials, name)))
        },
    );
}

/// Every enabled plugin's partial for `slot`, in plugin-name order. A partial that fails is
/// left out and reported; the page still renders.
fn fill_slot(state: &State, partials: &Environment<'static>, slot: &str) -> String {
    let mut out = String::new();
    let Some(plugins) = state.lookup(PLUGINS_VAR) else {
        return out;
    };
    let problems = state.lookup(PROBLEMS_VAR);
    let report = |p: String| {
        if let Some(r) = problems
            .as_ref()
            .and_then(|v| v.downcast_object_ref::<RenderProblems>())
        {
            r.push(p);
        }
    };
    let Ok(iter) = plugins.try_iter() else {
        return out;
    };
    for plugin in iter {
        let name = plugin
            .get_attr("name")
            .ok()
            .and_then(|n| n.as_str().map(str::to_owned))
            .unwrap_or_default();
        let has_slot = plugin
            .get_attr("slots")
            .ok()
            .and_then(|s| s.try_iter().ok())
            .is_some_and(|mut s| s.any(|v| v.as_str() == Some(slot)));
        if !has_slot {
            continue;
        }
        let template = format!("@plugin/{name}/{slot}.html");
        let rendered = partials.get_template(&template).and_then(|t| {
            t.render(minijinja::context! {
                site => state.lookup("site"),
                page => state.lookup("page"),
                plugin => plugin.clone(),
                __plugin => name.clone(),
                __i18n_missing => state.lookup(MISSING_VAR),
            })
        });
        match rendered {
            Ok(html) => out.push_str(&html),
            Err(e) => report(format!(
                "plugin.broken name={name} slot={slot} detail={e:#}"
            )),
        }
    }
    out
}

/// Applies stored values: options, color tokens, the chosen style, enabled plugins.
fn look(
    shared: &Shared,
    stored: &[(String, String)],
    plugins: &[(String, Vec<(String, String)>)],
) -> (Look, Vec<String>) {
    let owner = format!("theme:{}", shared.name);
    let (values, mut problems) = shared.schema.resolve(&owner, stored);

    let mut colors = shared.colors.clone();
    let mut styles = Vec::new();
    for (key, decl) in &shared.schema.decls {
        match decl.kind {
            FieldType::Color => {
                let token = decl.token.as_deref().unwrap_or(key);
                for (mode, map) in [("light", &mut colors.light), ("dark", &mut colors.dark)] {
                    if let Some(c) = values[key].get(mode).and_then(Json::as_str) {
                        map.insert(token.to_owned(), c.to_owned());
                    }
                }
            }
            FieldType::Choice => {
                let picked = values[key].as_str().unwrap_or_default();
                if let Some(path) = decl
                    .choices
                    .iter()
                    .find(|c| c.value == picked)
                    .and_then(|c| c.css.as_deref())
                {
                    styles.push(path);
                }
            }
            _ => {}
        }
    }

    let mut active = Vec::new();
    let mut plugin_css = String::new();
    let mut js = shared.js.clone();
    for (name, stored) in plugins {
        let Some(p) = shared.plugins.iter().find(|p| &p.name == name) else {
            problems.push(format!(
                "plugin.unknown name={name}: enabled but not bundled"
            ));
            continue;
        };
        for slot in &p.slots {
            if !shared.slots.contains(slot) {
                problems.push(format!(
                    "plugin.slot_missing name={name} slot={slot}: theme {} has no such slot",
                    shared.name
                ));
            }
        }
        let (values, more) = p.schema.resolve(&format!("plugin:{name}"), stored);
        problems.extend(more);
        active.push(ActivePlugin {
            name: name.clone(),
            options: options::template_values(&p.schema, &values),
            slots: p.slots.clone(),
        });
        plugin_css.push_str(&p.css);
        js.extend(p.js.iter().cloned());
    }
    // Plugin-name order, whatever order the store listed them in.
    active.sort_by(|a, b| a.name.cmp(&b.name));

    // Same order as ever: fonts, one block of tokens (`light-dark()` lets `color-scheme`
    // switch the palette; docs/ARCHITECTURE.md, renderer contract), the theme, its style,
    // then plugins.
    let mut css = shared.faces_css.clone();
    css.push_str(":root{");
    for (k, light) in &colors.light {
        let dark = &colors.dark[k];
        // Writing to a String cannot fail.
        let _ = write!(css, "--c-{k}:light-dark({light},{dark});");
    }
    css.push('}');
    css.push_str(&shared.style_css);
    for path in styles {
        css.push_str(&shared.styles[path]);
    }
    css.push_str(&plugin_css);

    (
        Look {
            css,
            colors,
            options: options::template_values(&shared.schema, &values),
            values,
            js,
            plugins: active,
        },
        problems,
    )
}
