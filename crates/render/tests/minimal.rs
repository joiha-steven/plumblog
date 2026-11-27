//! Renders every page kind with the real minimal theme, so a template or locale change that
//! breaks a page fails here before it reaches the Worker.

use plumblog_markdown::Rendered;
use plumblog_render::{Alternate, Body, Image, Link, PageInfo, Post, Site, Theme, ThemeFiles};
use std::collections::BTreeMap;
use std::path::Path;

fn theme_files() -> Vec<(String, String)> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../themes/minimal");
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(text) = std::fs::read_to_string(&p) {
                let rel = p
                    .strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, text));
            }
        }
    }
    out
}

fn load(files: &[(String, String)], chosen: &BTreeMap<String, toml::Value>) -> Theme {
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    Theme::load(&refs.as_slice(), None, chosen, "/theme/assets").unwrap()
}

fn site() -> Site {
    Site {
        title: "Plum Notes".into(),
        description: "A test blog.".into(),
        default_lang: "en".into(),
        home_url: "/".into(),
        search_url: None,
        feed_url: "/feed.xml".into(),
        rss_url: "/rss.xml".into(),
        json_feed_url: "/feed.json".into(),
        icon_url: "data:,".into(),
        image_url: None,
        author: None,
        footer: "Footer".into(),
        nav: vec![],
    }
}

fn post(slug: &str, title: &str, at: i64, pinned: bool) -> Post {
    Post {
        url: format!("/{slug}"),
        title: title.into(),
        lang: "en".into(),
        published_at: at,
        pinned,
        excerpt: None,
        body: Rendered {
            html: "<p>Some words <em>here</em> &amp; there.</p>".into(),
            excerpt: "Some words here and there for the excerpt".into(),
            reading_minutes: 1,
            ..Rendered::default()
        },
        tags: vec![Link {
            name: "rust".into(),
            url: "/tag/rust".into(),
        }],
        cover: None,
        thumb: None,
        translations: vec![],
    }
}

fn page(lang: &str) -> PageInfo {
    PageInfo {
        lang: lang.into(),
        canonical: "https://example.com/".into(),
        origin: "https://example.com".into(),
    }
}

#[test]
fn every_page_kind_renders() {
    let files = theme_files();
    let theme = load(&files, &BTreeMap::new());
    let a = post("first", "First <post>", 1_772_668_800, false);
    let b = post("second", "Second", 1_740_000_000, true);
    let posts = [&a, &b];

    let home = theme
        .render(
            &site(),
            &page("en"),
            &Body::Home {
                posts: &posts,
                older_url: Some("/page/2".into()),
                newer_url: None,
            },
        )
        .unwrap();
    assert!(home.starts_with("<!doctype html>"));
    assert!(home.contains("<title>Plum Notes</title>"));
    assert!(home.contains("First &lt;post&gt;"), "titles are escaped");
    assert!(home.contains("Mar 5, 2026"), "date uses the theme locale");
    assert!(home.contains("href=\"/page/2\" rel=\"next\">Older posts"));
    assert!(home.contains("class=\"posts pinned\""));
    assert!(home.contains("/theme/assets/fonts/inter-latin.woff2"));
    assert!(home.contains("--c-accent:light-dark(#8a2d6f,#e2a3d2)"));
    assert!(!home.contains("{{"), "no unrendered template syntax");

    let html = theme
        .render(
            &site(),
            &page("en"),
            &Body::Post {
                post: &a,
                prev: Some(&b),
                next: None,
            },
        )
        .unwrap();
    assert!(html.contains("<title>First &lt;post&gt; - Plum Notes</title>"));
    assert!(
        html.contains("<p>Some words <em>here</em> &amp; there.</p>"),
        "post HTML is not re-escaped"
    );
    assert!(html.contains("1 min read"));
    assert!(html.contains("href=\"/tag/rust\" rel=\"tag\">#rust"));
    assert!(html.contains("Some words here and there for the excerpt"));

    let tag = Link {
        name: "rust".into(),
        url: "/tag/rust".into(),
    };
    let html = theme
        .render(
            &site(),
            &page("en"),
            &Body::Tag {
                tag: &tag,
                posts: &posts,
                older_url: None,
                newer_url: None,
            },
        )
        .unwrap();
    assert!(html.contains("<h1 class=\"list-title\">#rust</h1>"));

    let years = vec![
        Link {
            name: "2026".into(),
            url: "/archive/2026".into(),
        },
        Link {
            name: "2025".into(),
            url: "/archive/2025".into(),
        },
    ];
    let html = theme
        .render(
            &site(),
            &page("en"),
            &Body::Archive {
                year: 2026,
                posts: &posts[..1],
                years,
                older_url: Some("/archive/2026/page/2".into()),
                newer_url: None,
            },
        )
        .unwrap();
    assert!(html.contains("Posts from 2026"));
    assert!(html.contains("href=\"/archive/2026\" aria-current=\"page\""));
    assert!(html.contains("href=\"/archive/2026/page/2\" rel=\"next\">Older posts"));

    let html = theme
        .render(&site(), &page("vi"), &Body::NotFound { recent: &posts })
        .unwrap();
    assert!(html.contains("<html lang=\"vi\""));
    assert!(html.contains("Kh\u{f4}ng t\u{ec}m th\u{1ea5}y trang"));
    assert!(html.contains("noindex"));
    // Fonts for the page language are preloaded too.
    assert!(html.contains("inter-vietnamese.woff2\" as=\"font\""));
}

#[test]
fn options_come_from_declarations_and_owner_values() {
    let files = theme_files();
    let defaults = load(&files, &BTreeMap::new());
    let mut chosen = BTreeMap::new();
    let mut accent = toml::Table::new();
    accent.insert("light".into(), "#123456".into());
    accent.insert("dark".into(), "#abcdef".into());
    chosen.insert("accent".to_owned(), toml::Value::Table(accent));
    chosen.insert("excerpt_words".to_owned(), toml::Value::Integer(2));
    // Not a declared choice: falls back to the default style instead of breaking the site.
    chosen.insert("style".to_owned(), toml::Value::String("gone".into()));
    let custom = load(&files, &chosen);
    assert!(
        custom
            .css()
            .contains("--c-accent:light-dark(#123456,#abcdef)")
    );
    assert_eq!(
        defaults.css(),
        custom.css().replace("#123456,#abcdef", "#8a2d6f,#e2a3d2")
    );

    let a = post("first", "First", 0, false);
    let html = custom
        .render(
            &site(),
            &page("en"),
            &Body::Home {
                posts: &[&a],
                older_url: None,
                newer_url: None,
            },
        )
        .unwrap();
    assert!(html.contains("<p class=\"excerpt\">Some words\u{2026}</p>"));
}

#[test]
fn inlined_css_fits_the_budget() {
    let files = theme_files();
    let theme = load(&files, &BTreeMap::new());
    let len = theme.css().len();
    println!("minimal inlined css: {len} bytes");
    assert!(!theme.css().contains("/*"), "comments are stripped");
    // Budgets: reader page CSS <= 15 KB is measured compressed, as it travels.
    // The raw cap keeps growth visible in review.
    assert!(len < 40_000, "inlined css grew to {len} bytes");
}

#[test]
fn partial_theme_falls_back_and_bad_slots_fail() {
    let files = theme_files();
    let default: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let default = default.as_slice();
    let manifest = default.read("theme.toml").unwrap();
    let style = default.read("style.css").unwrap();
    // A theme that overrides only the 404 page and one locale key.
    let child = [
        ("theme.toml", manifest),
        ("style.css", style),
        ("styles/writer.css", ""),
        ("assets/fonts/faces.css", ""),
        (
            "templates/404.html",
            "{% extends \"base.html\" %}{% block main %}<p>{{ t(\"notfound.title\") }}</p>{% endblock %}",
        ),
        ("locales/en.toml", "[notfound]\ntitle = \"Lost\"\n"),
    ];
    let theme = Theme::load(&&child[..], Some(&default), &BTreeMap::new(), "/a").unwrap();
    let html = theme
        .render(&site(), &page("en"), &Body::NotFound { recent: &[] })
        .unwrap();
    assert!(html.contains("<p>Lost</p>"), "own template and key win");
    assert!(
        html.contains("Skip to content"),
        "base.html and its keys come from the default"
    );

    let broken = [
        ("theme.toml", manifest),
        ("style.css", style),
        ("styles/writer.css", ""),
        ("assets/fonts/faces.css", ""),
        ("templates/404.html", "{{ slot(\"sidebar\") }}"),
    ];
    let theme = Theme::load(&&broken[..], None, &BTreeMap::new(), "/a").unwrap();
    let err = theme
        .render(&site(), &page("en"), &Body::NotFound { recent: &[] })
        .unwrap_err();
    assert!(err.detail.contains("unknown slot"), "{}", err.detail);
    assert!(
        err.detail.contains("404.html"),
        "error names the template: {}",
        err.detail
    );
}

#[test]
fn highlighter_loads_only_on_posts_with_code() {
    let files = theme_files();
    let theme = load(&files, &BTreeMap::new());
    let mut p = post("x", "X", 0, false);
    let render = |p: &Post| {
        theme
            .render(
                &site(),
                &page("en"),
                &Body::Post {
                    post: p,
                    prev: None,
                    next: None,
                },
            )
            .unwrap()
    };
    assert!(!render(&p).contains("highlight.js"));
    p.body.has_code = true;
    assert!(render(&p).contains("<script src=\"/theme/assets/highlight.js\" defer></script>"));
}

#[test]
fn post_flags_reach_templates() {
    let files = theme_files();
    let default: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let default = default.as_slice();
    let child = [
        ("theme.toml", default.read("theme.toml").unwrap()),
        ("style.css", ""),
        ("styles/writer.css", ""),
        ("assets/fonts/faces.css", ""),
        (
            "templates/post.html",
            "{{ post.has_code }} {{ post.has_math }} {{ post.has_embed }} {{ post.reading_minutes }}",
        ),
    ];
    let theme = Theme::load(&&child[..], Some(&default), &BTreeMap::new(), "/a").unwrap();
    let mut p = post("x", "X", 0, false);
    p.body.has_code = true;
    p.body.reading_minutes = 7;
    let html = theme
        .render(
            &site(),
            &page("en"),
            &Body::Post {
                post: &p,
                prev: None,
                next: None,
            },
        )
        .unwrap();
    assert_eq!(html, "True False False 7");
}

/// A theme following the documented plural format shows the right form per language, and
/// a key found nowhere is reported for the Worker to log.
#[test]
fn plural_messages_and_missing_keys_in_theme_locales() {
    let mut files = theme_files();
    for (path, text) in &mut files {
        match path.as_str() {
            "locales/en.toml" => text.push_str(
                "\n[test]\nposts = { one = \"{count} post\", other = \"{count} posts\" }\n",
            ),
            "templates/home.html" => {
                // Inside the last block: text outside blocks of a child template is ignored.
                let at = text.rfind("{% endblock %}").unwrap();
                text.insert_str(at, "<p id=\"plural\">{{ t(\"test.posts\", count=1) }}|{{ t(\"test.posts\", count=3) }}|{{ t(\"test.posts\", count=5) }}|{{ t(\"test.posts\", count=21) }}|{{ t(\"test.gone\") }}</p>");
            }
            _ => {}
        }
    }
    files.push((
        "locales/ru.toml".into(),
        "[test]\nposts = { one = \"{count} \u{437}\u{430}\u{43f}\u{438}\u{441}\u{44c}\", few = \"{count} \u{437}\u{430}\u{43f}\u{438}\u{441}\u{438}\", many = \"{count} \u{437}\u{430}\u{43f}\u{438}\u{441}\u{435}\u{439}\", other = \"{count} \u{437}\u{430}\u{43f}\u{438}\u{441}\u{438}\" }\n".into(),
    ));
    let theme = load(&files, &BTreeMap::new());
    let render = |lang: &str| {
        theme
            .render_reporting(
                &site(),
                &page(lang),
                &Body::Home {
                    posts: &[],
                    older_url: None,
                    newer_url: None,
                },
            )
            .unwrap()
    };
    let line = |(html, _): &(String, Vec<(String, String)>)| {
        let start = html.find("<p id=\"plural\">").unwrap() + 15;
        html[start..start + html[start..].find("</p>").unwrap()].to_owned()
    };
    let en = render("en");
    assert_eq!(line(&en), "1 post|3 posts|5 posts|21 posts|test.gone");
    // Missing keys come back with the page, once each, and do not leak into the next render.
    assert_eq!(en.1, [("en".to_owned(), "test.gone".to_owned())]);
    let ru = render("ru");
    assert_eq!(
        line(&ru),
        "1 \u{437}\u{430}\u{43f}\u{438}\u{441}\u{44c}|3 \u{437}\u{430}\u{43f}\u{438}\u{441}\u{438}|5 \u{437}\u{430}\u{43f}\u{438}\u{441}\u{435}\u{439}|21 \u{437}\u{430}\u{43f}\u{438}\u{441}\u{44c}|test.gone"
    );
    assert_eq!(ru.1, [("ru".to_owned(), "test.gone".to_owned())]);
}

#[test]
fn post_seo_uses_cover_translations_and_absolute_urls() {
    let files = theme_files();
    let theme = load(&files, &BTreeMap::new());
    let mut p = post("hello", "Hello", 1_790_942_400, false);
    p.cover = Some(Image {
        src: "/media/cover.avif".into(),
        srcset: None,
        width: 1200,
        height: 630,
        alt: String::new(),
    });
    p.translations = vec![Alternate {
        lang: "vi".into(),
        url: "/vi/xin-chao".into(),
    }];
    let html = theme
        .render(
            &site(),
            &page("en"),
            &Body::Post {
                post: &p,
                prev: None,
                next: None,
            },
        )
        .unwrap();
    assert!(html.contains("<meta property=\"og:type\" content=\"article\">"));
    assert!(
        html.contains(
            "<meta property=\"og:image\" content=\"https://example.com/media/cover.avif\">"
        )
    );
    assert!(html.contains("<meta name=\"twitter:card\" content=\"summary_large_image\">"));
    assert!(
        html.contains(
            "<meta property=\"article:published_time\" content=\"2026-10-02T12:00:00Z\">"
        )
    );
    // hreflang lists every version, this one included.
    assert!(
        html.contains(
            "<link rel=\"alternate\" hreflang=\"en\" href=\"https://example.com/hello\">"
        )
    );
    assert!(html.contains(
        "<link rel=\"alternate\" hreflang=\"vi\" href=\"https://example.com/vi/xin-chao\">"
    ));
    // The default language's version is also the fallback for every other language.
    assert!(html.contains(
        "<link rel=\"alternate\" hreflang=\"x-default\" href=\"https://example.com/hello\">"
    ));
    assert!(html.contains("\"image\":[\"https://example.com/media/cover.avif\"]"));
    assert!(html.contains("\"@type\":\"BlogPosting\""));
    assert!(html.contains("<meta property=\"og:locale\" content=\"en_US\">"));
    let br = theme
        .render(
            &site(),
            &page("pt-br"),
            &Body::Home {
                posts: &[],
                older_url: None,
                newer_url: None,
            },
        )
        .unwrap();
    assert!(br.contains("<meta property=\"og:locale\" content=\"pt_BR\">"));
    // No known territory: no tag rather than a wrong one.
    let unknown = theme
        .render(
            &site(),
            &page("tlh"),
            &Body::Home {
                posts: &[],
                older_url: None,
                newer_url: None,
            },
        )
        .unwrap();
    assert!(!unknown.contains("og:locale"));
}

#[test]
fn json_ld_cannot_close_its_script_element() {
    let files = theme_files();
    let theme = load(&files, &BTreeMap::new());
    let p = post("x", "</script><script>alert(1)</script>", 0, false);
    let html = theme
        .render(
            &site(),
            &page("en"),
            &Body::Post {
                post: &p,
                prev: None,
                next: None,
            },
        )
        .unwrap();
    let ld = html
        .split("<script type=\"application/ld+json\">")
        .nth(1)
        .unwrap();
    let ld = &ld[..ld.find("</script>").unwrap()];
    assert!(
        !ld.contains('<'),
        "JSON-LD escapes < so the title cannot end the script: {ld}"
    );
    assert!(ld.contains("\\u003c/script\\u003e"));
}
