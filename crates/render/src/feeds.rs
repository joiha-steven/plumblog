//! Machine-read documents: Atom, RSS 2.0, JSON Feed 1.1, sitemap, robots.txt and llms.txt.
//! These are formats, not reader layout, so they are built here rather than in themes. Every
//! URL is absolute and every date carries a time zone, as feed readers and crawlers require.

use crate::data::{Alternate, Post, civil};
use crate::json::J;
use std::fmt::Write as _;

/// Site facts every feed needs. Paths are site-relative; `origin` makes them absolute.
pub struct FeedMeta<'a> {
    pub title: &'a str,
    pub description: &'a str,
    pub lang: &'a str,
    pub origin: &'a str,
    pub home_path: &'a str,
    pub atom_path: &'a str,
    pub rss_path: &'a str,
    pub json_path: &'a str,
    pub author: Option<&'a str>,
    /// Unix time the site last changed; the feed date when there are no posts yet.
    pub updated: i64,
}

/// One `<url>` of the sitemap. `alternates` lists every language version, this one included
/// (search engines ignore hreflang sets that do not point back to themselves).
pub struct SitemapUrl {
    pub path: String,
    pub lastmod: Option<i64>,
    pub alternates: Vec<Alternate>,
}

const GENERATOR: &str = "plumblog";

#[must_use]
pub fn atom(m: &FeedMeta<'_>, posts: &[&Post]) -> String {
    let home = abs(m.origin, m.home_path);
    let mut o = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    let _ = writeln!(
        o,
        "<feed xmlns=\"http://www.w3.org/2005/Atom\" xml:lang=\"{}\">",
        x(m.lang)
    );
    let _ = writeln!(o, "<title>{}</title>", x(m.title));
    if !m.description.is_empty() {
        let _ = writeln!(o, "<subtitle>{}</subtitle>", x(m.description));
    }
    let _ = writeln!(
        o,
        "<link href=\"{}\" rel=\"alternate\" type=\"text/html\"/>",
        x(&home)
    );
    let _ = writeln!(
        o,
        "<link href=\"{}\" rel=\"self\" type=\"application/atom+xml\"/>",
        x(&abs(m.origin, m.atom_path))
    );
    // The feed's own URL: unique per language and format, unlike the home page.
    let _ = writeln!(o, "<id>{}</id>", x(&abs(m.origin, m.atom_path)));
    let _ = writeln!(o, "<updated>{}</updated>", rfc3339(newest(m, posts)));
    // Atom requires an author on the feed or on every entry.
    let _ = writeln!(
        o,
        "<author><name>{}</name></author>",
        x(m.author.unwrap_or(m.title))
    );
    let _ = writeln!(o, "<generator>{GENERATOR}</generator>");
    for p in posts {
        let url = abs(m.origin, &p.url);
        o.push_str("<entry>\n");
        let _ = writeln!(o, "<title>{}</title>", x(&p.title));
        let _ = writeln!(
            o,
            "<link href=\"{}\" rel=\"alternate\" type=\"text/html\"/>",
            x(&url)
        );
        let _ = writeln!(o, "<id>{}</id>", x(&url));
        let _ = writeln!(o, "<published>{}</published>", rfc3339(p.published_at));
        let _ = writeln!(o, "<updated>{}</updated>", rfc3339(p.published_at));
        let summary = excerpt(p);
        if !summary.is_empty() {
            let _ = writeln!(o, "<summary type=\"text\">{}</summary>", x(summary));
        }
        let _ = writeln!(
            o,
            "<content type=\"html\">{}</content>",
            x(&absolutize(&p.body.html, m.origin))
        );
        for t in &p.tags {
            let _ = writeln!(o, "<category term=\"{}\"/>", x(&t.name));
        }
        o.push_str("</entry>\n");
    }
    o.push_str("</feed>\n");
    o
}

#[must_use]
pub fn rss(m: &FeedMeta<'_>, posts: &[&Post]) -> String {
    let mut o = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    o.push_str(
        "<rss version=\"2.0\" xmlns:atom=\"http://www.w3.org/2005/Atom\" xmlns:content=\"http://purl.org/rss/1.0/modules/content/\">\n<channel>\n",
    );
    let _ = writeln!(o, "<title>{}</title>", x(m.title));
    let _ = writeln!(o, "<link>{}</link>", x(&abs(m.origin, m.home_path)));
    // RSS requires a channel description; the title stands in when the site has none.
    let description = if m.description.is_empty() {
        m.title
    } else {
        m.description
    };
    let _ = writeln!(o, "<description>{}</description>", x(description));
    let _ = writeln!(o, "<language>{}</language>", x(m.lang));
    let _ = writeln!(
        o,
        "<lastBuildDate>{}</lastBuildDate>",
        rfc822(newest(m, posts))
    );
    let _ = writeln!(
        o,
        "<atom:link href=\"{}\" rel=\"self\" type=\"application/rss+xml\"/>",
        x(&abs(m.origin, m.rss_path))
    );
    let _ = writeln!(o, "<generator>{GENERATOR}</generator>");
    for p in posts {
        let url = abs(m.origin, &p.url);
        o.push_str("<item>\n");
        let _ = writeln!(o, "<title>{}</title>", x(&p.title));
        let _ = writeln!(o, "<link>{}</link>", x(&url));
        let _ = writeln!(o, "<guid isPermaLink=\"true\">{}</guid>", x(&url));
        let _ = writeln!(o, "<pubDate>{}</pubDate>", rfc822(p.published_at));
        let _ = writeln!(o, "<description>{}</description>", x(excerpt(p)));
        let _ = writeln!(
            o,
            "<content:encoded>{}</content:encoded>",
            x(&absolutize(&p.body.html, m.origin))
        );
        for t in &p.tags {
            let _ = writeln!(o, "<category>{}</category>", x(&t.name));
        }
        o.push_str("</item>\n");
    }
    o.push_str("</channel>\n</rss>\n");
    o
}

#[must_use]
pub fn json_feed(m: &FeedMeta<'_>, posts: &[&Post]) -> String {
    let items = posts
        .iter()
        .map(|p| {
            let url = abs(m.origin, &p.url);
            let summary = excerpt(p);
            J::obj(vec![
                ("id", Some(J::s(url.clone()))),
                ("url", Some(J::s(url))),
                ("title", Some(J::s(p.title.clone()))),
                (
                    "content_html",
                    Some(J::s(absolutize(&p.body.html, m.origin))),
                ),
                ("summary", (!summary.is_empty()).then(|| J::s(summary))),
                (
                    "image",
                    p.cover.as_ref().map(|c| J::s(abs(m.origin, &c.src))),
                ),
                ("date_published", Some(J::s(rfc3339(p.published_at)))),
                ("language", Some(J::s(p.lang.clone()))),
                (
                    "tags",
                    (!p.tags.is_empty())
                        .then(|| J::Arr(p.tags.iter().map(|t| J::s(t.name.clone())).collect())),
                ),
            ])
        })
        .collect();
    let feed = J::obj(vec![
        ("version", Some(J::s("https://jsonfeed.org/version/1.1"))),
        ("title", Some(J::s(m.title))),
        ("home_page_url", Some(J::s(abs(m.origin, m.home_path)))),
        ("feed_url", Some(J::s(abs(m.origin, m.json_path)))),
        (
            "description",
            (!m.description.is_empty()).then(|| J::s(m.description)),
        ),
        ("language", Some(J::s(m.lang))),
        (
            "authors",
            m.author
                .map(|a| J::Arr(vec![J::obj(vec![("name", Some(J::s(a)))])])),
        ),
        ("items", Some(J::Arr(items))),
    ]);
    let mut s = feed.to_string();
    s.push('\n');
    s
}

#[must_use]
pub fn sitemap(origin: &str, urls: &[SitemapUrl]) -> String {
    let mut o = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n");
    o.push_str(
        "<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\" xmlns:xhtml=\"http://www.w3.org/1999/xhtml\">\n",
    );
    for u in urls {
        o.push_str("<url>");
        let _ = write!(o, "<loc>{}</loc>", x(&abs(origin, &u.path)));
        if let Some(t) = u.lastmod {
            let _ = write!(o, "<lastmod>{}</lastmod>", rfc3339(t));
        }
        for a in &u.alternates {
            let _ = write!(
                o,
                "<xhtml:link rel=\"alternate\" hreflang=\"{}\" href=\"{}\"/>",
                x(&a.lang),
                x(&abs(origin, &a.url))
            );
        }
        o.push_str("</url>\n");
    }
    o.push_str("</urlset>\n");
    o
}

/// Everything public may be crawled; the admin and API are not pages.
#[must_use]
pub fn robots(origin: &str) -> String {
    format!(
        "User-agent: *\nAllow: /\nDisallow: /admin/\nDisallow: /api/\n\nSitemap: {origin}/sitemap.xml\n"
    )
}

/// llms.txt (llmstxt.org): the site in Markdown for language models. `posts_heading` is the
/// locale's word for "Posts".
#[must_use]
pub fn llms(m: &FeedMeta<'_>, posts_heading: &str, posts: &[&Post]) -> String {
    let mut o = format!("# {}\n\n", md_line(m.title));
    if !m.description.is_empty() {
        let _ = write!(o, "> {}\n\n", md_line(m.description));
    }
    let _ = write!(o, "## {}\n\n", md_line(posts_heading));
    for p in posts {
        let _ = write!(
            o,
            "- [{}]({})",
            md_line(&p.title).replace(['[', ']'], ""),
            abs(m.origin, &p.url)
        );
        let summary = excerpt(p);
        if !summary.is_empty() {
            let _ = write!(o, ": {}", md_line(summary));
        }
        o.push('\n');
    }
    o
}

fn excerpt(p: &Post) -> &str {
    match &p.excerpt {
        Some(e) if !e.trim().is_empty() => e.trim(),
        _ => p.body.excerpt.trim(),
    }
}

/// Newest post, else the site's last change: never 1970 for an empty feed.
fn newest(m: &FeedMeta<'_>, posts: &[&Post]) -> i64 {
    posts
        .iter()
        .map(|p| p.published_at)
        .max()
        .unwrap_or(m.updated)
}

/// hreflang set for a post (site-relative URLs): every language version, this one included,
/// plus `x-default` for the default language's version. Empty when there is only one.
#[must_use]
pub fn alternates(post: &Post, default_lang: &str) -> Vec<Alternate> {
    if post.translations.is_empty() {
        return Vec::new();
    }
    let mut all = vec![Alternate {
        lang: post.lang.clone(),
        url: post.url.clone(),
    }];
    all.extend(post.translations.iter().cloned());
    if let Some(d) = all.iter().find(|a| a.lang == default_lang) {
        let url = d.url.clone();
        all.push(Alternate {
            lang: "x-default".to_owned(),
            url,
        });
    }
    all
}

/// One line of Markdown text: line breaks would start new blocks.
fn md_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Site-relative paths become absolute; absolute URLs pass through.
pub(crate) fn abs(origin: &str, path: &str) -> String {
    if path.starts_with('/') && !path.starts_with("//") {
        format!("{origin}{path}")
    } else {
        path.to_owned()
    }
}

/// Feed readers show content away from the site, so `href="/..."` and `src="/..."` in stored
/// post HTML must carry the origin.
pub(crate) fn absolutize(html: &str, origin: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    while let Some(i) = rest.find("=\"/") {
        let attr_end = i;
        let is_link_attr =
            rest[..attr_end].ends_with(" href") || rest[..attr_end].ends_with(" src");
        let after = &rest[i + 2..];
        out.push_str(&rest[..i + 2]);
        if is_link_attr && !after.starts_with("//") {
            out.push_str(origin);
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

/// XML text and attribute escaping.
pub(crate) fn x(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            c if xml_char(c) => o.push(c),
            // Not allowed in XML 1.0 even as a reference; dropping them keeps the feed parseable.
            _ => {}
        }
    }
    o
}

/// XML 1.0 `Char`: #x9 | #xA | #xD | [#x20-#xD7FF] | [#xE000-#xFFFD] | [#x10000-#x10FFFF].
/// Rust `char` is never a surrogate, so the gap between #xD7FF and #xE000 needs no check.
fn xml_char(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\r' | '\u{20}'..='\u{FFFD}' | '\u{10000}'..)
}

/// `2026-10-02T12:00:00Z` (Atom, JSON Feed, sitemap, JSON-LD).
#[must_use]
pub fn rfc3339(unix: i64) -> String {
    let (y, mo, d) = civil(unix);
    let s = unix.rem_euclid(86_400);
    format!(
        "{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}Z",
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}

/// `Fri, 02 Oct 2026 12:00:00 +0000` (RSS). English names are part of the format, not UI text.
#[must_use]
pub fn rfc822(unix: i64) -> String {
    const DAYS: [&str; 7] = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"]; // 1970-01-01 was a Thursday
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (y, mo, d) = civil(unix);
    let s = unix.rem_euclid(86_400);
    let wd = usize::try_from(unix.div_euclid(86_400).rem_euclid(7)).unwrap_or(0);
    let month = MONTHS[usize::try_from(mo).unwrap_or(1).clamp(1, 12) - 1];
    format!(
        "{}, {d:02} {month} {y:04} {:02}:{:02}:{:02} +0000",
        DAYS[wd],
        s / 3600,
        s / 60 % 60,
        s % 60
    )
}

#[cfg(test)]
mod tests {
    use super::{FeedMeta, absolutize, atom, rfc822, rfc3339, rss, x};

    #[test]
    fn an_empty_feed_is_dated_by_the_site_not_1970() {
        let m = FeedMeta {
            title: "T",
            description: "",
            lang: "en",
            origin: "https://b.test",
            home_path: "/",
            atom_path: "/feed.xml",
            rss_path: "/rss.xml",
            json_path: "/feed.json",
            author: None,
            updated: 1_790_942_400,
        };
        let a = atom(&m, &[]);
        assert!(a.contains("<updated>2026-10-02T12:00:00Z</updated>"), "{a}");
        let r = rss(&m, &[]);
        assert!(
            r.contains("<lastBuildDate>Fri, 02 Oct 2026 12:00:00 +0000</lastBuildDate>"),
            "{r}"
        );
        assert!(!a.contains("1970") && !r.contains("1970"));
    }

    #[test]
    fn dates_in_both_formats() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00Z");
        assert_eq!(rfc822(0), "Thu, 01 Jan 1970 00:00:00 +0000");
        // 2026-10-02 12:00 UTC was a Friday.
        assert_eq!(rfc3339(1_790_942_400), "2026-10-02T12:00:00Z");
        assert_eq!(rfc822(1_790_942_400), "Fri, 02 Oct 2026 12:00:00 +0000");
    }

    #[test]
    fn links_become_absolute_but_not_protocol_relative_or_text() {
        let html = r#"<a href="/tag/x">a</a> <img src="/i.png"> <a href="//cdn/x">b</a> <a href="https://e.com/">c</a> <p data-x="/keep">="/text</p>"#;
        assert_eq!(
            absolutize(html, "https://b.test"),
            r#"<a href="https://b.test/tag/x">a</a> <img src="https://b.test/i.png"> <a href="//cdn/x">b</a> <a href="https://e.com/">c</a> <p data-x="/keep">="/text</p>"#
        );
    }

    #[test]
    fn xml_escaping_drops_forbidden_controls() {
        assert_eq!(x("a&b<c>\"'\u{1}\n"), "a&amp;b&lt;c&gt;&quot;&apos;\n");
        assert_eq!(
            x("\u{FFFE}\u{FFFF}\u{7F}\u{D7FF}\u{E000}\u{FFFD}\u{1F600}\u{10FFFF}\t\r"),
            "\u{7F}\u{D7FF}\u{E000}\u{FFFD}\u{1F600}\u{10FFFF}\t\r"
        );
    }
}
