//! Reader pages: home (paged), post, tag, yearly archive and 404, rendered by the theme and
//! kept in the Cache API under a key that includes the site content version.

mod cache;
mod routes;
pub mod source;

use crate::keys;
use plumblog_i18n::{Catalog, Key};
use plumblog_model::VERSION;
use plumblog_render::{Body, Link, PageInfo, Post, Site, Theme, ThemeError};
use routes::Route;
use source::{Filter, ReaderSource, SampleSource, Settings, StoredPost};
use std::collections::BTreeMap;
use std::sync::OnceLock;
use worker::{Context, Headers, Request, Response, Result, console_error, console_warn};

/// Where the default theme's `assets/` folder is served (Workers static assets).
pub const THEME_ASSETS_URL: &str = "/theme/minimal";

/// Posts suggested on the 404 page.
const NOT_FOUND_SUGGESTIONS: usize = 5;

/// What a request ends as, before it becomes a `Response`.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Page { status: u16, html: String },
    Redirect(String),
}

/// Theme loaded once per isolate: parsing theme.toml, locales and templates costs more than
/// rendering a page, so it must not happen per request.
fn theme() -> std::result::Result<&'static Theme, &'static ThemeError> {
    static THEME: OnceLock<std::result::Result<Theme, ThemeError>> = OnceLock::new();
    THEME
        .get_or_init(|| {
            Theme::load(
                &source::THEME_FILES,
                None,
                &BTreeMap::new(),
                THEME_ASSETS_URL,
            )
        })
        .as_ref()
}

/// Serves a reader page, from cache when possible.
pub async fn serve(req: &Request, ctx: &Context) -> Result<Response> {
    let url = req.url()?;
    let path = url.path().to_owned();
    let origin = url.origin().ascii_serialization();
    let src = SampleSource::get();

    let version = match src.content_version().await {
        Ok(v) => v,
        Err(e) => return Ok(failure(&format!("content version: {e}"))),
    };
    // Same content version and engine build means the same bytes, so browsers may revalidate.
    let etag = format!("\"{version}-{VERSION}\"");
    if req.headers().get("if-none-match")?.as_deref() == Some(etag.as_str()) {
        let h = Headers::new();
        h.set("etag", &etag)?;
        return Ok(Response::empty()?.with_status(304).with_headers(h));
    }

    let key = cache::key(&origin, version, &path);
    let lookup = cache::get(&key).await;
    if let cache::Lookup::Hit(resp) = lookup {
        return Ok(resp);
    }

    let started = worker::Date::now().as_millis();
    let outcome = match render(src, &origin, &path).await {
        Ok(o) => o,
        Err(detail) => return Ok(failure(&detail)),
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
        Outcome::Page { status, html } => {
            let h = Headers::new();
            h.set("content-type", "text/html; charset=utf-8")?;
            // Server-Timing lets local runs and DevTools see render cost; on Cloudflare the
            // clock stands still while code runs, so it reads 0 there.
            h.set("server-timing", &format!("render;dur={render_ms}"))?;
            if status == 200 {
                h.set("etag", &etag)?;
                // max-age=0: browsers revalidate (cheap 304); s-maxage: the edge copy lives
                // until the content version changes the key.
                h.set(
                    "cache-control",
                    "public, max-age=0, must-revalidate, s-maxage=31536000",
                )?;
            } else {
                // 404s are not stored: random URLs must not fill the cache.
                h.set("cache-control", "no-store")?;
            }
            h.set("x-plumblog-cache", lookup.label())?;
            let mut resp = Response::from_html(html)?
                .with_status(status)
                .with_headers(h);
            if status == 200 && matches!(lookup, cache::Lookup::Miss) {
                cache::put_later(ctx, key, resp.cloned()?);
            }
            Ok(resp)
        }
    }
}

/// Plain-text 500 from the core locale, for when the theme or the data cannot produce a page.
fn failure(detail: &str) -> Response {
    console_error!("pages: render failed: {detail}");
    let text = core_text("en", keys::PAGE_FAILED);
    Response::error(text, 500).unwrap_or_else(|_| Response::empty().expect("empty response"))
}

fn core_text(lang: &str, key: Key) -> String {
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
pub async fn render(
    src: &impl ReaderSource,
    origin: &str,
    path: &str,
) -> std::result::Result<Outcome, String> {
    let theme = theme().map_err(|e| format!("theme: {e}"))?;
    let settings = src.settings().await.map_err(err)?;
    let p = Pages {
        src,
        theme,
        site: site(&settings),
        info: PageInfo {
            lang: settings.lang.clone(),
            canonical: format!("{origin}{path}"),
        },
        per: settings.per_page.max(1),
    };
    let page = match routes::parse(path) {
        Route::Redirect(to) => return Ok(Outcome::Redirect(to)),
        Route::Home { page } => p.home(page).await?,
        Route::Post { slug } => p.post(&slug).await?,
        Route::Tag { slug, page } => p.tag(&slug, page).await?,
        Route::Archive { year, page } => p.archive(year, page).await?,
        Route::NotFound => None,
    };
    let (status, html) = match page {
        Some(html) => (200, html),
        None => (404, p.not_found().await?),
    };
    Ok(Outcome::Page { status, html })
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// One request's rendering context. Each page method returns `None` for "no such page".
struct Pages<'a, S> {
    src: &'a S,
    theme: &'a Theme,
    site: Site,
    info: PageInfo,
    per: usize,
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
        let (html, missing) = self
            .theme
            .render_reporting(&self.site, &self.info, body)
            .map_err(err)?;
        for (lang, key) in missing {
            console_warn!("i18n.missing_key lang={lang} key={key}");
        }
        Ok(html)
    }

    async fn home(&self, n: usize) -> PageResult {
        let slice = self
            .src
            .posts(Filter::All, (n - 1) * self.per, self.per)
            .await
            .map_err(err)?;
        if slice.posts.is_empty() && n > 1 {
            return Ok(None);
        }
        let posts: Vec<Post> = slice.posts.iter().map(to_render).collect();
        let refs: Vec<&Post> = posts.iter().collect();
        self.show(&Body::Home {
            posts: &refs,
            older_url: slice.more.then(|| routes::home_page(n + 1)),
            newer_url: (n > 1).then(|| routes::home_page(n - 1)),
        })
    }

    async fn post(&self, slug: &str) -> PageResult {
        let Some(n) = self.src.post(slug).await.map_err(err)? else {
            return Ok(None);
        };
        let post = to_render(&n.post);
        let older = n.older.as_ref().map(to_render);
        let newer = n.newer.as_ref().map(to_render);
        self.show(&Body::Post {
            post: &post,
            prev: older.as_ref(),
            next: newer.as_ref(),
        })
    }

    async fn tag(&self, slug: &str, n: usize) -> PageResult {
        let Some(tag) = self.src.tag(slug).await.map_err(err)? else {
            return Ok(None);
        };
        let slice = self
            .src
            .posts(Filter::Tag(slug), (n - 1) * self.per, self.per)
            .await
            .map_err(err)?;
        if slice.posts.is_empty() {
            return Ok(None);
        }
        let posts: Vec<Post> = slice.posts.iter().map(to_render).collect();
        let refs: Vec<&Post> = posts.iter().collect();
        let link = Link {
            name: tag.name,
            url: routes::tag_page(&tag.slug, 1),
        };
        self.show(&Body::Tag {
            tag: &link,
            posts: &refs,
            older_url: slice.more.then(|| routes::tag_page(slug, n + 1)),
            newer_url: (n > 1).then(|| routes::tag_page(slug, n - 1)),
        })
    }

    async fn archive(&self, year: i32, n: usize) -> PageResult {
        // Paged like home: storage work per request stays bounded however busy a year was.
        let slice = self
            .src
            .posts(Filter::Year(year), (n - 1) * self.per, self.per)
            .await
            .map_err(err)?;
        if slice.posts.is_empty() {
            return Ok(None);
        }
        let posts: Vec<Post> = slice.posts.iter().map(to_render).collect();
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
            older_url: slice.more.then(|| routes::archive(year, n + 1)),
            newer_url: (n > 1).then(|| routes::archive(year, n - 1)),
        })
    }

    async fn not_found(&self) -> std::result::Result<String, String> {
        let recent = self
            .src
            .posts(Filter::All, 0, NOT_FOUND_SUGGESTIONS)
            .await
            .map_err(err)?;
        let posts: Vec<Post> = recent.posts.iter().map(to_render).collect();
        let refs: Vec<&Post> = posts.iter().collect();
        self.render(&Body::NotFound { recent: &refs })
    }
}

fn site(s: &Settings) -> Site {
    Site {
        title: s.title.clone(),
        description: s.description.clone(),
        home_url: "/".to_owned(),
        // Search and feeds land with their own issues (#16, #19).
        search_url: None,
        feed_url: "/feed.xml".to_owned(),
        // No site icon yet (issue: installable web apps); `data:,` stops the browser from
        // requesting /favicon.ico for nothing.
        icon_url: "data:,".to_owned(),
        footer: s.footer.clone(),
        nav: Vec::new(),
    }
}

fn to_render(p: &StoredPost) -> Post {
    Post {
        url: routes::post(&p.slug),
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
        cover: None,
        thumb: None,
    }
}

#[cfg(test)]
mod tests;
