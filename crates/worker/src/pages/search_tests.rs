//! The search page over the in-memory store holding the sample posts. The same queries run
//! against D1 in tools/worker-test/search.test.mjs. Vietnamese is escaped (code must be
//! ASCII): duong = "d\u{01B0}\u{1EDD}ng" with d-stroke "\u{0111}", Da Lat = "\u{0110}\u{00E0}
//! L\u{1EA1}t", Nguoi Viet = "Ng\u{01B0}\u{1EDD}i Vi\u{1EC7}t".

use super::db::Db;
use super::sample::SampleSource;
use super::search::{MAX_QUERY_CHARS, RESULT_LIMIT};
use super::tests_support::run;
use super::{Outcome, RenderError, render, seed};
use plumblog_store::memory::MemoryStore;
use plumblog_store::{PostKind, PostStatus, PostStore, SavePost};

fn store() -> MemoryStore {
    let db = MemoryStore::default();
    run(seed::sample_posts(&db)).unwrap();
    db
}

fn search(db: &MemoryStore, q: &str) -> String {
    search_at(db, "/search", q)
}

fn search_at(db: &MemoryStore, path: &str, q: &str) -> String {
    match get(db, path, q) {
        (200, body) => body,
        (status, body) => panic!("{path}?q={q:?}: {status} {body}"),
    }
}

/// Status and body of a page, or 301 and the redirect target.
fn get(db: &MemoryStore, path: &str, q: &str) -> (u16, String) {
    match run(render(SampleSource::get(), &Db(db), &Db(db), path, q)).unwrap() {
        Outcome::Page { status, body, .. } => (status, body),
        Outcome::Redirect(to) => (301, to),
    }
}

/// A published Vietnamese post whose title and search text are `text`.
fn publish_vi(db: &MemoryStore, slug: &str, text: &str) {
    publish_in(db, "vi", slug, text);
}

fn publish_in(db: &MemoryStore, lang: &str, slug: &str, text: &str) {
    let author = run(plumblog_store::UserStore::list_users(db)).unwrap()[0].id;
    run(db.save_post(SavePost {
        id: None,
        expected_version: None,
        translation_group: None,
        kind: PostKind::Post,
        lang: lang.to_owned(),
        slug: slug.to_owned(),
        status: PostStatus::Published,
        published_at: Some(1_900_000_000_000),
        author_id: author,
        title: text.to_owned(),
        markdown: String::new(),
        html: format!("<p>{text}</p>"),
        toc_json: "[]".to_owned(),
        excerpt: String::new(),
        word_count: 1,
        reading_minutes: 1,
        pinned: false,
        comments_enabled: false,
        cover_media_id: None,
        series_id: None,
        series_position: None,
        tag_ids: Vec::new(),
        media_ids: Vec::new(),
        search_text: text.to_owned(),
        revision: None,
        client_id: None,
        now: 1_900_000_000_000,
    }))
    .unwrap();
}

/// Links in the result list, in order.
fn results(html: &str) -> Vec<String> {
    let Some(list) = html.split("<ol class=\"posts\">").nth(1) else {
        return Vec::new();
    };
    let list = &list[..list.find("</ol>").unwrap()];
    list.split("href=\"")
        .skip(1)
        .map(|s| s[..s.find('"').unwrap()].to_owned())
        .filter(|h| !h.starts_with("/tag/"))
        .collect()
}

#[test]
fn empty_query_shows_the_form_and_a_hint() {
    let db = store();
    for q in ["", "   "] {
        let html = search(&db, q);
        assert!(html.contains("<meta name=\"robots\" content=\"noindex\">"));
        assert!(html.contains("Search titles and text of every post."));
        assert!(html.contains("<link rel=\"canonical\" href=\"https://blog.test/search\">"));
        assert!(results(&html).is_empty());
        assert!(!html.contains("application/ld+json"), "no structured data");
    }
}

#[test]
fn vietnamese_matches_with_and_without_marks() {
    let db = store();
    // Words only this post has: exactly one hit.
    for q in [
        "d\u{01B0}\u{1EDD}ng",
        "\u{0111}\u{01B0}\u{1EDD}ng",
        "duong",
        "DUONG",
        "du\u{031B}o\u{031B}\u{0300}ng", // typed decomposed (macOS)
        "Ng\u{01B0}\u{1EDD}i Vi\u{1EC7}t",
        "nguoi viet",
    ] {
        assert_eq!(results(&search(&db, q)), ["/notes-from-da-lat"], "{q:?}");
    }
    // Words are prefixes, so "da lat" also finds "day ... later"; the post must be among them.
    for q in ["\u{0110}\u{00E0} L\u{1EA1}t", "da lat"] {
        assert!(
            results(&search(&db, q)).contains(&"/notes-from-da-lat".to_owned()),
            "{q:?}"
        );
    }
}

#[test]
fn results_are_newest_first_and_every_word_must_match() {
    let db = store();
    let html = search(&db, "tools");
    let hits = results(&html);
    assert!(hits.len() >= 2, "{hits:?}");
    assert_eq!(hits[0], "/why-small-tools");
    assert!(html.contains(&format!("{} posts for", hits.len())));
    let none = search(&db, "duong hanoi");
    assert!(results(&none).is_empty());
    assert!(none.contains("Nothing found for"));
}

#[test]
fn fts_syntax_is_plain_text() {
    let db = store();
    for q in [
        "\"",
        "\"\"",
        "*",
        "NEAR(",
        "NEAR(a b)",
        "OR",
        "a OR b",
        "-",
        "-duong",
        "\"unbalanced",
        "title:duong",
        "^duong",
        "(",
        ")",
        "duong*",
        "{x}",
        "'",
        ";--",
        "AND NOT",
    ] {
        // `search` asserts a 200 page: never an error, whatever the syntax.
        search(&db, q);
    }
    // Operators are words: "OR" does not widen a query into either word.
    assert!(results(&search(&db, "duong OR tools")).is_empty());
    assert_eq!(results(&search(&db, "-duong")), ["/notes-from-da-lat"]);
}

#[test]
fn the_query_is_echoed_escaped_and_capped() {
    let db = store();
    let html = search(&db, "<script>alert(1)</script>");
    assert!(!html.contains("<script>alert"));
    assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    let long = "a".repeat(MAX_QUERY_CHARS * 5);
    let html = search(&db, &long);
    assert!(html.contains(&"a".repeat(MAX_QUERY_CHARS)));
    assert!(!html.contains(&"a".repeat(MAX_QUERY_CHARS + 1)));
}

#[test]
fn wide_queries_stop_at_the_cap_and_say_so() {
    let db = MemoryStore::default();
    run(seed::sample_posts(&db)).unwrap();
    run(seed::copies(&db, "widely", RESULT_LIMIT + 5)).unwrap();
    let html = search(&db, "widely");
    assert_eq!(results(&html).len(), RESULT_LIMIT);
    assert!(html.contains(&format!("The {RESULT_LIMIT} newest posts for")));
}

#[test]
fn a_store_failure_is_an_error_not_an_empty_page() {
    struct Broken;
    impl super::search::SearchSource for Broken {
        fn search(
            &self,
            _: &str,
            _: &str,
            _: u32,
        ) -> impl Future<Output = Result<Vec<plumblog_store::PostSummary>, String>> {
            std::future::ready(Err("d1 down".to_owned()))
        }
    }
    let db = MemoryStore::default();
    let r = run(render(
        SampleSource::get(),
        &Db(&db),
        &Broken,
        "/search",
        "duong",
    ));
    // A storage error, never a theme one: a pushed theme does not fall back on it.
    assert_eq!(r.unwrap_err(), RenderError::Other("d1 down".to_owned()));
    // A query with no words never reaches the store.
    assert!(
        run(render(
            SampleSource::get(),
            &Db(&db),
            &Broken,
            "/search",
            "**"
        ))
        .is_ok()
    );
}

// "B\u{1ED9}t m\u{00E0}u" is the Vietnamese "bot mau" (pigment).
const BOT_MAU: &str = "B\u{1ED9}t m\u{00E0}u r\u{1EA5}t \u{0111}\u{1EB9}p";

#[test]
fn each_language_searches_only_its_own_posts() {
    let db = store();
    publish_vi(&db, "bot-mau", BOT_MAU);
    for q in [
        "bot mau",
        "B\u{1ED9}t m\u{00E0}u",
        "\u{0111}\u{1EB9}p",
        "dep",
    ] {
        assert_eq!(
            results(&search_at(&db, "/vi/search", q)),
            ["/vi/bot-mau"],
            "{q:?}"
        );
        assert!(
            results(&search_at(&db, "/search", q)).is_empty(),
            "{q:?} at /search"
        );
    }
    // An English query finds English at /search and nothing at /vi/search.
    assert_eq!(results(&search(&db, "tools"))[0], "/why-small-tools");
    assert!(results(&search_at(&db, "/vi/search", "tools")).is_empty());
}

#[test]
fn search_urls_follow_the_one_url_rule() {
    let db = store();
    assert_eq!(
        get(&db, "/en/search", "tools"),
        (301, "/search?q=tools".to_owned())
    );
    assert_eq!(
        get(&db, "/en/search", "\u{0111}\u{00E0} l\u{1EA1}t&x"),
        (301, "/search?q=%C4%91%C3%A0+l%E1%BA%A1t%26x".to_owned())
    );
    assert_eq!(get(&db, "/en/search", ""), (301, "/search".to_owned()));
    assert_eq!(get(&db, "/vi/search/", "x"), (301, "/vi/search".to_owned()));
    assert_eq!(get(&db, "/xx/search", "tools").0, 404);
    assert_eq!(get(&db, "/api/search", "tools").0, 404);
}

#[test]
fn the_page_and_its_form_follow_the_page_language() {
    let db = store();
    let action = |path: &str| {
        let html = search_at(&db, path, "");
        html.contains(&format!(
            "action=\"{}\"",
            if path.starts_with("/vi") {
                "/vi/search"
            } else {
                "/search"
            }
        ))
    };
    let vi = search_at(&db, "/vi/search", "bot");
    assert!(vi.contains("<html lang=\"vi\""));
    assert!(vi.contains("<link rel=\"canonical\" href=\"https://blog.test/vi/search\">"));
    assert!(vi.contains("<meta name=\"robots\" content=\"noindex\">"));
    assert!(!vi.contains("action=\"/search\""), "{vi}");
    assert!(action("/vi/search") && action("/search"));
    // Any vi page submits to /vi/search; any default-language page to /search.
    for path in ["/vi/", "/vi/bai-viet-dau-tien"] {
        let (status, html) = get(&db, path, "");
        assert_eq!(status, 200, "{path}");
        assert!(html.contains("action=\"/vi/search\""), "{path}");
        assert!(html.contains("href=\"/vi/search\""), "{path}");
        assert!(!html.contains("action=\"/search\""), "{path}");
    }
    for path in ["/", "/first-post", "/nothing-here"] {
        let html = get(&db, path, "").1;
        assert!(html.contains("action=\"/search\""), "{path}");
    }
}

#[test]
fn only_a_listed_language_has_a_search_page_else_it_is_a_post_path() {
    let db = store();
    // `fr` is not a site language: `/fr/search` is the post it always was.
    publish_in(&db, "fr", "search", "Bonjour");
    let (status, html) = match run(render(&Db(&db), &Db(&db), &Db(&db), "/fr/search", "")).unwrap()
    {
        Outcome::Page { status, body, .. } => (status, body),
        Outcome::Redirect(to) => panic!("redirected to {to}"),
    };
    assert_eq!(status, 200);
    assert!(html.contains("Bonjour"), "{html}");
    assert!(html.contains("<link rel=\"canonical\" href=\"https://blog.test/fr/search\">"));
    // A listed language's search page wins over a stored post of that slug.
    publish_in(&db, "vi", "search", "Shadowed");
    assert!(search_at(&db, "/vi/search", "").contains("action=\"/vi/search\""));
}
