//! Reader URLs, in one place: parsing a path and building the links pages point to.
//!
//! `/` and `/page/N` home, `/<lang>/` and `/<lang>/page/N` a language's home, `/<slug>` post in the default language, `/<lang>/<slug>` in another, `/tag/<slug>`, `/series/<slug>` (and
//! `/<lang>/tag/<slug>`, `/<lang>/series/<slug>` for another language) and `/archive/<year>` (each with
//! `/page/N`). Redirect targets are always built here as `/`-rooted paths with no `//`, so a
//! request path can never turn into an off-site `Location`.
//! `/search?q=` and `/<lang>/search?q=`. The names in [`RESERVED`] cannot be post slugs.

/// Which listing a tag-or-series route is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Taxon {
    Tag,
    Series,
}

impl Taxon {
    const fn segment(self) -> &'static str {
        match self {
            Self::Tag => "tag",
            Self::Series => "series",
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    Home {
        page: usize,
    },
    /// `/<lang>/` and `/<lang>/page/N`. Whether `lang` is a site language, and not the default
    /// (whose home is `/`), only the page knows.
    LangHome {
        lang: String,
        page: usize,
    },
    /// `/<lang>/page/1`: a redirect to `/<lang>/` for a site language, else not a page.
    LangFirstPage {
        lang: String,
    },
    /// `lang` is `None` for `/<slug>` (the default language, which only the page knows).
    Post {
        lang: Option<String>,
        slug: String,
    },
    /// `/tag/<slug>` (`lang` `None`: the default language) and `/<lang>/tag/<slug>`, with
    /// `/page/N`. Whether `lang` is a site language, and not the default (whose pages are at
    /// the root), only the page knows.
    Tag {
        lang: Option<String>,
        slug: String,
        page: usize,
    },
    /// As [`Route::Tag`], for `/series/<slug>` and `/<lang>/series/<slug>`.
    Series {
        lang: Option<String>,
        slug: String,
        page: usize,
    },
    /// `/<lang>/tag/<slug>/page/1` and the series form: a redirect to the page-less URL of a
    /// site language (the default language's is the root), else not a page.
    TaxonFirstPage {
        kind: Taxon,
        lang: String,
        slug: String,
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
    /// `/search?q=` (`lang` `None`: the default language) and `/<lang>/search?q=`: the query
    /// string is read by the caller, not routed. Whether `lang` is a site language, and not the
    /// default (whose search is `/search`), only the page knows.
    Search {
        lang: Option<String>,
    },
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
        let lang = &path[1..path.len() - 1];
        if is_lang(lang) && !RESERVED.contains(&lang) {
            return Route::LangHome {
                lang: lang.to_owned(),
                page: 1,
            };
        }
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
        [kind @ ("tag" | "series"), slug, rest @ ..] if is_slug(slug) && is_paging(rest) => {
            taxon(kind, None, slug, rest)
        }
        [lang, kind @ ("tag" | "series"), slug, rest @ ..]
            if is_lang(lang) && !RESERVED.contains(lang) && is_slug(slug) && is_paging(rest) =>
        {
            taxon(kind, Some(lang), slug, rest)
        }
        ["archive", y] => match year(y) {
            Some(year) => Route::Archive { year, page: 1 },
            None => Route::NotFound,
        },
        ["archive", y, "page", n] => match (year(y), page_number(n)) {
            (Some(year), Some(1)) => Route::Redirect(archive(year, 1)),
            (Some(year), Some(page)) => Route::Archive { year, page },
            _ => Route::NotFound,
        },
        [lang, "page", n] if is_lang(lang) && !RESERVED.contains(lang) => match page_number(n) {
            Some(1) => Route::LangFirstPage {
                lang: (*lang).to_owned(),
            },
            Some(page) => Route::LangHome {
                lang: (*lang).to_owned(),
                page,
            },
            None => Route::NotFound,
        },
        ["search"] => Route::Search { lang: None },
        [lang, "search"] if is_lang(lang) && !RESERVED.contains(lang) => Route::Search {
            lang: Some((*lang).to_owned()),
        },
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
        [slug] if is_slug(slug) && !is_owned(slug) => Route::Post {
            lang: None,
            slug: (*slug).to_owned(),
        },
        [lang, slug] if is_lang(lang) && !RESERVED.contains(lang) && is_slug(slug) => Route::Post {
            lang: Some((*lang).to_owned()),
            slug: (*slug).to_owned(),
        },
        _ => Route::NotFound,
    }
}

/// The language (`None`: the default) and slug of `/<slug>/comments` or
/// `/<lang>/<slug>/comments`, where a post's comment form posts (POST only; GET there is a
/// 404 like any unknown path).
pub fn comment_form(path: &str) -> Option<(Option<&str>, &str)> {
    let rest = path.strip_prefix('/')?.strip_suffix("/comments")?;
    match rest.split_once('/') {
        None => (is_slug(rest) && !is_owned(rest)).then_some((None, rest)),
        Some((lang, slug)) => (is_lang(lang) && !RESERVED.contains(&lang) && is_slug(slug))
            .then_some((Some(lang), slug)),
    }
}

/// Where a post's comment form posts.
pub fn comments(lang: &str, default_lang: &str, slug: &str) -> String {
    format!("{}/comments", post_in(lang, default_lang, slug))
}

/// First path segments the router, the static assets or the API own, so none is a language
/// and none is a new post slug in the default language (the posts API refuses them from this
/// list). Two names are also single-segment pages that stay reachable if a post already has
/// the slug: see [`is_owned`].
pub(crate) const RESERVED: &[&str] = &[
    "page", "tag", "series", "archive", "search", "theme", "plugin", "media", "admin", "api",
    "healthz",
];

/// Whether `/<slug>` (one segment) belongs to the router rather than to a post. `series` is
/// reserved for new posts but `/series` alone is not owned: only `/series/<slug>` is, so a
/// default-language post that already has the slug (stored before the series pages) stays
/// served. (`search` is owned: the posts API refused it in the default language before.)
fn is_owned(slug: &str) -> bool {
    slug != "series" && RESERVED.contains(&slug)
}

/// What follows `/tag/<slug>` or `/series/<slug>`: nothing, or `/page/N`.
fn is_paging(rest: &[&str]) -> bool {
    matches!(rest, [] | ["page", _])
}

/// A tag or series route; `/page/1` is the page without it.
fn taxon(kind: &str, lang: Option<&str>, slug: &str, rest: &[&str]) -> Route {
    let kind = if kind == "tag" {
        Taxon::Tag
    } else {
        Taxon::Series
    };
    let (lang_owned, slug_owned) = (lang.map(str::to_owned), slug.to_owned());
    let page = match rest {
        [] => 1,
        [_, n] => match page_number(n) {
            Some(1) => {
                return match lang {
                    Some(l) => Route::TaxonFirstPage {
                        kind,
                        lang: l.to_owned(),
                        slug: slug_owned,
                    },
                    None => Route::Redirect(taxon_in(kind, None, slug, 1)),
                };
            }
            Some(p) => p,
            None => return Route::NotFound,
        },
        _ => return Route::NotFound,
    };
    match kind {
        Taxon::Tag => Route::Tag {
            lang: lang_owned,
            slug: slug_owned,
            page,
        },
        Taxon::Series => Route::Series {
            lang: lang_owned,
            slug: slug_owned,
            page,
        },
    }
}

/// Page `n` of a tag or series page: the default language's at the root, another's under
/// `/<lang>/` (`lang` `None` is the root).
fn taxon_in(kind: Taxon, lang: Option<&str>, slug: &str, n: usize) -> String {
    let prefix = lang.map_or_else(String::new, |l| format!("/{l}"));
    let seg = kind.segment();
    if n <= 1 {
        format!("{prefix}/{seg}/{slug}")
    } else {
        format!("{prefix}/{seg}/{slug}/page/{n}")
    }
}

/// Page `n` of tag `slug` in `lang`: `/tag/<slug>` for the default language, else
/// `/<lang>/tag/<slug>`.
pub fn tag_in(lang: &str, default_lang: &str, slug: &str, n: usize) -> String {
    taxon_in(Taxon::Tag, (lang != default_lang).then_some(lang), slug, n)
}

/// As [`tag_in`] for a series page.
pub fn series_in(lang: &str, default_lang: &str, slug: &str, n: usize) -> String {
    taxon_in(
        Taxon::Series,
        (lang != default_lang).then_some(lang),
        slug,
        n,
    )
}

/// The default language's search page; results are `?q=` on it.
pub const SEARCH: &str = "/search";

/// The sitemap (`Route::Sitemap`).
pub const SITEMAP: &str = "/sitemap.xml";

/// A language's search page: the default language's at the root, any other under `/<lang>/`.
pub fn search_in(lang: &str, default_lang: &str) -> String {
    if lang == default_lang {
        SEARCH.to_owned()
    } else {
        format!("/{lang}{SEARCH}")
    }
}

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
pub(super) fn is_lang(s: &str) -> bool {
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

/// A language's home: `/<lang>/`, then `/<lang>/page/N`. The default language's is
/// [`home_page`]; see [`home_in`].
pub fn lang_home(lang: &str, n: usize) -> String {
    if n <= 1 {
        format!("/{lang}/")
    } else {
        format!("/{lang}/page/{n}")
    }
}

/// Page `n` of the home of `lang`: the root for the default language, else under `/<lang>/`.
pub fn home_in(lang: &str, default_lang: &str, n: usize) -> String {
    if lang == default_lang {
        home_page(n)
    } else {
        lang_home(lang, n)
    }
}

pub fn post(slug: &str) -> String {
    format!("/{slug}")
}

/// A post's URL: the default language at the root, any other under `/<lang>/`.
pub fn post_in(lang: &str, default_lang: &str, slug: &str) -> String {
    if lang == default_lang {
        post(slug)
    } else {
        format!("/{lang}/{slug}")
    }
}

pub fn archive(year: i32, n: usize) -> String {
    if n <= 1 {
        format!("/archive/{year}")
    } else {
        format!("/archive/{year}/page/{n}")
    }
}
