//! Template data. Inputs (`Site`, `Post`...) come from the Worker; the private `*Data` structs
//! are what templates see, so every template variable is defined by a Rust type here.

use plumblog_markdown::Rendered;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Image {
    pub src: String,
    pub srcset: Option<String>,
    pub width: u32,
    pub height: u32,
    pub alt: String,
}

/// A named link: tags, navigation, archive years.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Link {
    pub name: String,
    pub url: String,
}

/// One language version of a page (hreflang). `url` is site-relative on input, absolute in
/// template data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Alternate {
    pub lang: String,
    pub url: String,
}

/// Site-wide values every page gets as `site`.
#[derive(Clone, Debug, Serialize)]
pub struct Site {
    pub title: String,
    pub description: String,
    /// The site's main language; its versions get `hreflang="x-default"`.
    pub default_lang: String,
    pub home_url: String,
    pub search_url: Option<String>,
    /// Atom feed (the main one); `rss_url` and `json_feed_url` are the same posts.
    pub feed_url: String,
    pub rss_url: String,
    pub json_feed_url: String,
    pub icon_url: String,
    /// Social card image when a page has no cover; site-relative or absolute.
    pub image_url: Option<String>,
    /// Named as the author in feeds and JSON-LD; the site itself when `None`.
    pub author: Option<String>,
    pub footer: String,
    pub nav: Vec<NavItem>,
}

#[derive(Clone, Debug, Serialize)]
pub struct NavItem {
    pub label: String,
    pub url: String,
    pub current: bool,
}

/// Per-request page facts only the router knows.
#[derive(Clone, Debug)]
pub struct PageInfo {
    pub lang: String,
    /// Absolute URL of this page.
    pub canonical: String,
    /// `https://host`, for making site-relative URLs absolute: the site's configured public
    /// origin, never the request's Host header in production.
    pub origin: String,
}

/// A published post as the reader site needs it. `body` is the HTML stored on save, so reading
/// never parses Markdown.
#[derive(Clone, Debug)]
pub struct Post {
    pub url: String,
    pub title: String,
    pub lang: String,
    /// Unix seconds, UTC.
    pub published_at: i64,
    pub pinned: bool,
    /// Written by the author; when empty the stored auto excerpt is cut to `excerpt_words`.
    pub excerpt: Option<String>,
    pub body: Rendered,
    pub tags: Vec<Link>,
    pub cover: Option<Image>,
    pub thumb: Option<Image>,
    /// Other language versions of this post (hreflang, sitemap).
    pub translations: Vec<Alternate>,
}

/// What a page shows; picks the template.
pub enum Body<'a> {
    Home {
        posts: &'a [&'a Post],
        older_url: Option<String>,
        newer_url: Option<String>,
    },
    Post {
        post: &'a Post,
        prev: Option<&'a Post>,
        next: Option<&'a Post>,
    },
    Tag {
        tag: &'a Link,
        posts: &'a [&'a Post],
        older_url: Option<String>,
        newer_url: Option<String>,
    },
    Archive {
        year: i32,
        posts: &'a [&'a Post],
        years: Vec<Link>,
        older_url: Option<String>,
        newer_url: Option<String>,
    },
    NotFound {
        recent: &'a [&'a Post],
    },
}

/// One post row (`entry.html`).
#[derive(Serialize)]
pub(crate) struct EntryData<'a> {
    pub title: &'a str,
    pub url: &'a str,
    pub lang: &'a str,
    pub pinned: bool,
    pub excerpt: String,
    pub date_iso: String,
    pub date_display: String,
    pub year: i32,
    pub reading_minutes: u32,
    pub cover: Option<&'a Image>,
    pub thumb: Option<&'a Image>,
}

#[derive(Serialize)]
pub(crate) struct PostData<'a> {
    #[serde(flatten)]
    pub entry: EntryData<'a>,
    pub html: &'a str,
    /// The theme loads its code-highlighting script only when this is true.
    pub has_code: bool,
    /// Math is rendered to `MathML` on save; a theme may still want a math stylesheet.
    pub has_math: bool,
    /// Click-to-load embed facades are in the post.
    pub has_embed: bool,
    pub tags: &'a [Link],
    pub prev: Option<LinkData<'a>>,
    pub next: Option<LinkData<'a>>,
}

#[derive(Serialize)]
pub(crate) struct LinkData<'a> {
    pub title: &'a str,
    pub url: &'a str,
}

/// Calendar date (UTC) of a unix timestamp. Hinnant's days-to-civil, so no date crate is needed.
pub(crate) fn civil(unix: i64) -> (i32, u32, u32) {
    let z = unix.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    // Ranges are bounded by the algorithm: d in 1..=31, m in 1..=12.
    (
        i32::try_from(y).unwrap_or(i32::MAX),
        u32::try_from(m).unwrap_or(1),
        u32::try_from(d).unwrap_or(1),
    )
}

/// First `max` words of `text`; an ellipsis marks a cut. 0 means no excerpt.
pub(crate) fn cut_words(text: &str, max: usize) -> String {
    if max == 0 {
        return String::new();
    }
    let mut seen = 0;
    for (i, _) in text.match_indices(char::is_whitespace) {
        if !text[..i].ends_with(char::is_whitespace) {
            seen += 1;
            if seen == max {
                let rest = text[i..].trim();
                if rest.is_empty() {
                    break;
                }
                return format!(
                    "{}\u{2026}",
                    text[..i].trim_end_matches([',', ';', ':', '.'])
                );
            }
        }
    }
    text.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::{civil, cut_words};

    #[test]
    fn civil_dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(951_782_400), (2000, 2, 29));
        assert_eq!(civil(1_772_668_800), (2026, 3, 5));
        assert_eq!(civil(-86_400), (1969, 12, 31));
    }

    #[test]
    fn cuts_at_word_boundary() {
        assert_eq!(cut_words("one two three", 2), "one two\u{2026}");
        assert_eq!(cut_words("one, two, three", 2), "one, two\u{2026}");
        assert_eq!(cut_words("one two", 2), "one two");
        assert_eq!(cut_words("one two", 5), "one two");
        assert_eq!(cut_words("one two", 0), "");
    }
}
