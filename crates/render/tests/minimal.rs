//! Renders every page kind with the real minimal theme, so a template or locale change that
//! breaks a page fails here before it reaches the Worker.

use plumblog_markdown::Rendered;
use plumblog_render::{Body, Link, PageInfo, Post, Site, Theme, ThemeFiles};
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
        home_url: "/".into(),
        search_url: None,
        feed_url: "/feed.xml".into(),
        icon_url: "data:,".into(),
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
            toc: vec![],
            excerpt: "Some words here and there for the excerpt".into(),
        },
        tags: vec![Link {
            name: "rust".into(),
            url: "/tag/rust".into(),
        }],
        cover: None,
        thumb: None,
    }
}

fn page(lang: &str) -> PageInfo {
    PageInfo {
        lang: lang.into(),
        canonical: "https://example.com/".into(),
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
