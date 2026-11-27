//! Reader URLs, in one place: parsing a path and building the links pages point to.
//!
//! `/` and `/page/N` home, `/<slug>` post, `/tag/<slug>` and `/archive/<year>` (each with
//! `/page/N`). Redirect targets are always built here as `/`-rooted paths with no `//`, so a
//! request path can never turn into an off-site `Location`.
//! `/search?q=`. `page`, `tag`, `archive`, `search`, `theme`, `admin` and `api` cannot be post
//! slugs.

#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    Home {
        page: usize,
    },
    Post {
        slug: String,
    },
    Tag {
        slug: String,
        page: usize,
    },
    Archive {
        year: i32,
        page: usize,
    },
    /// `/feed.xml` (Atom), `/rss.xml`, `/feed.json`; `/<lang>/...` for other languages.
    Feed {
        lang: Option<String>,
        format: FeedFormat,
    },
    /// `/search?q=`: the query string is read by the caller, not routed.
    Search,
    Sitemap,
    Robots,
    Llms,
    /// One canonical URL per page: `/page/1` and trailing slashes redirect.
    Redirect(String),
    NotFound,
}

pub fn parse(path: &str) -> Route {
    // Empty segments and backslashes are never our URLs, and `//host` would be scheme-relative
    // in a redirect. Encoded slashes are refused too: decoded later they mean the same thing.
    let lower = path.to_ascii_lowercase();
    if !path.starts_with('/')
        || path.contains("//")
        || path.contains('\\')
        || lower.contains("%2f")
        || lower.contains("%5c")
    {
        return Route::NotFound;
    }
    if path != "/" && path.ends_with('/') {
        // One trailing slash only (no `//` above), so the target is a non-empty `/x` path.
        return Route::Redirect(path[..path.len() - 1].to_owned());
    }
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();
    match parts.as_slice() {
        [""] => Route::Home { page: 1 },
        ["page", n] => match page_number(n) {
            Some(1) => Route::Redirect(home_page(1)),
            Some(page) => Route::Home { page },
            None => Route::NotFound,
        },
        ["tag", slug] if is_slug(slug) => Route::Tag {
            slug: (*slug).to_owned(),
            page: 1,
        },
        ["tag", slug, "page", n] if is_slug(slug) => match page_number(n) {
            Some(1) => Route::Redirect(tag_page(slug, 1)),
            Some(page) => Route::Tag {
                slug: (*slug).to_owned(),
                page,
            },
            None => Route::NotFound,
        },
        ["archive", y] => match year(y) {
            Some(year) => Route::Archive { year, page: 1 },
            None => Route::NotFound,
        },
        ["archive", y, "page", n] => match (year(y), page_number(n)) {
            (Some(year), Some(1)) => Route::Redirect(archive(year, 1)),
            (Some(year), Some(page)) => Route::Archive { year, page },
            _ => Route::NotFound,
        },
        ["search"] => Route::Search,
        ["sitemap.xml"] => Route::Sitemap,
        ["robots.txt"] => Route::Robots,
        ["llms.txt"] => Route::Llms,
        [file] if FeedFormat::from_file(file).is_some() => Route::Feed {
            lang: None,
            format: FeedFormat::from_file(file).unwrap_or(FeedFormat::Atom),
        },
        [lang, file] if is_lang(lang) && FeedFormat::from_file(file).is_some() => Route::Feed {
            lang: Some((*lang).to_owned()),
            format: FeedFormat::from_file(file).unwrap_or(FeedFormat::Atom),
        },
        [slug] if is_slug(slug) && !RESERVED.contains(slug) => Route::Post {
            slug: (*slug).to_owned(),
        },
        _ => Route::NotFound,
    }
}

/// The slug of `/<slug>/comments`, where a post's comment form posts (POST only; GET there
/// is a 404 like any unknown path).
pub fn comment_form(path: &str) -> Option<&str> {
    let slug = path.strip_prefix('/')?.strip_suffix("/comments")?;
    (is_slug(slug) && !RESERVED.contains(&slug)).then_some(slug)
}

/// Where a post's comment form posts.
pub fn comments(slug: &str) -> String {
    format!("{}/comments", post(slug))
}

const RESERVED: &[&str] = &["page", "tag", "archive", "search", "theme", "admin", "api"];

/// The search page; results are `?q=` on it.
pub const SEARCH: &str = "/search";

/// Slugs are lowercase ASCII letters, digits and dashes; anything else is a 404 without a
/// storage lookup.
fn is_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeedFormat {
    Atom,
    Rss,
    Json,
}

impl FeedFormat {
    const ALL: [Self; 3] = [Self::Atom, Self::Rss, Self::Json];

    pub const fn file(self) -> &'static str {
        match self {
            Self::Atom => "feed.xml",
            Self::Rss => "rss.xml",
            Self::Json => "feed.json",
        }
    }

    pub const fn content_type(self) -> &'static str {
        match self {
            Self::Atom => "application/atom+xml; charset=utf-8",
            Self::Rss => "application/rss+xml; charset=utf-8",
            Self::Json => "application/feed+json; charset=utf-8",
        }
    }

    fn from_file(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.file() == name)
    }
}

/// `en`, `vi`, `pt-br`, `zh-hant`: BCP 47 language with an optional region or script.
fn is_lang(s: &str) -> bool {
    let mut parts = s.split('-');
    let primary = parts.next().unwrap_or("");
    (2..=3).contains(&primary.len())
        && primary.bytes().all(|b| b.is_ascii_lowercase())
        && parts.all(|p| {
            (2..=8).contains(&p.len())
                && p.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

/// The default language's feeds live at the root; others under `/<lang>/`.
pub fn feed(lang: Option<&str>, format: FeedFormat) -> String {
    match lang {
        Some(l) => format!("/{l}/{}", format.file()),
        None => format!("/{}", format.file()),
    }
}

fn year(s: &str) -> Option<i32> {
    s.parse::<i32>().ok().filter(|y| s.len() == 4 && *y > 0)
}

/// `2`..; leading zeros and absurd numbers are not pages.
fn page_number(s: &str) -> Option<usize> {
    if s.starts_with('0') || s.len() > 6 {
        return None;
    }
    s.parse().ok().filter(|n| *n >= 1)
}

pub fn home_page(n: usize) -> String {
    if n <= 1 {
        "/".to_owned()
    } else {
        format!("/page/{n}")
    }
}

pub fn post(slug: &str) -> String {
    format!("/{slug}")
}

pub fn tag_page(slug: &str, n: usize) -> String {
    if n <= 1 {
        format!("/tag/{slug}")
    } else {
        format!("/tag/{slug}/page/{n}")
    }
}

pub fn archive(year: i32, n: usize) -> String {
    if n <= 1 {
        format!("/archive/{year}")
    } else {
        format!("/archive/{year}/page/{n}")
    }
}
