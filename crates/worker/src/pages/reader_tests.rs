//! Reader pages over the in-memory store (the same `Db` that reads D1 in the Worker): site
//! settings and their defaults, routing by language, paging guards, an empty site. The same
//! flows run against D1 in tools/worker-test/reader.test.mjs.

use super::db::Db;
use super::tests_support::run;
use super::{Outcome, render, seed};
use plumblog_store::memory::MemoryStore;
use plumblog_store::{PostKind, PostStatus, PostStore, SavePost, SettingsStore};

fn get(db: &MemoryStore, path: &str) -> (u16, String) {
    match run(render(&Db(db), &Db(db), &Db(db), path, "")).unwrap() {
        Outcome::Page { status, body, .. } => (status, body),
        Outcome::Redirect(to) => (301, to),
    }
}

fn seeded() -> MemoryStore {
    let db = MemoryStore::default();
    run(seed::sample_posts(&db)).unwrap();
    db
}

#[test]
fn an_empty_site_renders_with_defaults() {
    let db = MemoryStore::default();
    let (status, home) = get(&db, "/");
    assert_eq!(status, 200);
    assert!(home.contains("<title>Untitled site</title>"), "{home}");
    for path in [
        "/feed.xml",
        "/rss.xml",
        "/feed.json",
        "/sitemap.xml",
        "/llms.txt",
    ] {
        assert_eq!(get(&db, path).0, 200, "{path}");
    }
    assert!(get(&db, "/feed.xml").1.contains("<feed"));
    assert_eq!(get(&db, "/nothing-here").0, 404);
    assert_eq!(get(&db, "/page/2").0, 404);
    assert_eq!(get(&db, "/tag/none").0, 404);
    assert_eq!(get(&db, "/archive/2025").0, 404);
}

#[test]
fn site_settings_come_from_the_store() {
    let db = seeded();
    let (_, home) = get(&db, "/");
    assert!(home.contains("<title>Plum Notes</title>"));
    run(db.put_setting("site_title", "\"Other\"", 1)).unwrap();
    assert!(get(&db, "/").1.contains("<title>Other</title>"));
}

#[test]
fn languages_live_under_their_own_prefix() {
    let db = seeded();
    // The default language is at the root only.
    assert_eq!(get(&db, "/en/first-post"), (301, "/first-post".to_owned()));
    let (status, en) = get(&db, "/first-post");
    assert_eq!(status, 200);
    assert!(en.contains("hreflang=\"vi\" href=\"https://blog.test/vi/bai-viet-dau-tien\""));
    assert!(en.contains("hreflang=\"x-default\" href=\"https://blog.test/first-post\""));
    // The other language is under its prefix, never at the root.
    assert_eq!(get(&db, "/bai-viet-dau-tien").0, 404);
    assert_eq!(get(&db, "/en/bai-viet-dau-tien").1, "/bai-viet-dau-tien");
    let (status, vi) = get(&db, "/vi/bai-viet-dau-tien");
    assert_eq!(status, 200);
    assert!(vi.contains("<html lang=\"vi\""));
    assert!(vi.contains("hreflang=\"en\" href=\"https://blog.test/first-post\""));
    assert_eq!(
        get(&db, "/vi/first-post").0,
        404,
        "a post is in one language"
    );
    assert_eq!(get(&db, "/fr/first-post").0, 404);
}

#[test]
fn a_trashed_post_is_not_found() {
    let db = seeded();
    assert_eq!(get(&db, "/first-post").0, 200);
    let post = run(db.find_post("en", "first-post")).unwrap().unwrap();
    run(db.trash_post(post.id, 5)).unwrap();
    assert_eq!(get(&db, "/first-post").0, 404);
    assert!(!get(&db, "/").1.contains("/first-post\""));
}

#[test]
fn pages_past_the_end_are_404_decided_from_the_count() {
    let db = seeded();
    // 10 sample posts at 5 per page: two pages.
    assert_eq!(get(&db, "/page/2").0, 200);
    assert_eq!(get(&db, "/page/3").0, 404);
    assert_eq!(get(&db, "/page/1"), (301, "/".to_owned()));
    assert_eq!(get(&db, "/tag/life/page/9").0, 404);
    assert_eq!(get(&db, "/archive/2025/page/9").0, 404);
    // Offset 10,000 at 10 per page: refused before any query.
    run(db.put_setting("site_per_page", "10", 1)).unwrap();
    assert_eq!(get(&db, "/page/1001").0, 404);
    assert_eq!(get(&db, "/page/1000").0, 404);
    assert_eq!(get(&db, "/tag/life/page/1001").0, 404);
    assert_eq!(get(&db, "/archive/2025/page/1001").0, 404);
    assert_eq!(get(&db, "/page/999999").0, 404);
}

/// A published `kind` at `/<slug>` with its own comment switch on.
fn commentable(db: &MemoryStore, kind: PostKind, slug: &str) {
    let author = run(plumblog_store::UserStore::list_users(db)).unwrap()[0].id;
    run(db.save_post(SavePost {
        id: None,
        expected_version: None,
        translation_group: None,
        kind,
        lang: "en".to_owned(),
        slug: slug.to_owned(),
        status: PostStatus::Published,
        published_at: Some(1_000),
        author_id: author,
        title: "T".to_owned(),
        markdown: String::new(),
        html: "<p>x</p>".to_owned(),
        toc_json: "[]".to_owned(),
        excerpt: String::new(),
        word_count: 1,
        reading_minutes: 1,
        pinned: false,
        comments_enabled: true,
        cover_media_id: None,
        series_id: None,
        series_position: None,
        tag_ids: Vec::new(),
        media_ids: Vec::new(),
        search_text: String::new(),
        revision: None,
        now: 1_000,
    }))
    .unwrap();
}

#[test]
fn a_static_page_shows_no_comment_form() {
    let db = seeded();
    run(db.put_setting("comments.enabled", "true", 1)).unwrap();
    commentable(&db, PostKind::Post, "a-post");
    commentable(&db, PostKind::Page, "about");
    assert!(
        get(&db, "/a-post").1.contains("id=\"comments\""),
        "posts take comments"
    );
    let (status, page) = get(&db, "/about");
    assert_eq!(status, 200);
    assert!(
        !page.contains("id=\"comments\""),
        "the comment target refuses pages"
    );
}
