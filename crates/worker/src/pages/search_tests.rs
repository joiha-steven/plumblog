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

fn store() -> MemoryStore {
    let db = MemoryStore::default();
    run(seed::sample_posts(&db)).unwrap();
    db
}

fn search(db: &MemoryStore, q: &str) -> String {
    match run(render(SampleSource::get(), &Db(db), &Db(db), "/search", q)).unwrap() {
        Outcome::Page { status, body, .. } => {
            assert_eq!(status, 200, "{q:?}");
            body
        }
        Outcome::Redirect(to) => panic!("{q:?} redirected to {to}"),
    }
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
