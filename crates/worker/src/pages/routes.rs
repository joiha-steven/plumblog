//! Reader URLs, in one place: parsing a path and building the links pages point to.
//!
//! `/` and `/page/N` home, `/<slug>` post, `/tag/<slug>` (and `/page/N`), `/archive/<year>`.
//! `page`, `tag`, `archive`, `theme`, `admin` and `api` cannot be post slugs.

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
    },
    /// One canonical URL per page: `/page/1` and trailing slashes redirect.
    Redirect(String),
    NotFound,
}

pub fn parse(path: &str) -> Route {
    if path != "/" && path.ends_with('/') {
        return Route::Redirect(path.trim_end_matches('/').to_owned());
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
        ["archive", y] => match y.parse::<i32>() {
            Ok(year) if y.len() == 4 && year > 0 => Route::Archive { year },
            _ => Route::NotFound,
        },
        [slug] if is_slug(slug) && !RESERVED.contains(slug) => Route::Post {
            slug: (*slug).to_owned(),
        },
        _ => Route::NotFound,
    }
}

const RESERVED: &[&str] = &["page", "tag", "archive", "theme", "admin", "api"];

/// Slugs are lowercase ASCII letters, digits and dashes; anything else is a 404 without a
/// storage lookup.
fn is_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 200
        && s.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
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

pub fn archive(year: i32) -> String {
    format!("/archive/{year}")
}
