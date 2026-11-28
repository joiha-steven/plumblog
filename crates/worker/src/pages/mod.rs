//! Reader pages: home (paged), post, tag, yearly archive and 404, rendered by the theme and
//! kept in the Cache API under a key that includes the site content version.

mod cache;
mod cover;
mod db;
mod docs;
pub(crate) mod reader_db;
pub(crate) mod routes;
#[cfg(any(test, feature = "contract-tests"))]
pub mod sample;
mod search;
pub mod source;

use crate::d1::D1Store;
use crate::keys;
use db::{Db, ExtSource};
use plumblog_i18n::{Catalog, Key};
use plumblog_model::VERSION;
use plumblog_render::{Body, Link, PageInfo, Post, Site, Theme, ThemeError};
use plumblog_store::{PostKind, PostStatus, PostStore, READER_MAX_OFFSET, SettingsStore, comment};
use routes::Route;
use search::SearchSource;
use source::{Filter, ReaderSource, Settings, StoredPost};
use std::cell::Cell;
use worker::{Context, Env, Headers, Request, Response, Result, console_error};

/// Where the default theme's `assets/` folder is served (Workers static assets).
pub const THEME_ASSETS_URL: &str = "/theme/minimal";
/// Where bundled plugins' `assets/` folders are served: `/plugin/<name>/...`.
pub const PLUGIN_ASSETS_URL: &str = "/plugin";

/// Posts suggested on the 404 page.
const NOT_FOUND_SUGGESTIONS: usize = 5;

/// What a request ends as, before it becomes a `Response`.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Page {
        status: u16,
        content_type: &'static str,
        body: String,
        /// False when part of the page fell back after a failed store read: serve it, but
        /// never let a cache keep it (see `cache_policy`).
        cacheable: bool,
    },
    Redirect(String),
}

/// Why a page could not be rendered: the theme's templates failed (a pushed theme may then
/// fall back to the default), or anything else (storage, the theme failing to load), which
/// takes the plain error path.
#[derive(Debug, PartialEq, Eq)]
pub enum RenderError {
    Theme(String),
    Other(String),
}

impl RenderError {
    #[must_use]
    pub fn detail(&self) -> &str {
        match self {
            Self::Theme(d) | Self::Other(d) => d,
        }
    }
}

/// The theme pages render with (pushed or embedded default; see `crate::theme_runtime`).
fn theme() -> std::result::Result<&'static Theme, &'static ThemeError> {
    crate::theme_runtime::current()
}

/// The theme with this site's stored options and enabled plugins, and whether both were
/// read. Reading them may fail (D1 hiccup): the page then uses defaults rather than failing,
/// the log says why, and the caller must not let it be cached.
async fn configured(
    base: &Theme,
    ext: &impl ExtSource,
) -> std::result::Result<(Theme, bool), String> {
    let mut complete = true;
    let stored = match ext.theme_options(base.name()).await {
        Ok(v) => v,
        Err(e) => {
            warn(&format!(
                "option.read_failed owner=theme:{} detail={e}",
                base.name()
            ));
            complete = false;
            Vec::new()
        }
    };
    // No bundled plugin: no query.
    let plugins = if base.plugin_names().is_empty() {
        Vec::new()
    } else {
        match ext.enabled_plugins().await {
            Ok(v) => v,
            Err(e) => {
                warn(&format!("plugin.read_failed detail={e}"));
                complete = false;
                Vec::new()
            }
        }
    };
    let (theme, problems) = base.with_options(&stored, &plugins);
    for p in problems {
        warn(&p);
    }
    Ok((theme, complete))
}

/// `(etag and edge copy allowed, cache-control)` for a rendered page.
fn cache_policy(status: u16, cacheable: bool) -> (bool, &'static str) {
    if status == 200 && cacheable {
        // max-age=0: browsers revalidate (cheap 304); s-maxage: the edge copy lives until
        // the content version changes the key.
        (
            true,
            "public, max-age=0, must-revalidate, s-maxage=31536000",
        )
    } else {
        // 404s: random URLs must not fill the cache. Not cacheable: a page built after a
        // failed store read would otherwise stay wrong until the next content write.
        (false, "no-store")
    }
}

/// Serves a reader page, from cache when possible.
pub async fn serve(req: &Request, env: &Env, ctx: &Context) -> Result<Response> {
    let url = req.url()?;
    let path = url.path().to_owned();
    let origin = url.origin().ascii_serialization();
    let d1 = match env.d1("DB") {
        Ok(d) => D1Store::new(d),
        Err(e) => return Ok(failure(&format!("D1 binding DB: {e}"))),
    };
    let db = Db(&d1);
    let src = &db;
    // Search results depend on the query string and on the store, neither of which is in
    // the cache key or the ETag, so they skip both.
    if path == routes::SEARCH {
        let query = url
            .query_pairs()
            .find(|(k, _)| k == "q")
            .map(|(_, v)| v.into_owned())
            .unwrap_or_default();
        return live(src, &db, &path, &query).await;
    }
    // From the store: option and plugin changes bump it too, so they reach cached pages.
    let version = match db.content_version().await {
        Ok(v) => v,
        Err(e) => return Ok(failure(&format!("content version: {e}"))),
    };
    // Same content version and engine build means the same bytes, so browsers may revalidate.
    let etag = format!(
        "\"{version}{}-{VERSION}\"",
        crate::theme_runtime::cache_tag()
    );
    if req.headers().get("if-none-match")?.as_deref() == Some(etag.as_str()) {
        let h = Headers::new();
        h.set("etag", &etag)?;
        return Ok(Response::empty()?.with_status(304).with_headers(h));
    }

    let key = cache::key_for(env, &origin, version, &path);
    let lookup = cache::get(&key).await;
    if let cache::Lookup::Hit(resp) = lookup {
        return Ok(resp);
    }

    let started = worker::Date::now().as_millis();
    let (outcome, tag) = match render_or_default(src, &db, &path, "").await {
        Ok(o) => o,
        Err(e) => return Ok(failure(e.detail())),
    };
    let render_ms = worker::Date::now().as_millis().saturating_sub(started);

    match outcome {
        // `to` comes from routes:: builders (a `/`-rooted path without `//`); it is appended
        // to our own origin, never resolved against the request, so it cannot leave the site.
        Outcome::Redirect(to) => {
            if !to.starts_with('/') || to.starts_with("//") {
                return Ok(failure(&format!("refused redirect target {to:?}")));
            }
            Response::redirect_with_status(worker::Url::parse(&format!("{origin}{to}"))?, 301)
        }
        Outcome::Page {
            status,
            content_type,
            body,
            cacheable,
        } => {
            let h = Headers::new();
            h.set("content-type", content_type)?;
            // Server-Timing lets local runs and DevTools see render cost; on Cloudflare the
            // clock stands still while code runs, so it reads 0 there.
            h.set("server-timing", &format!("render;dur={render_ms}"))?;
            let (store, cache_control) = cache_policy(status, cacheable);
            if store {
                h.set("etag", &etag)?;
            }
            h.set("cache-control", cache_control)?;
            let label = if cacheable {
                lookup.label()
            } else {
                cache::Lookup::Bypass.label()
            };
            h.set("x-plumblog-cache", label)?;
            if let Some(t) = tag {
                h.set(plumblog_render::pushed::THEME_HEADER, t)?;
            }
            let stored = if store && matches!(lookup, cache::Lookup::Miss) {
                stored_copy(
                    req,
                    env,
                    version,
                    &path,
                    &body,
                    content_type,
                    &etag,
                    cache_control,
                    tag,
                )?
            } else {
                None
            };
            let mut resp = Response::ok(body)?.with_status(status).with_headers(h);
            if store && matches!(lookup, cache::Lookup::Miss) {
                cache::put_later(ctx, env, key, resp.cloned()?, stored);
            }
            Ok(resp)
        }
    }
}

/// The R2 copy of a page the Cache API is about to keep, or `None` when the guards
/// ([`cache::may_store`]) or a missing version id keep it local.
#[expect(clippy::too_many_arguments, reason = "the parts of one rendered page")]
fn stored_copy(
    req: &Request,
    env: &Env,
    version: u64,
    path: &str,
    body: &str,
    content_type: &'static str,
    etag: &str,
    cache_control: &'static str,
    theme: Option<&'static str>,
) -> Result<Option<cache::Stored>> {
    let Some(id) = crate::version_id(env) else {
        return Ok(None);
    };
    let headers = req.headers();
    let credentials = headers.has("cookie")? || headers.has("authorization")?;
    let key = cache::store_key(&id, version, crate::theme_runtime::cache_tag(), path);
    let get = req.method() == worker::Method::Get;
    if !cache::may_store(get, credentials, body.len(), &key, cache::store_limit(env)) {
        return Ok(None);
    }
    Ok(Some(cache::Stored {
        key,
        body: body.as_bytes().to_vec(),
        content_type,
        etag: etag.to_owned(),
        cache_control,
        theme,
        path: path.to_owned(),
        cf_ray: headers.get("cf-ray")?,
    }))
}

/// A page rendered for this request only: no `ETag`, no Cache API, never stored by browsers.
async fn live<S: ExtSource + SearchSource>(
    src: &impl ReaderSource,
    db: &S,
    path: &str,
    query: &str,
) -> Result<Response> {
    let (outcome, tag) = match render_or_default(src, db, path, query).await {
        Ok(o) => o,
        Err(e) => return Ok(failure(e.detail())),
    };
    let Outcome::Page {
        status,
        content_type,
        body,
        ..
    } = outcome
    else {
        return Ok(failure("live page asked for a redirect"));
    };
    let h = Headers::new();
    h.set("content-type", content_type)?;
    h.set("cache-control", "no-store")?;
    h.set("x-plumblog-cache", cache::Lookup::Bypass.label())?;
    if let Some(t) = tag {
        h.set(plumblog_render::pushed::THEME_HEADER, t)?;
    }
    Ok(Response::ok(body)?.with_status(status).with_headers(h))
}

/// [`render`], and when a pushed theme's templates fail, [`render_with`] the embedded
/// default (`crate::theme_runtime::fallback`), never cached; any other error is returned as
/// it is. `.1` is the `x-plumblog-theme` value: the pushed theme's tag when it rendered the
/// page, else `None`.
async fn render_or_default<S: ExtSource + SearchSource>(
    src: &impl ReaderSource,
    db: &S,
    path: &str,
    query: &str,
) -> std::result::Result<(Outcome, Option<&'static str>), RenderError> {
    let detail = match render(src, db, db, path, query).await {
        Ok(o) => return Ok((o, crate::theme_runtime::served_tag())),
        Err(RenderError::Theme(d)) => d,
        Err(e) => return Err(e),
    };
    let Some(base) = crate::theme_runtime::fallback(path, &detail) else {
        return Err(RenderError::Theme(detail));
    };
    let mut out = render_with(base, src, db, db, path, query).await?;
    if let Outcome::Page { cacheable, .. } = &mut out {
        *cacheable = false;
    }
    Ok((out, None))
}

/// Plain-text 500 from the core locale, for when the theme or the data cannot produce a page.
fn failure(detail: &str) -> Response {
    console_error!("pages: render failed: {detail}");
    let text = core_text("en", keys::PAGE_FAILED);
    Response::error(text, 500).unwrap_or_else(|_| Response::empty().expect("empty response"))
}

pub(crate) fn core_text(lang: &str, key: Key) -> String {
    let strings = |l: &str| {
        source::CORE_LOCALES
            .iter()
            .find(|(name, _)| *name == l)
            .and_then(|(_, src)| plumblog_i18n::parse(src).ok())
            .unwrap_or_default()
    };
    Catalog::new(strings(lang), strings("en"))
        .with_lang(lang)
        .t(key, &[])
}

/// Routes and renders without touching the runtime, so every route is testable on the host.
///
/// # Errors
/// A storage or template failure, as a detail for the log.
/// Takes no request origin on purpose: the Host header is the client's to choose, so it must
/// never reach a public URL (canonical, feed and entry ids, sitemap).
///
/// `ext` gives the theme its stored options and post pages their comments; `query` is the
/// `q` parameter, read only by the search route, which `search` serves. The comment form's
/// `?comment=` result is shown by theme JS, so it never changes the page or its cache key.
pub async fn render(
    src: &impl ReaderSource,
    ext: &impl ExtSource,
    search: &impl SearchSource,
    path: &str,
    query: &str,
) -> std::result::Result<Outcome, RenderError> {
    let base = theme().map_err(|e| RenderError::Other(format!("theme: {e}")))?;
    render_with(base, src, ext, search, path, query).await
}

/// [`render`] with `base` instead of the active theme.
///
/// # Errors
/// A storage or template failure, as a detail for the log.
pub async fn render_with(
    base: &Theme,
    src: &impl ReaderSource,
    ext: &impl ExtSource,
    search: &impl SearchSource,
    path: &str,
    query: &str,
) -> std::result::Result<Outcome, RenderError> {
    let (theme, complete) = configured(base, ext).await.map_err(RenderError::Other)?;
    let failed = Cell::new(false);
    let mut out = route(src, theme, ext, search, path, query, &failed)
        .await
        .map_err(|d| {
            if failed.get() {
                RenderError::Theme(d)
            } else {
                RenderError::Other(d)
            }
        })?;
    if let Outcome::Page { cacheable, .. } = &mut out {
        *cacheable &= complete;
    }
    Ok(out)
}

/// Store reads for pages, over D1, for routes outside this module (admin, `crate::contract`).
pub(crate) fn db_source(store: &D1Store) -> Db<'_, D1Store> {
    Db(store)
}

/// The page a draft would publish as, rendered with the site's theme and options: the admin
/// preview. `now` is the date it shows, Unix seconds. The real route lives with the posts API (#10); test builds serve it as
/// `/__preview` (`crate::contract`) so the admin is built against the real renderer.
///
/// # Errors
/// A storage or template failure, as a detail for the log.
#[cfg_attr(not(feature = "contract-tests"), allow(dead_code))]
pub async fn preview(
    src: &impl ReaderSource,
    ext: &impl ExtSource,
    title: &str,
    markdown: &str,
    lang: &str,
    now: i64,
) -> std::result::Result<String, String> {
    let base = theme().map_err(|e| format!("theme: {e}"))?;
    let (theme, _) = configured(base, ext).await?;
    let settings = src.settings().await.map_err(err)?;
    let failed = Cell::new(false);
    let origin = public_origin(&settings);
    let p = Pages {
        src,
        theme,
        site: site(&settings, &settings.lang),
        info: PageInfo {
            lang: settings.lang.clone(),
            canonical: format!("{origin}/"),
            origin: origin.to_owned(),
        },
        per: settings.per_page.max(1),
        settings: &settings,
        complete: Cell::new(true),
        theme_failed: &failed,
    };
    let post = Post {
        url: "/".to_owned(),
        title: title.to_owned(),
        lang: if lang.is_empty() {
            settings.lang.clone()
        } else {
            lang.to_owned()
        },
        published_at: now,
        pinned: false,
        excerpt: None,
        body: plumblog_markdown::render(markdown, plumblog_markdown::Options::default()),
        tags: Vec::new(),
        cover: None,
        thumb: None,
        translations: Vec::new(),
    };
    // A draft has no comments yet; the preview shows the page as it publishes.
    p.render(&Body::Post {
        post: &post,
        prev: None,
        next: None,
        comments: None,
    })
    .map(|html| without_scripts(&html))
}

/// The page with its `<script>` elements removed. The preview frame runs no scripts (it shares
/// the admin's origin so theme fonts load), so they would only fill the console with blocked
/// script errors; the page is complete without them.
#[cfg_attr(not(feature = "contract-tests"), allow(dead_code))]
fn without_scripts(html: &str) -> String {
    // ASCII lowercasing keeps byte offsets, so indexes found in `lower` apply to `html`.
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len());
    let mut at = 0;
    while let Some(i) = lower[at..].find("<script").map(|i| at + i) {
        out.push_str(&html[at..i]);
        at = lower[i..]
            .find("</script>")
            .map_or(html.len(), |j| i + j + "</script>".len());
    }
    out.push_str(&html[at..]);
    out
}

async fn route(
    src: &impl ReaderSource,
    theme: Theme,
    ext: &impl ExtSource,
    search: &impl SearchSource,
    path: &str,
    query: &str,
    failed: &Cell<bool>,
) -> std::result::Result<Outcome, String> {
    let settings = src.settings().await.map_err(err)?;
    let origin = public_origin(&settings);
    let p = Pages {
        src,
        theme,
        site: site(&settings, &settings.lang),
        info: PageInfo {
            lang: settings.lang.clone(),
            canonical: format!("{origin}{path}"),
            origin: origin.to_owned(),
        },
        per: settings.per_page.max(1),
        settings: &settings,
        complete: Cell::new(true),
        theme_failed: failed,
    };
    let page = match routes::parse(path) {
        Route::Redirect(to) => return Ok(Outcome::Redirect(to)),
        Route::Home { page } => p.home(page).await?,
        // The default language lives at the root: its `/<lang>/` form has one URL, the root.
        Route::Post {
            lang: Some(l),
            slug,
        } if l == settings.lang => return Ok(Outcome::Redirect(routes::post(&slug))),
        Route::Post { lang, slug } => {
            p.post(lang.as_deref().unwrap_or(&settings.lang), &slug, ext)
                .await?
        }
        Route::Tag { slug, page } => p.tag(&slug, page).await?,
        Route::Archive { year, page } => p.archive(year, page).await?,
        Route::Search => p.search(search, query).await?,
        Route::Feed { lang, format } => {
            return Ok(match p.feed(&settings, lang.as_deref(), format).await? {
                Some(body) => doc(format.content_type(), body),
                None => p.not_found_page().await?,
            });
        }
        Route::Sitemap => return Ok(doc(docs::XML, p.sitemap(&settings).await?)),
        Route::Robots => return Ok(doc(docs::TEXT, docs::robots(origin))),
        Route::Llms => return Ok(doc(docs::TEXT, p.llms(&settings).await?)),
        Route::NotFound => None,
    };
    let mut out = match page {
        Some(html) => doc(HTML, html),
        None => p.not_found_page().await?,
    };
    if let Outcome::Page { cacheable, .. } = &mut out {
        *cacheable &= p.complete.get();
    }
    Ok(out)
}

const HTML: &str = "text/html; charset=utf-8";

fn doc(content_type: &'static str, body: String) -> Outcome {
    Outcome::Page {
        status: 200,
        content_type,
        body,
        cacheable: true,
    }
}

/// Public URLs when the site has no configured origin: fixed, so every host serves the same
/// ids, and plainly not the real site, so the warning below gets noticed.
pub const UNSET_ORIGIN: &str = plumblog_render::UNSET_ORIGIN;

/// A post's public URL for webhook payloads, by the same origin and language rules as reader
/// pages. 1 query.
pub async fn post_url(store: &D1Store, lang: &str, slug: &str) -> String {
    match Db(store).settings().await {
        Ok(s) => format!(
            "{}{}",
            public_origin(&s),
            routes::post_in(lang, &s.lang, slug)
        ),
        Err(e) => {
            warn(&format!(
                "webhook.post_url: settings unreadable ({e}); using {UNSET_ORIGIN}"
            ));
            format!("{UNSET_ORIGIN}/{lang}/{slug}")
        }
    }
}

fn public_origin(settings: &Settings) -> &str {
    match settings.origin.as_deref().map(|o| o.trim_end_matches('/')) {
        Some(o) if !o.is_empty() => o,
        _ => {
            // Worker log, not the system log: this runs per page view (docs/ARCHITECTURE.md,
            // System log). Rendered pages are cached per content version, so it repeats only
            // once per page per version.
            warn(&format!(
                "settings.origin_missing: public URLs use {UNSET_ORIGIN}; set the site origin (blog setup writes it)"
            ));
            UNSET_ORIGIN
        }
    }
}

/// A Worker log warning. `console_warn!` only exists in wasm, so host tests print to stderr.
fn warn(line: &str) {
    #[cfg(target_arch = "wasm32")]
    worker::console_warn!("{line}");
    #[cfg(not(target_arch = "wasm32"))]
    eprintln!("{line}");
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// One request's rendering context. Each page method returns `None` for "no such page".
struct Pages<'a, S> {
    src: &'a S,
    theme: Theme,
    /// Site and page facts in the default language; a post in another language swaps both.
    site: Site,
    info: PageInfo,
    per: usize,
    settings: &'a Settings,
    /// Cleared when part of the page fell back after a failed store read (comments).
    complete: Cell<bool>,
    /// Set when the theme's templates failed, so the error is told apart from a store one.
    theme_failed: &'a Cell<bool>,
}

type PageResult = std::result::Result<Option<String>, String>;

impl<S: ReaderSource> Pages<'_, S> {
    fn show(&self, body: &Body<'_>) -> PageResult {
        self.render(body).map(Some)
    }

    /// Renders and logs locale keys the theme asked for but no locale has. Worker logs, not
    /// the system log: this happens while serving a page view, and the system log never
    /// writes per view (docs/ARCHITECTURE.md, System log).
    fn render(&self, body: &Body<'_>) -> std::result::Result<String, String> {
        let other = match body {
            Body::Post { post, .. } if post.lang != self.info.lang => Some((
                site(self.settings, &post.lang),
                PageInfo {
                    lang: post.lang.clone(),
                    ..self.info.clone()
                },
            )),
            _ => None,
        };
        let (site, info) = other
            .as_ref()
            .map_or((&self.site, &self.info), |(s, i)| (s, i));
        let report = self.theme.render_reporting(site, info, body).map_err(|e| {
            self.theme_failed.set(true);
            err(e)
        })?;
        for (lang, key) in report.missing {
            warn(&format!("i18n.missing_key lang={lang} key={key}"));
        }
        for p in report.problems {
            warn(&p);
        }
        Ok(report.html)
    }

    /// Where page `n` of a listing starts, or `None` when that is past the offset cap: such a
    /// page is a 404 before any query runs.
    fn offset(&self, n: usize) -> Option<usize> {
        let offset = (n - 1).checked_mul(self.per)?;
        (offset < READER_MAX_OFFSET as usize).then_some(offset)
    }

    fn render_posts(&self, posts: &[StoredPost]) -> Vec<Post> {
        posts.iter().map(|p| self.to_render(p)).collect()
    }

    /// A stored post as the theme shows it: URLs follow the language rule (the default
    /// language at the root), and the hreflang alternates are its translations.
    fn to_render(&self, p: &StoredPost) -> Post {
        to_render(p, &self.settings.lang)
    }

    async fn home(&self, n: usize) -> PageResult {
        let Some(offset) = self.offset(n) else {
            return Ok(None);
        };
        let slice = self
            .src
            .posts(Filter::Home(&self.settings.lang), offset, self.per)
            .await
            .map_err(err)?;
        // Decided from the count: a page past the last one is a 404 (page 1 of an empty
        // site is the empty home).
        if n > 1 && offset >= slice.total {
            return Ok(None);
        }
        let posts = self.render_posts(&slice.posts);
        let refs: Vec<&Post> = posts.iter().collect();
        self.show(&Body::Home {
            posts: &refs,
            older_url: (offset + slice.posts.len() < slice.total).then(|| routes::home_page(n + 1)),
            newer_url: (n > 1).then(|| routes::home_page(n - 1)),
        })
    }

    async fn post(&self, lang: &str, slug: &str, ext: &impl ExtSource) -> PageResult {
        let Some(n) = self.src.post(lang, slug).await.map_err(err)? else {
            return Ok(None);
        };
        let post = self.to_render(&n.post);
        let older = n.older.as_ref().map(|p| self.to_render(p));
        let newer = n.newer.as_ref().map(|p| self.to_render(p));
        // Comments are for posts: a static page shows no form (the comment route refuses it),
        // so it reads none. A failed read shows the post without its comments rather than
        // failing the page; the page is then not cached (see `complete`).
        let comments = if n.post.is_page {
            None
        } else {
            match ext.comments(&n.post, &self.settings.lang).await {
                Ok(c) => c,
                Err(e) => {
                    warn(&format!("comments.read_failed slug={slug} detail={e}"));
                    self.complete.set(false);
                    None
                }
            }
        };
        self.show(&Body::Post {
            post: &post,
            prev: older.as_ref(),
            next: newer.as_ref(),
            comments: comments.as_ref(),
        })
    }

    async fn tag(&self, slug: &str, n: usize) -> PageResult {
        let Some(offset) = self.offset(n) else {
            return Ok(None);
        };
        let Some(tag) = self.src.tag(slug).await.map_err(err)? else {
            return Ok(None);
        };
        let slice = self
            .src
            .posts(Filter::Tag(&tag), offset, self.per)
            .await
            .map_err(err)?;
        if slice.posts.is_empty() {
            return Ok(None);
        }
        let posts = self.render_posts(&slice.posts);
        let refs: Vec<&Post> = posts.iter().collect();
        let link = Link {
            name: tag.name,
            url: routes::tag_page(&tag.slug, 1),
        };
        self.show(&Body::Tag {
            tag: &link,
            posts: &refs,
            older_url: (offset + slice.posts.len() < slice.total)
                .then(|| routes::tag_page(slug, n + 1)),
            newer_url: (n > 1).then(|| routes::tag_page(slug, n - 1)),
        })
    }

    async fn archive(&self, year: i32, n: usize) -> PageResult {
        let Some(offset) = self.offset(n) else {
            return Ok(None);
        };
        // Paged like home: storage work per request stays bounded however busy a year was.
        let slice = self
            .src
            .posts(Filter::Year(year), offset, self.per)
            .await
            .map_err(err)?;
        if slice.posts.is_empty() {
            return Ok(None);
        }
        let posts = self.render_posts(&slice.posts);
        let refs: Vec<&Post> = posts.iter().collect();
        let years = self
            .src
            .years()
            .await
            .map_err(err)?
            .into_iter()
            .map(|y| Link {
                name: y.to_string(),
                url: routes::archive(y, 1),
            })
            .collect();
        self.show(&Body::Archive {
            year,
            posts: &refs,
            years,
            older_url: (offset + slice.posts.len() < slice.total)
                .then(|| routes::archive(year, n + 1)),
            newer_url: (n > 1).then(|| routes::archive(year, n - 1)),
        })
    }

    async fn not_found(&self) -> std::result::Result<String, String> {
        let recent = self
            .src
            .posts(Filter::Home(&self.settings.lang), 0, NOT_FOUND_SUGGESTIONS)
            .await
            .map_err(err)?;
        let posts = self.render_posts(&recent.posts);
        let refs: Vec<&Post> = posts.iter().collect();
        self.render(&Body::NotFound { recent: &refs })
    }

    async fn not_found_page(&self) -> std::result::Result<Outcome, String> {
        Ok(Outcome::Page {
            status: 404,
            content_type: HTML,
            body: self.not_found().await?,
            cacheable: true,
        })
    }
}

/// The stored post behind the reader page `/<slug>` (`lang` `None`) or `/<lang>/<slug>` and
/// whether readers may comment on it now, for the comment route. Only what the page shows
/// takes comments: a published post, the default language at the root only. 2 queries.
///
/// # Errors
/// A store read, as a detail for the log.
pub(crate) async fn comment_target<S: PostStore + SettingsStore>(
    store: &S,
    lang: Option<&str>,
    slug: &str,
) -> std::result::Result<Option<(plumblog_model::PostId, bool)>, String> {
    let settings = reader_db::parse_settings(&store.all_settings().await.map_err(err)?);
    if lang == Some(settings.lang.as_str()) {
        return Ok(None);
    }
    let lang = lang.unwrap_or(&settings.lang);
    let Some(post) = store.find_post(lang, slug).await.map_err(err)? else {
        return Ok(None);
    };
    if post.status != PostStatus::Published || post.kind != PostKind::Post {
        return Ok(None);
    }
    let site = comment::site_enabled(store).await.map_err(err)?;
    Ok(Some((post.id, comment::is_open(site, &post))))
}

/// Site facts for a page in `lang`: its feed links are that language's feeds.
fn site(s: &Settings, lang: &str) -> Site {
    let feed_lang = (lang != s.lang).then_some(lang);
    Site {
        title: s.title.clone(),
        description: s.description.clone(),
        default_lang: s.lang.clone(),
        home_url: "/".to_owned(),
        search_url: Some(routes::SEARCH.to_owned()),
        feed_url: routes::feed(feed_lang, routes::FeedFormat::Atom),
        rss_url: routes::feed(feed_lang, routes::FeedFormat::Rss),
        json_feed_url: routes::feed(feed_lang, routes::FeedFormat::Json),
        image_url: None,
        author: None,
        // No site icon yet (issue: installable web apps); `data:,` stops the browser from
        // requesting /favicon.ico for nothing.
        icon_url: "data:,".to_owned(),
        footer: s.footer.clone(),
        nav: Vec::new(),
    }
}

fn to_render(p: &StoredPost, default_lang: &str) -> Post {
    Post {
        url: routes::post_in(&p.lang, default_lang, &p.slug),
        title: p.title.clone(),
        lang: p.lang.clone(),
        published_at: p.published_at,
        pinned: p.pinned,
        excerpt: p.excerpt.clone(),
        body: p.body.clone(),
        tags: p
            .tags
            .iter()
            .map(|t| Link {
                name: t.name.clone(),
                url: routes::tag_page(&t.slug, 1),
            })
            .collect(),
        cover: p.cover.clone(),
        thumb: None,
        translations: p
            .translations
            .iter()
            .map(|(lang, slug)| plumblog_render::Alternate {
                lang: lang.clone(),
                url: routes::post_in(lang, default_lang, slug),
            })
            .collect(),
    }
}

#[cfg(test)]
mod docs_tests;
#[cfg(test)]
mod options_tests;
#[cfg(test)]
mod reader_tests;
#[cfg(test)]
mod search_tests;
#[cfg(any(test, feature = "contract-tests"))]
pub mod seed;
#[cfg(test)]
mod tests;
#[cfg(test)]
use tests as tests_support;
