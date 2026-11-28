//! Tag and series pages are per language (issue #132): `/tag/<slug>` and `/series/<slug>` list
//! the default language, `/<lang>/tag/<slug>` and `/<lang>/series/<slug>` one other site
//! language. Over the in-memory store; tools/worker-test/series.test.mjs runs the same on D1.

use super::db::Db;
use super::tests_support::run;
use super::{Outcome, render, seed};
use plumblog_model::{SeriesId, TagId};
use plumblog_store::memory::MemoryStore;
use plumblog_store::{
    NewSeries, PostKind, PostStatus, PostStore, SavePost, SettingsStore, TaxonomyStore,
};

fn page(db: &MemoryStore, path: &str, q: &str) -> (u16, String, bool) {
    match run(render(&Db(db), &Db(db), &Db(db), path, q)).unwrap() {
        Outcome::Page {
            status,
            body,
            cacheable,
            ..
        } => (status, body, cacheable),
        Outcome::Redirect(to) => (301, to, true),
    }
}

fn get(db: &MemoryStore, path: &str) -> (u16, String) {
    let (s, b, _) = page(db, path, "");
    (s, b)
}

/// Sample site settings (en default, vi listed, 5 per page, origin blog.test), no posts.
fn site() -> MemoryStore {
    let db = MemoryStore::default();
    run(seed::sample_posts(&db)).unwrap();
    db
}

struct Add {
    lang: String,
    slug: String,
    at: i64,
    tags: Vec<TagId>,
    series: Option<(SeriesId, Option<i64>)>,
    status: PostStatus,
}

fn add(db: &MemoryStore, a: Add) {
    let author = run(plumblog_store::UserStore::list_users(db)).unwrap()[0].id;
    run(db.save_post(SavePost {
        id: None,
        expected_version: None,
        translation_group: None,
        kind: PostKind::Post,
        lang: a.lang,
        slug: a.slug.clone(),
        status: a.status,
        published_at: (a.status == PostStatus::Published).then_some(a.at),
        author_id: author,
        title: format!("Title {}", a.slug),
        markdown: String::new(),
        html: "<p>x</p>".to_owned(),
        toc_json: "[]".to_owned(),
        excerpt: String::new(),
        word_count: 1,
        reading_minutes: 1,
        pinned: false,
        comments_enabled: false,
        cover_media_id: None,
        series_id: a.series.map(|s| s.0),
        series_position: a.series.and_then(|s| s.1),
        tag_ids: a.tags,
        media_ids: Vec::new(),
        search_text: String::new(),
        revision: None,
        client_id: None,
        now: a.at,
    }))
    .unwrap();
}

fn post(lang: &str, slug: &str, at: i64) -> Add {
    Add {
        lang: lang.to_owned(),
        slug: slug.to_owned(),
        at,
        tags: Vec::new(),
        series: None,
        status: PostStatus::Published,
    }
}

const T0: i64 = 1_900_000_000_000;

/// Tag `plum` on 2 en and 7 vi posts (two vi pages), tag `vi-only` on one vi post; series
/// `journey` ("Rust Journey", a description) with en posts at positions 2 and 1 and vi
/// posts at 3, 1, and 2 twice; series `en-only` has an en post only.
fn fixture() -> MemoryStore {
    let db = site();
    let plum = run(db.ensure_tag("plum", "Plum")).unwrap();
    let vi_only = run(db.ensure_tag("vi-only", "Vi only")).unwrap();
    let journey = run(db.create_series(NewSeries {
        slug: "journey".into(),
        title: "Rust Journey".into(),
        description: "A long way.".into(),
    }))
    .unwrap();
    let en_only = run(db.create_series(NewSeries {
        slug: "en-only".into(),
        title: "English Only".into(),
        description: String::new(),
    }))
    .unwrap();
    let tagged = |lang: &str, slug: &str, at, series| Add {
        tags: vec![plum],
        series,
        ..post(lang, slug, at)
    };
    add(&db, tagged("en", "e1", T0, Some((journey, Some(2)))));
    add(&db, tagged("en", "e2", T0 + 1000, Some((journey, Some(1)))));
    for i in 0..7 {
        let slug = format!("v{i}");
        add(&db, tagged("vi", &slug, T0 + i * 1000, None));
    }
    // Series posts in vi: date order is the reverse of reading order, and v-b/v-c tie.
    add(&db, tagged("vi", "s-c", T0 + 100, Some((journey, Some(3)))));
    add(&db, tagged("vi", "s-a", T0 + 300, Some((journey, Some(1)))));
    add(&db, tagged("vi", "s-b", T0 + 200, Some((journey, Some(2)))));
    add(
        &db,
        tagged("vi", "s-bb", T0 + 250, Some((journey, Some(2)))),
    );
    add(
        &db,
        Add {
            tags: vec![vi_only],
            ..post("vi", "lonely", T0)
        },
    );
    add(
        &db,
        Add {
            series: Some((en_only, Some(1))),
            ..post("en", "e-solo", T0)
        },
    );
    // Never listed: a draft in the series and tag.
    add(
        &db,
        Add {
            tags: vec![plum],
            series: Some((journey, Some(0))),
            status: PostStatus::Draft,
            ..post("vi", "draft", T0)
        },
    );
    db
}

/// The post links of a listing, in order.
fn links(html: &str) -> Vec<String> {
    let Some(list) = html.split("<ol class=\"posts\">").nth(1) else {
        return Vec::new();
    };
    let list = &list[..list.find("</ol>").unwrap()];
    list.split("<a class=\"entry")
        .skip(1)
        .map(|s| {
            let h = &s[s.find("href=\"").unwrap() + 6..];
            h[..h.find('"').unwrap()].to_owned()
        })
        .collect()
}

#[test]
fn a_tag_page_lists_one_language() {
    let db = fixture();
    let (status, en) = get(&db, "/tag/plum");
    assert_eq!(status, 200, "{en}");
    assert_eq!(
        links(&en),
        ["/e2", "/e1"],
        "default language only, newest first"
    );
    assert!(en.contains("<html lang=\"en\""));
    assert!(en.contains("<link rel=\"canonical\" href=\"https://blog.test/tag/plum\">"));

    let (status, vi) = get(&db, "/vi/tag/plum");
    assert_eq!(status, 200, "{vi}");
    assert!(vi.contains("<html lang=\"vi\""));
    assert!(vi.contains("<link rel=\"canonical\" href=\"https://blog.test/vi/tag/plum\">"));
    let l = links(&vi);
    assert_eq!(l.len(), 5, "page size");
    assert!(l.iter().all(|h| h.starts_with("/vi/")), "{l:?}");
    assert!(!l.contains(&"/vi/draft".to_owned()));
    assert!(vi.contains("href=\"/vi/tag/plum/page/2\" rel=\"next\""));
    let p2 = get(&db, "/vi/tag/plum/page/2").1;
    assert_eq!(links(&p2).len(), 5, "11 vi posts: 5, 5, 1");
    assert!(p2.contains("href=\"/vi/tag/plum\" rel=\"prev\""));
    assert!(
        !p2.contains("rel=\"alternate\" hreflang"),
        "alternates on page 1 only"
    );
    assert_eq!(links(&get(&db, "/vi/tag/plum/page/3").1).len(), 1);
    assert_eq!(get(&db, "/vi/tag/plum/page/4").0, 404);
    assert_eq!(get(&db, "/tag/plum/page/2").0, 404, "en has one page");
}

#[test]
fn tag_pages_link_their_language_versions() {
    let db = fixture();
    for path in ["/tag/plum", "/vi/tag/plum"] {
        let html = get(&db, path).1;
        assert!(
            html.contains("hreflang=\"en\" href=\"https://blog.test/tag/plum\""),
            "{path}"
        );
        assert!(
            html.contains("hreflang=\"vi\" href=\"https://blog.test/vi/tag/plum\""),
            "{path}"
        );
        assert!(
            html.contains("hreflang=\"x-default\" href=\"https://blog.test/tag/plum\""),
            "{path}"
        );
    }
    // A tag in one language alone has no alternates, and no page in the other.
    let lonely = get(&db, "/vi/tag/vi-only");
    assert_eq!(lonely.0, 200);
    assert!(!lonely.1.contains("rel=\"alternate\" hreflang"));
    assert_eq!(
        get(&db, "/tag/vi-only").0,
        404,
        "no en post: a 404, not an empty page"
    );
    assert_eq!(get(&db, "/tag/nope").0, 404);
    assert_eq!(get(&db, "/vi/tag/nope").0, 404);
}

#[test]
fn tag_urls_have_one_form() {
    let db = fixture();
    let moved = |p: &str, to: &str| assert_eq!(get(&db, p), (301, to.to_owned()), "{p}");
    moved("/en/tag/plum", "/tag/plum");
    moved("/en/tag/plum/page/2", "/tag/plum/page/2");
    moved("/en/tag/plum/page/1", "/tag/plum");
    moved("/tag/plum/page/1", "/tag/plum");
    moved("/vi/tag/plum/page/1", "/vi/tag/plum");
    moved("/vi/tag/plum/", "/vi/tag/plum");
    moved("/tag/plum/", "/tag/plum");
    // Not a site language: nothing, as before.
    for p in [
        "/xx/tag/plum",
        "/xx/tag/plum/page/1",
        "/de/tag/plum",
        "/api/tag/plum",
    ] {
        assert_eq!(get(&db, p).0, 404, "{p}");
    }
    assert_eq!(get(&db, "/vi/tag/plum/page/0").0, 404);
    assert_eq!(
        get(&db, "/vi/tag/plum/page/1001").0,
        404,
        "past the offset cap"
    );
}

#[test]
fn a_series_page_is_in_reading_order_per_language() {
    let db = fixture();
    let (status, en) = get(&db, "/series/journey");
    assert_eq!(status, 200, "{en}");
    assert_eq!(links(&en), ["/e2", "/e1"], "position 1, then 2");
    assert!(en.contains("<h1 class=\"list-title\">Rust Journey</h1>"));
    assert!(en.contains("A long way."));
    assert!(en.contains("<title>Rust Journey - "));
    assert!(en.contains("<link rel=\"canonical\" href=\"https://blog.test/series/journey\">"));

    let (status, vi) = get(&db, "/vi/series/journey");
    assert_eq!(status, 200, "{vi}");
    // Positions 1, 2, 2, 3; the tie by publish date, oldest first; the draft is not listed.
    assert_eq!(links(&vi), ["/vi/s-a", "/vi/s-b", "/vi/s-bb", "/vi/s-c"]);
    assert!(vi.contains("<html lang=\"vi\""));
    assert!(vi.contains("<link rel=\"canonical\" href=\"https://blog.test/vi/series/journey\">"));
    for html in [&en, &vi] {
        assert!(html.contains("hreflang=\"en\" href=\"https://blog.test/series/journey\""));
        assert!(html.contains("hreflang=\"vi\" href=\"https://blog.test/vi/series/journey\""));
        assert!(html.contains("hreflang=\"x-default\" href=\"https://blog.test/series/journey\""));
    }
    // Only the default language has this one.
    let solo = get(&db, "/series/en-only");
    assert_eq!(solo.0, 200);
    assert!(!solo.1.contains("rel=\"alternate\" hreflang"));
    assert_eq!(get(&db, "/vi/series/en-only").0, 404);
    assert_eq!(get(&db, "/series/nope").0, 404);
    assert_eq!(get(&db, "/vi/series/nope").0, 404);
}

#[test]
fn series_urls_have_one_form_and_page() {
    let db = fixture();
    let moved = |p: &str, to: &str| assert_eq!(get(&db, p), (301, to.to_owned()), "{p}");
    moved("/en/series/journey", "/series/journey");
    moved("/en/series/journey/page/3", "/series/journey/page/3");
    moved("/series/journey/page/1", "/series/journey");
    moved("/vi/series/journey/page/1", "/vi/series/journey");
    moved("/vi/series/journey/", "/vi/series/journey");
    assert_eq!(get(&db, "/xx/series/journey").0, 404);
    assert_eq!(
        get(&db, "/vi/series/journey/page/2").0,
        404,
        "4 posts: one page"
    );
}

#[test]
fn a_long_series_pages_in_reading_order() {
    let db = site();
    let s = run(db.create_series(NewSeries {
        slug: "long".into(),
        title: "Long".into(),
        description: String::new(),
    }))
    .unwrap();
    for i in 0..7 {
        // Newest first by date would be the reverse of the series.
        let slug = format!("p{i}");
        add(
            &db,
            Add {
                series: Some((s, Some(i))),
                ..post("vi", &slug, T0 - i * 1000)
            },
        );
    }
    let p1 = get(&db, "/vi/series/long").1;
    assert_eq!(
        links(&p1),
        ["/vi/p0", "/vi/p1", "/vi/p2", "/vi/p3", "/vi/p4"]
    );
    assert!(p1.contains("href=\"/vi/series/long/page/2\" rel=\"next\""));
    let p2 = get(&db, "/vi/series/long/page/2").1;
    assert_eq!(links(&p2), ["/vi/p5", "/vi/p6"]);
    assert!(p2.contains("href=\"/vi/series/long\" rel=\"prev\""));
}

#[test]
fn the_sitemap_lists_each_non_empty_tag_and_series_page_per_language() {
    let db = fixture();
    let map = get(&db, "/sitemap.xml").1;
    for loc in [
        "/tag/plum",
        "/vi/tag/plum",
        "/vi/tag/vi-only",
        "/series/journey",
        "/vi/series/journey",
        "/series/en-only",
    ] {
        assert!(
            map.contains(&format!("<loc>https://blog.test{loc}</loc>")),
            "{loc}"
        );
    }
    for loc in ["/tag/vi-only", "/vi/series/en-only", "/tag/nope"] {
        assert!(
            !map.contains(&format!("<loc>https://blog.test{loc}</loc>")),
            "{loc}"
        );
    }
    let plum = &map[map.find("<loc>https://blog.test/tag/plum</loc>").unwrap()..];
    let plum = &plum[..plum.find("</url>").unwrap()];
    assert!(plum.contains("hreflang=\"vi\" href=\"https://blog.test/vi/tag/plum\""));
    assert!(plum.contains("hreflang=\"x-default\" href=\"https://blog.test/tag/plum\""));
    let lone = &map[map
        .find("<loc>https://blog.test/vi/tag/vi-only</loc>")
        .unwrap()..];
    assert!(!lone[..lone.find("</url>").unwrap()].contains("hreflang"));
}

#[test]
fn a_post_links_its_series_and_tags_in_its_own_language() {
    let db = fixture();
    let en = get(&db, "/e1").1;
    assert!(
        en.contains(
            "<p class=\"series\">Series: <a href=\"/series/journey\">Rust Journey</a> (2)</p>"
        ),
        "{en}"
    );
    assert!(en.contains("href=\"/tag/plum\" rel=\"tag\""));
    let vi = get(&db, "/vi/s-c").1;
    assert!(
        vi.contains("<a href=\"/vi/series/journey\">Rust Journey</a> (3)</p>"),
        "{vi}"
    );
    assert!(vi.contains("href=\"/vi/tag/plum\" rel=\"tag\""));
    assert!(
        !get(&db, "/vi/v0").1.contains("class=\"series\""),
        "no series, no line"
    );
}

#[test]
fn a_post_stored_as_series_stays_reachable() {
    let db = fixture();
    add(&db, post("en", "series", T0));
    let (status, html) = get(&db, "/series");
    assert_eq!(status, 200, "{html}");
    assert!(html.contains("Title series"));
    assert_eq!(
        get(&db, "/series/journey").0,
        200,
        "and /series/<slug> is the listing"
    );
    assert!(get(&db, "/series/journey").1.contains("Rust Journey"));
    assert_eq!(get(&db, "/series/"), (301, "/series".to_owned()));
    // The same slug in another language is just a post.
    add(&db, post("vi", "series", T0));
    assert!(get(&db, "/vi/series").1.contains("Title series"));
}

#[test]
fn a_post_stored_as_search_in_a_listed_language_stays_reachable() {
    let db = fixture();
    // No such post: the form, never cached.
    let (status, form, cacheable) = page(&db, "/vi/search", "");
    assert_eq!(status, 200);
    assert!(form.contains("<form"), "{form}");
    assert!(!cacheable, "the search form is never cached");

    add(&db, post("vi", "search", T0));
    let (status, html, cacheable) = page(&db, "/vi/search", "");
    assert_eq!(status, 200);
    assert!(html.contains("Title search"), "{html}");
    assert!(html.contains("<link rel=\"canonical\" href=\"https://blog.test/vi/search\">"));
    assert!(cacheable, "a stored post is cached like any post");
    // Any `q` is a search, even blank.
    for q in ["rust", " "] {
        let (status, html, _) = page(&db, "/vi/search", q);
        assert_eq!(status, 200);
        assert!(!html.contains("Title search") || q == "rust", "{q:?}");
        assert!(html.contains("noindex"), "{q:?}: a search page");
    }
    // An unlisted language keeps its old meaning: a post path.
    add(&db, post("de", "search", T0));
    assert!(get(&db, "/de/search").1.contains("Title search"));
}

#[test]
fn a_post_in_an_unlisted_language_links_the_default_languages_pages_or_none() {
    let db = fixture();
    run(db.put_setting("site_languages", "[\"en\",\"vi\",\"fr\"]", 1)).unwrap();
    let plum = run(db.ensure_tag("plum", "Plum")).unwrap();
    let fr_only = run(db.ensure_tag("fr-only", "Fr only")).unwrap();
    let journey = run(db.find_series("journey")).unwrap().unwrap().id;
    let fr_series = run(db.create_series(NewSeries {
        slug: "fr-series".into(),
        title: "Fr series".into(),
        description: String::new(),
    }))
    .unwrap();
    for (slug, series) in [("f1", journey), ("f2", fr_series)] {
        add(
            &db,
            Add {
                tags: vec![plum, fr_only],
                series: Some((series, Some(1))),
                ..post("fr", slug, T0)
            },
        );
    }
    // Listed: the tag and series pages of fr exist.
    assert!(
        get(&db, "/fr/f1")
            .1
            .contains("href=\"/fr/tag/plum\" rel=\"tag\"")
    );
    assert_eq!(get(&db, "/fr/tag/plum").0, 200);

    run(db.put_setting("site_languages", "[\"en\",\"vi\"]", 2)).unwrap();
    assert_eq!(get(&db, "/fr/tag/plum").0, 404, "fr is no longer listed");
    let html = get(&db, "/fr/f1").1;
    assert!(
        html.contains("href=\"/tag/plum\" rel=\"tag\""),
        "default language has plum: {html}"
    );
    assert!(!html.contains("/fr/tag/"), "no link to a 404");
    assert!(
        html.contains("<span>#Fr only</span>"),
        "no en post has it: plain text"
    );
    assert!(html.contains("<a href=\"/series/journey\">Rust Journey</a>"));
    let plain = get(&db, "/fr/f2").1;
    assert!(
        plain.contains("<p class=\"series\">Series: Fr series (1)</p>"),
        "{plain}"
    );
    assert!(!plain.contains("/fr/series/"));
}
