//! The sample blog `blog theme dev` serves and `blog theme check` renders every template
//! with: the same posts the Worker serves before storage (`crates/worker/sample`), embedded at
//! build time. The CLI may not depend on the Worker, so the small date helper is repeated.

use super::embed::SAMPLE_FILES;
use plumblog_markdown::Options;
use plumblog_render::{
    Body, CommentItem, Comments, Link, PageInfo, Post, Report, Site, Theme, ThemeError,
};

pub const ORIGIN: &str = "http://127.0.0.1";

pub struct Sample {
    site: Site,
    /// Pinned first, then newest first; with the post's year.
    posts: Vec<(Post, i32)>,
    per_page: usize,
    lang: String,
}

/// Header lines (`key: value`) up to the first blank line, then the Markdown body.
fn split(text: &str) -> (Vec<(&str, &str)>, &str) {
    let (head, body) = text.split_once("\n\n").unwrap_or((text, ""));
    let fields = head
        .lines()
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim(), v.trim()))
        .collect();
    (fields, body)
}

fn field<'a>(fields: &[(&'a str, &'a str)], name: &str) -> &'a str {
    fields
        .iter()
        .find(|(k, _)| *k == name)
        .map_or("", |(_, v)| v)
}

/// `YYYY-MM-DD` at noon UTC, as the Worker's sample does (Hinnant's days-from-civil).
fn unix_from_date(date: &str) -> (i64, i32) {
    let mut parts = date.split('-').map(|p| p.parse::<i64>().unwrap_or(1));
    let (y, m, d) = (
        parts.next().unwrap_or(1970),
        parts.next().unwrap_or(1),
        parts.next().unwrap_or(1),
    );
    let yy = if m <= 2 { y - 1 } else { y };
    let era = yy.div_euclid(400);
    let yoe = yy - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    (
        (era * 146_097 + doe - 719_468) * 86_400 + 43_200,
        i32::try_from(y).unwrap_or(1970),
    )
}

impl Sample {
    #[must_use]
    pub fn load() -> Self {
        let site_txt = SAMPLE_FILES
            .iter()
            .find(|(n, _)| *n == "site.txt")
            .map_or("", |(_, t)| *t);
        let (s, _) = split(site_txt);
        let lang = field(&s, "lang").to_owned();
        let mut posts: Vec<(Post, i32)> = SAMPLE_FILES
            .iter()
            .filter(|(n, _)| *n != "site.txt")
            .map(|(name, text)| {
                let (f, body) = split(text);
                let slug = name.trim_end_matches(".txt");
                let (at, year) = unix_from_date(field(&f, "date"));
                let excerpt = field(&f, "excerpt");
                let post = Post {
                    url: format!("/{slug}"),
                    title: field(&f, "title").to_owned(),
                    lang: lang.clone(),
                    published_at: at,
                    pinned: field(&f, "pinned") == "yes",
                    excerpt: (!excerpt.is_empty()).then(|| excerpt.to_owned()),
                    body: plumblog_markdown::render(body, Options::default()),
                    tags: field(&f, "tags")
                        .split(',')
                        .map(str::trim)
                        .filter(|t| !t.is_empty())
                        .map(|t| Link {
                            name: t.to_owned(),
                            url: format!("/tag/{t}"),
                        })
                        .collect(),
                    cover: None,
                    thumb: None,
                    translations: Vec::new(),
                    series: None,
                };
                (post, year)
            })
            .collect();
        posts.sort_by(|a, b| {
            b.0.pinned
                .cmp(&a.0.pinned)
                .then(b.0.published_at.cmp(&a.0.published_at))
        });
        let site = Site {
            title: field(&s, "title").to_owned(),
            description: field(&s, "description").to_owned(),
            default_lang: lang.clone(),
            home_url: "/".to_owned(),
            search_url: Some("/search".to_owned()),
            feed_url: "/feed.xml".to_owned(),
            rss_url: "/rss.xml".to_owned(),
            json_feed_url: "/feed.json".to_owned(),
            // `data:,` keeps the browser from asking for /favicon.ico.
            icon_url: "data:,".to_owned(),
            image_url: None,
            author: None,
            footer: field(&s, "footer").to_owned(),
            nav: Vec::new(),
        };
        Self {
            site,
            posts,
            per_page: field(&s, "per_page").parse().unwrap_or(5).max(1),
            lang,
        }
    }

    /// Paths `blog theme check` renders: every page kind, with and without comments.
    #[must_use]
    pub fn check_targets(&self) -> Vec<String> {
        let first = self.posts.first().map_or("/", |(p, _)| p.url.as_str());
        let year = self.posts.first().map_or(2025, |(_, y)| *y);
        let tag = self
            .posts
            .iter()
            .find_map(|(p, _)| p.tags.first())
            .map_or("life", |t| t.name.as_str());
        let mut v = vec!["/".to_owned(), "/page/2".to_owned(), first.to_owned()];
        v.push(format!("{first}?comments=off"));
        v.push(format!("/tag/{tag}"));
        v.push(format!("/archive/{year}"));
        v.push("/search".to_owned());
        v.push("/search?q=post".to_owned());
        v.push("/no-such-page".to_owned());
        v
    }

    fn slice(
        &self,
        all: &[&Post],
        n: usize,
        base: &str,
    ) -> (Vec<Post>, Option<String>, Option<String>) {
        let from = (n - 1) * self.per_page;
        let part: Vec<Post> = all
            .iter()
            .skip(from)
            .take(self.per_page)
            .map(|p| (*p).clone())
            .collect();
        let link = |m: usize| match (m, base) {
            (1, "") => "/".to_owned(),
            (1, _) => base.to_owned(),
            _ => format!("{base}/page/{m}"),
        };
        let older = (from + self.per_page < all.len()).then(|| link(n + 1));
        let newer = (n > 1).then(|| link(n - 1));
        (part, older, newer)
    }

    /// Renders `target` (`/path?query`) with `theme`; `lang` is the page language.
    /// Returns the HTTP status and the page. Unknown paths are the 404 page.
    pub fn render(
        &self,
        theme: &Theme,
        target: &str,
        lang: &str,
    ) -> (u16, Result<Report, ThemeError>) {
        let (path, query) = target.split_once('?').unwrap_or((target, ""));
        let at = Route {
            sample: self,
            theme,
            lang,
            path,
            query,
        };
        let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let found = match segs.as_slice() {
            [] => at.home(1),
            ["page", n] => page_n(n).and_then(|n| at.home(n)),
            ["tag", tag, rest @ ..] => at.tag(tag, rest),
            ["archive", year, rest @ ..] => at.archive(year, rest),
            ["search"] => Some(at.search()),
            [slug] => at.post(slug),
            _ => None,
        };
        found.map_or_else(|| at.not_found(), |r| (200, r))
    }

    #[must_use]
    pub fn lang(&self) -> &str {
        &self.lang
    }
}

fn sample_comments() -> Comments {
    let item = |id, author: &str, at, src: &str| CommentItem {
        id,
        author: author.to_owned(),
        owner: false,
        created_at: at,
        html: plumblog_markdown::render_comment(src).html,
    };
    Comments {
        open: true,
        items: vec![
            item(
                1,
                "Linh",
                1_780_000_000,
                "A first comment with *emphasis* and a [link](https://example.com).",
            ),
            item(
                2,
                "A very long display name that keeps going to test wrapping",
                1_780_100_000,
                "A second one.\n\nWith two paragraphs.",
            ),
        ],
        action_url: "/comment".to_owned(),
        turnstile_key: String::new(),
        max_length: 2_000,
        name_max_length: 60,
        max_links: 2,
        more: false,
    }
}

fn page_n(s: &str) -> Option<usize> {
    s.parse::<usize>().ok().filter(|n| *n >= 1)
}

/// `/page/N` or nothing after a tag or year.
fn list_page(rest: &[&str]) -> Option<usize> {
    match rest {
        [] => Some(1),
        ["page", n] => page_n(n),
        _ => None,
    }
}

type Rendered = Result<Report, ThemeError>;

/// One request: the sample, the theme and the address, so each page kind is a small method.
struct Route<'a> {
    sample: &'a Sample,
    theme: &'a Theme,
    lang: &'a str,
    path: &'a str,
    query: &'a str,
}

impl Route<'_> {
    fn param(&self, name: &str) -> Option<&str> {
        self.query
            .split('&')
            .find_map(|kv| kv.strip_prefix(name).and_then(|v| v.strip_prefix('=')))
    }

    fn render(&self, body: &Body<'_>) -> Rendered {
        let page = PageInfo {
            lang: self.lang.to_owned(),
            canonical: format!("{ORIGIN}{}", self.path),
            origin: ORIGIN.to_owned(),
        };
        self.theme.render_reporting(&self.sample.site, &page, body)
    }

    fn all(&self) -> Vec<&Post> {
        self.sample.posts.iter().map(|(p, _)| p).collect()
    }

    fn home(&self, n: usize) -> Option<Rendered> {
        let all = self.all();
        if (n - 1) * self.sample.per_page >= all.len().max(1) {
            return None;
        }
        let (part, older_url, newer_url) = self.sample.slice(&all, n, "");
        let posts: Vec<&Post> = part.iter().collect();
        Some(self.render(&Body::Home {
            posts: &posts,
            older_url,
            newer_url,
        }))
    }

    fn tag(&self, tag: &str, rest: &[&str]) -> Option<Rendered> {
        let n = list_page(rest)?;
        let posts: Vec<&Post> = self
            .all()
            .into_iter()
            .filter(|p| p.tags.iter().any(|t| t.name == tag))
            .collect();
        if posts.is_empty() {
            return None;
        }
        let (part, older_url, newer_url) = self.sample.slice(&posts, n, &format!("/tag/{tag}"));
        let refs: Vec<&Post> = part.iter().collect();
        let link = Link {
            name: tag.to_owned(),
            url: format!("/tag/{tag}"),
        };
        Some(self.render(&Body::Tag {
            tag: &link,
            posts: &refs,
            older_url,
            newer_url,
        }))
    }

    fn archive(&self, year: &str, rest: &[&str]) -> Option<Rendered> {
        let year = year.parse::<i32>().ok()?;
        let n = list_page(rest)?;
        let posts: Vec<&Post> = self
            .sample
            .posts
            .iter()
            .filter(|(_, y)| *y == year)
            .map(|(p, _)| p)
            .collect();
        if posts.is_empty() {
            return None;
        }
        let (part, older_url, newer_url) =
            self.sample.slice(&posts, n, &format!("/archive/{year}"));
        let refs: Vec<&Post> = part.iter().collect();
        let mut years: Vec<i32> = self.sample.posts.iter().map(|(_, y)| *y).collect();
        years.sort_unstable_by(|a, b| b.cmp(a));
        years.dedup();
        let years = years
            .into_iter()
            .map(|y| Link {
                name: y.to_string(),
                url: format!("/archive/{y}"),
            })
            .collect();
        Some(self.render(&Body::Archive {
            year,
            posts: &refs,
            years,
            older_url,
            newer_url,
        }))
    }

    fn search(&self) -> Rendered {
        let query = self.param("q").unwrap_or("").replace('+', " ");
        let needle = query.to_lowercase();
        let hits: Vec<&Post> = if needle.is_empty() {
            Vec::new()
        } else {
            self.all()
                .into_iter()
                .filter(|p| {
                    p.title.to_lowercase().contains(&needle)
                        || p.body.excerpt.to_lowercase().contains(&needle)
                })
                .collect()
        };
        self.render(&Body::Search {
            query: &query,
            posts: &hits,
            more: false,
        })
    }

    fn post(&self, slug: &str) -> Option<Rendered> {
        let all = self.all();
        let i = all.iter().position(|p| p.url == format!("/{slug}"))?;
        let comments = (self.param("comments") != Some("off")).then(sample_comments);
        Some(self.render(&Body::Post {
            post: all[i],
            prev: i.checked_sub(1).map(|j| all[j]),
            next: all.get(i + 1).copied(),
            comments: comments.as_ref(),
        }))
    }

    fn not_found(&self) -> (u16, Rendered) {
        let recent: Vec<&Post> = self.all().into_iter().take(3).collect();
        (404, self.render(&Body::NotFound { recent: &recent }))
    }
}
