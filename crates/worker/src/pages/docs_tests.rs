//! Feed and sitemap rules that validators (W3C Feed Validator, JSON Feed spec, sitemaps.org)
//! check, encoded as tests against the sample posts.

use super::source::StoredPost;
use super::tests_support::{Fixture, get_from, get_typed};

/// Strict enough to catch what breaks feed readers: balanced tags, quoted attributes, only
/// the five XML entities or numeric references, nothing after the root element.
fn well_formed(xml: &str) -> Result<(), String> {
    let mut stack: Vec<&str> = Vec::new();
    let mut rest = xml
        .strip_prefix("<?xml version=\"1.0\" encoding=\"utf-8\"?>\n")
        .ok_or("missing XML declaration")?;
    let mut roots = 0;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix("</") {
            let end = r.find('>').ok_or("unclosed end tag")?;
            let name = r[..end].trim();
            if stack.pop() != Some(name) {
                return Err(format!("unexpected </{name}>"));
            }
            rest = &r[end + 1..];
        } else if let Some(r) = rest.strip_prefix('<') {
            let end = r.find('>').ok_or("unclosed tag")?;
            let inner = &r[..end];
            let (inner, empty) = inner
                .strip_suffix('/')
                .map_or((inner, false), |i| (i, true));
            let name_end = inner.find(char::is_whitespace).unwrap_or(inner.len());
            let name = &inner[..name_end];
            if name.is_empty()
                || !name
                    .chars()
                    .all(|c| c.is_alphanumeric() || ":-_.".contains(c))
            {
                return Err(format!("bad element name {name:?}"));
            }
            let mut attrs = inner[name_end..].trim();
            while !attrs.is_empty() {
                let eq = attrs
                    .find("=\"")
                    .ok_or(format!("unquoted attribute in <{name}>"))?;
                let val_end = attrs[eq + 2..].find('"').ok_or("unclosed attribute")?;
                entities(&attrs[eq + 2..eq + 2 + val_end])?;
                if attrs[eq + 2..eq + 2 + val_end].contains('<') {
                    return Err("< in attribute".into());
                }
                attrs = attrs[eq + 3 + val_end..].trim_start();
            }
            if stack.is_empty() {
                roots += 1;
            }
            if !empty {
                stack.push(name);
            }
            rest = &r[end + 1..];
        } else {
            let end = rest.find('<').unwrap_or(rest.len());
            let text = &rest[..end];
            if stack.is_empty() && !text.trim().is_empty() {
                return Err("text outside the root element".into());
            }
            entities(text)?;
            if text.contains('>') && text.contains("]]>") {
                return Err("]]> in text".into());
            }
            rest = &rest[end..];
        }
    }
    if !stack.is_empty() {
        return Err(format!("unclosed {stack:?}"));
    }
    if roots != 1 {
        return Err(format!("{roots} root elements"));
    }
    Ok(())
}

fn entities(text: &str) -> Result<(), String> {
    for (i, _) in text.match_indices('&') {
        let tail = &text[i + 1..];
        let end = tail.find(';').ok_or("bare &")?;
        let e = &tail[..end];
        let ok = matches!(e, "amp" | "lt" | "gt" | "quot" | "apos")
            || e.strip_prefix("#x")
                .is_some_and(|h| !h.is_empty() && h.chars().all(|c| c.is_ascii_hexdigit()))
            || e.strip_prefix('#')
                .is_some_and(|d| !d.is_empty() && d.chars().all(|c| c.is_ascii_digit()));
        if !ok {
            return Err(format!("unknown entity &{e};"));
        }
    }
    Ok(())
}

/// Text of every `<tag>...</tag>` (no attributes on the tag).
fn all<'a>(xml: &'a str, tag: &str) -> Vec<&'a str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    xml.match_indices(&open)
        .filter_map(|(i, _)| {
            let from = i + open.len();
            xml[from..].find(&close).map(|e| &xml[from..from + e])
        })
        .collect()
}

fn is_rfc3339(s: &str) -> bool {
    let b = s.as_bytes();
    s.len() == 20
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b[19] == b'Z'
        && [0..4, 5..7, 8..10, 11..13, 14..16, 17..19]
            .iter()
            .all(|r| s[r.clone()].bytes().all(|c| c.is_ascii_digit()))
}

fn is_rfc822(s: &str) -> bool {
    // "Fri, 02 Oct 2026 12:00:00 +0000"
    let p: Vec<&str> = s.split(' ').collect();
    p.len() == 6
        && ["Mon,", "Tue,", "Wed,", "Thu,", "Fri,", "Sat,", "Sun,"].contains(&p[0])
        && p[1].len() == 2
        && [
            "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
        ]
        .contains(&p[2])
        && p[3].len() == 4
        && p[4].len() == 8
        && p[5] == "+0000"
}

#[test]
fn the_checker_catches_broken_xml() {
    let decl = "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n";
    assert!(well_formed(&format!("{decl}<a><b>x &amp; y</b></a>")).is_ok());
    for bad in [
        "<a><b></a>",
        "<a>x & y</a>",
        "<a>&nbsp;</a>",
        "<a x=1></a>",
        "<a></a><b></b>",
        "<a>",
    ] {
        assert!(well_formed(&format!("{decl}{bad}")).is_err(), "{bad}");
    }
}

#[test]
fn atom_feed_follows_rfc_4287() {
    let (status, ct, xml) = get_typed("/feed.xml");
    assert_eq!(status, 200);
    assert_eq!(ct, "application/atom+xml; charset=utf-8");
    well_formed(&xml).unwrap();
    assert!(xml.contains("<feed xmlns=\"http://www.w3.org/2005/Atom\" xml:lang=\"en\">"));
    assert!(xml.contains(
        "<link href=\"https://blog.test/feed.xml\" rel=\"self\" type=\"application/atom+xml\"/>"
    ));
    assert!(
        xml.contains("<author><name>"),
        "feed-level author covers every entry"
    );
    let entries = xml.matches("<entry>").count();
    assert!(entries >= 7);
    // Required per entry: id, title, updated; ids are absolute and unique.
    let ids = all(&xml, "id");
    assert_eq!(ids.len(), entries + 1);
    assert!(ids.iter().all(|i| i.starts_with("https://blog.test/")));
    let mut unique = ids.clone();
    unique.dedup();
    assert_eq!(unique.len(), ids.len());
    assert_eq!(all(&xml, "title").len(), entries + 1);
    for d in all(&xml, "updated")
        .into_iter()
        .chain(all(&xml, "published"))
    {
        assert!(is_rfc3339(d), "{d}");
    }
    // Full content, escaped once, with absolute links.
    assert!(xml.contains("&lt;div class=&quot;code&quot; data-lang=&quot;rust&quot;&gt;"));
    assert!(!xml.contains("href=&quot;/"), "relative links in content");
}

#[test]
fn rss_feed_follows_rss_2_0() {
    let (status, ct, xml) = get_typed("/rss.xml");
    assert_eq!(status, 200);
    assert_eq!(ct, "application/rss+xml; charset=utf-8");
    well_formed(&xml).unwrap();
    assert!(xml.contains("<rss version=\"2.0\""));
    for required in ["title", "link", "description"] {
        assert!(!all(&xml, required).is_empty(), "channel {required}");
    }
    assert!(xml.contains(
        "<atom:link href=\"https://blog.test/rss.xml\" rel=\"self\" type=\"application/rss+xml\"/>"
    ));
    let items = xml.matches("<item>").count();
    assert!(items >= 7);
    assert_eq!(
        xml.matches("<guid isPermaLink=\"true\">https://blog.test/")
            .count(),
        items
    );
    let dates = all(&xml, "pubDate");
    assert_eq!(dates.len(), items);
    assert!(dates.iter().all(|d| is_rfc822(d)), "{dates:?}");
    assert!(is_rfc822(all(&xml, "lastBuildDate")[0]));
}

#[test]
fn sitemap_lists_every_public_page_with_absolute_urls() {
    let (status, ct, xml) = get_typed("/sitemap.xml");
    assert_eq!(status, 200);
    assert_eq!(ct, "application/xml; charset=utf-8");
    well_formed(&xml).unwrap();
    let locs = all(&xml, "loc");
    assert!(locs.iter().all(|l| l.starts_with("https://blog.test/")));
    for page in [
        "https://blog.test/",
        "https://blog.test/reading-on-screens",
        "https://blog.test/tag/tools",
        "https://blog.test/archive/2025",
    ] {
        assert!(locs.contains(&page), "{page}");
    }
    assert!(all(&xml, "lastmod").iter().all(|d| is_rfc3339(d)));
}

#[test]
fn robots_and_llms_txt() {
    let (status, ct, robots) = get_typed("/robots.txt");
    assert_eq!((status, ct), (200, "text/plain; charset=utf-8"));
    assert!(robots.contains("Sitemap: https://blog.test/sitemap.xml\n"));
    assert!(robots.contains("Disallow: /admin/\n"));

    let (status, _, llms) = get_typed("/llms.txt");
    assert_eq!(status, 200);
    assert!(llms.starts_with("# Plum Notes\n\n> "));
    assert!(
        llms.contains("\n## Posts\n\n"),
        "heading from the core locale"
    );
    assert!(
        llms.contains(
            "- [Reading on screens, done properly](https://blog.test/reading-on-screens)"
        )
    );
}

#[test]
fn feeds_per_language_and_one_url_each() {
    // The default language lives at the root only; a language with no posts has no feed.
    assert_eq!(get_typed("/en/feed.xml").0, 404);
    assert_eq!(get_typed("/fr/rss.xml").0, 404);
    assert_eq!(
        get_typed("/vi/rss.xml").0,
        200,
        "the sample has one vi post"
    );
    assert_eq!(get_typed("/feed.json").0, 200);
}

/// The sample site plus a Vietnamese version of one post.
fn translated() -> Fixture {
    Fixture::new(|posts, _| {
        let en = posts
            .iter_mut()
            .find(|p| p.slug == "reading-on-screens")
            .unwrap();
        en.translations = vec![("vi".into(), "doc-tren-man-hinh".into())];
        let vi = StoredPost {
            slug: "doc-tren-man-hinh".into(),
            title: "Doc tren man hinh".into(),
            lang: "vi".into(),
            translations: vec![("en".into(), "reading-on-screens".into())],
            ..en.clone()
        };
        posts.push(vi);
    })
}

#[test]
fn each_feed_has_its_own_id_and_pages_link_their_language_feeds() {
    let src = translated();
    let get = |path| get_from(&src, path);
    let (_, _, en) = get("/feed.xml");
    assert_eq!(all(&en, "id")[0], "https://blog.test/feed.xml");
    assert!(en.contains("<link href=\"https://blog.test/\" rel=\"alternate\""));
    let (status, _, vi) = get("/vi/feed.xml");
    assert_eq!(status, 200);
    well_formed(&vi).unwrap();
    assert!(vi.contains("xml:lang=\"vi\""));
    assert_eq!(all(&vi, "id")[0], "https://blog.test/vi/feed.xml");
    assert_eq!(vi.matches("<entry>").count(), 1);

    let (_, _, vi_page) = get("/vi/doc-tren-man-hinh");
    assert!(vi_page.starts_with("<!doctype html>\n<html lang=\"vi\""));
    for feed in ["/vi/feed.xml", "/vi/rss.xml", "/vi/feed.json"] {
        assert!(vi_page.contains(&format!("href=\"{feed}\"")), "{feed}");
    }
    let (_, _, en_page) = get("/reading-on-screens");
    assert!(
        en_page.contains("type=\"application/atom+xml\" title=\"Plum Notes\" href=\"/feed.xml\"")
    );
    assert!(!en_page.contains("/vi/feed"));
}

#[test]
fn x_default_points_at_the_default_language_version() {
    let src = translated();
    let x_default = "hreflang=\"x-default\" href=\"https://blog.test/reading-on-screens\"";
    for path in ["/reading-on-screens", "/vi/doc-tren-man-hinh"] {
        let (_, _, html) = get_from(&src, path);
        assert!(html.contains(x_default), "{path}");
    }
    let (_, _, sitemap) = get_from(&src, "/sitemap.xml");
    well_formed(&sitemap).unwrap();
    // Both versions carry the whole set: vi, en and x-default.
    assert_eq!(sitemap.matches(x_default).count(), 2);
    assert_eq!(sitemap.matches("hreflang=\"vi\"").count(), 2);
}

const PUBLIC_PATHS: [&str; 11] = [
    "/",
    "/reading-on-screens",
    "/tag/tools",
    "/archive/2025",
    "/feed.xml",
    "/rss.xml",
    "/feed.json",
    "/sitemap.xml",
    "/robots.txt",
    "/llms.txt",
    "/no-such-page",
];

#[test]
fn public_urls_use_the_configured_origin() {
    // `render` takes no request origin at all, so a Host header cannot reach these.
    let src = Fixture::new(|_, s| s.origin = Some("https://blog.example/".into()));
    for path in PUBLIC_PATHS {
        let (_, _, body) = get_from(&src, path);
        assert!(body.contains("https://blog.example/"), "{path}");
        assert!(!body.contains("blog.test"), "{path}: sample origin leaked");
        assert!(
            !body.contains("blog.example//"),
            "{path}: trailing slash kept"
        );
    }
    let (_, _, home) = get_from(&src, "/");
    assert!(home.contains("<link rel=\"canonical\" href=\"https://blog.example/\">"));
}

#[test]
fn no_configured_origin_gives_one_fixed_origin_everywhere() {
    for unset in [None, Some(String::new()), Some("/".to_owned())] {
        let src = Fixture::new(|_, s| s.origin = unset);
        for path in PUBLIC_PATHS {
            let (_, _, body) = get_from(&src, path);
            assert!(
                body.contains(&format!("{}/", super::UNSET_ORIGIN)),
                "{path}"
            );
            assert!(!body.contains("blog.test"), "{path}");
        }
    }
}

#[test]
fn an_empty_site_is_dated_by_its_own_timestamp() {
    let src = Fixture::new(|posts, _| posts.clear());
    let (status, _, atom) = get_from(&src, "/feed.xml");
    assert_eq!(status, 200);
    assert!(
        atom.contains("<updated>2026-09-01T12:00:00Z</updated>"),
        "{atom}"
    );
    let (_, _, rss) = get_from(&src, "/rss.xml");
    let built = all(&rss, "lastBuildDate");
    assert!(
        built[0].contains("01 Sep 2026") && is_rfc822(built[0]),
        "{rss}"
    );
    let (_, _, sitemap) = get_from(&src, "/sitemap.xml");
    // No post, so no page date to state: the home entry has no lastmod.
    assert!(!sitemap.contains("<lastmod>"), "{sitemap}");
    for path in ["/feed.xml", "/rss.xml", "/feed.json", "/sitemap.xml"] {
        let (_, _, body) = get_from(&src, path);
        assert!(!body.contains("1970"), "{path} has an epoch date");
    }
}

#[test]
fn editing_a_post_keeps_its_atom_entry_id() {
    let ids = |src: &Fixture| {
        let (_, _, xml) = get_from(src, "/feed.xml");
        all(&xml, "id")
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>()
    };
    let before = ids(&Fixture::new(|_, _| {}));
    let after = ids(&Fixture::new(|posts, _| {
        let p = posts
            .iter_mut()
            .find(|p| p.slug == "reading-on-screens")
            .unwrap();
        p.title = "A new title".into();
        p.body.html = "<p>Rewritten.</p>".into();
        p.excerpt = Some("New summary".into());
    }));
    assert_eq!(before, after);
    assert!(after.contains(&"https://blog.test/reading-on-screens".to_owned()));
}

#[test]
fn hostile_titles_still_parse_as_xml() {
    // C0 controls, the XML 1.0 non-characters U+FFFE/U+FFFF, a CDATA end, markup and emoji.
    let hostile = "a\u{1}\u{8}\u{1B}b \u{FFFE}\u{FFFF}]]> <i>&amp;</i> \u{1F600}";
    let clean = "ab ]]> <i>&amp;</i> \u{1F600}";
    let src = Fixture::new(|posts, _| {
        let p = &mut posts[0];
        p.title = hostile.into();
        p.excerpt = Some(hostile.into());
        p.tags[0].name = hostile.into();
        p.lang = "vi".into();
    });
    for path in ["/vi/feed.xml", "/vi/rss.xml", "/feed.xml", "/sitemap.xml"] {
        let (status, _, xml) = get_from(&src, path);
        assert_eq!(status, 200, "{path}");
        let doc = roxmltree::Document::parse(&xml).unwrap_or_else(|e| panic!("{path}: {e}"));
        if path.starts_with("/vi/") {
            let item = if path.ends_with("rss.xml") {
                "item"
            } else {
                "entry"
            };
            let titles: Vec<&str> = doc
                .descendants()
                .filter(|n| {
                    n.has_tag_name("title") && n.parent().is_some_and(|p| p.has_tag_name(item))
                })
                .filter_map(|n| n.text())
                .collect();
            assert_eq!(titles, [clean], "{path}");
            let tags: Vec<String> = doc
                .descendants()
                .filter(|n| n.has_tag_name("category"))
                .filter_map(|n| n.attribute("term").or(n.text()).map(str::to_owned))
                .collect();
            assert!(tags.iter().any(|t| t == clean), "{path}: {tags:?}");
        }
    }
}
