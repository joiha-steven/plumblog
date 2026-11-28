//! Reader pages over the in-memory store (the same `Db` that reads D1 in the Worker): site
//! settings and their defaults, routing by language, paging guards, an empty site. The same
//! flows run against D1 in tools/worker-test/reader.test.mjs.

use super::db::Db;
use super::tests_support::run;
use super::{Outcome, render, seed};
use plumblog_model::MediaId;
use plumblog_store::memory::MemoryStore;
use plumblog_store::{
    MediaStore, NewMedia, PostKind, PostStatus, PostStore, SavePost, SettingsStore,
};

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
    published(db, kind, slug, None);
}

fn published(db: &MemoryStore, kind: PostKind, slug: &str, cover: Option<MediaId>) {
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
        cover_media_id: cover,
        series_id: None,
        series_position: None,
        tag_ids: Vec::new(),
        media_ids: Vec::new(),
        search_text: String::new(),
        revision: None,
        client_id: None,
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

/// Page data from the store, but a comment read is a bug: a static page must not make one.
struct NoComments<'a>(Db<'a, MemoryStore>);

impl super::db::ExtSource for NoComments<'_> {
    async fn content_version(&self) -> Result<u64, String> {
        self.0.content_version().await
    }
    async fn theme_options(&self, theme: &str) -> Result<Vec<(String, String)>, String> {
        self.0.theme_options(theme).await
    }
    async fn enabled_plugins(&self) -> Result<Vec<(String, Vec<(String, String)>)>, String> {
        self.0.enabled_plugins().await
    }
    fn comments(
        &self,
        post: &super::source::StoredPost,
        _: &str,
    ) -> impl Future<Output = Result<Option<plumblog_render::Comments>, String>> {
        std::future::ready(Err(format!("comments read for {}", post.slug)))
    }
}

#[test]
fn a_static_page_reads_no_comments_and_stays_cacheable() {
    let db = seeded();
    run(db.put_setting("comments.enabled", "true", 1)).unwrap();
    commentable(&db, PostKind::Page, "about");
    commentable(&db, PostKind::Post, "a-post");
    let ext = NoComments(Db(&db));
    let cacheable = |path: &str| match run(render(&Db(&db), &ext, &Db(&db), path, "")).unwrap() {
        Outcome::Page {
            status: 200,
            cacheable,
            ..
        } => cacheable,
        other => panic!("{path}: {other:?}"),
    };
    assert!(cacheable("/about"), "no comment read, so a complete page");
    assert!(
        !cacheable("/a-post"),
        "a post does read them (and this read fails)"
    );
}

#[test]
fn home_pages_and_404_suggestions_show_the_default_language_only() {
    let db = seeded();
    // The vi post is the oldest, so it would be on the last page if languages were mixed.
    for path in ["/", "/page/2", "/nope"] {
        let (_, html) = get(&db, path);
        assert!(
            !html.contains("bai-viet-dau-tien"),
            "{path} lists a vi post"
        );
        assert!(html.contains("href=\"/"), "{path}");
    }
    assert!(
        get(&db, "/page/2").1.contains("/first-post"),
        "the oldest en post is last"
    );
    assert_eq!(get(&db, "/page/3").0, 404, "9 en posts at 5 per page");
}

#[test]
fn an_empty_feed_is_dated_by_a_real_time_or_not_at_all() {
    let db = MemoryStore::default();
    for path in ["/feed.xml", "/rss.xml", "/sitemap.xml"] {
        let body = get(&db, path).1;
        assert!(!body.contains("1970"), "{path}");
        assert!(
            !body.contains("<updated>")
                && !body.contains("lastBuildDate")
                && !body.contains("<lastmod>"),
            "{path} states a date the site never had: {body}"
        );
    }
    // A setting write is a time the site really has (ms).
    run(db.put_setting("site_title", "\"T\"", 1_790_942_400_000)).unwrap();
    assert!(
        get(&db, "/feed.xml")
            .1
            .contains("<updated>2026-10-02T12:00:00Z</updated>")
    );
    assert!(get(&db, "/rss.xml").1.contains("<lastBuildDate>"));
    assert!(!get(&db, "/sitemap.xml").1.contains("<lastmod>"));
}

const VARIANTS: &str = r#"[
 {"key":"media/1/cover-480.avif","width":480,"height":240,"bytes":10,"mime":"image/avif"},
 {"key":"media/1/cover-960.avif","width":960,"height":480,"bytes":20,"mime":"image/avif"},
 {"key":"media/1/cover-480.webp","width":480,"height":240,"bytes":30,"mime":"image/webp"},
 {"key":"media/1/cover-960.webp","width":960,"height":480,"bytes":40,"mime":"image/webp"},
 {"key":"media/1/cover-1600.webp","width":1600,"height":800,"bytes":50,"mime":"image/webp"}]"#;

fn media(db: &MemoryStore, alt: &str, variants: &str) -> MediaId {
    run(db.create_media(NewMedia {
        r2_key: "media/1/cover.png".to_owned(),
        mime: "image/png".to_owned(),
        bytes: 100,
        width: 3000,
        height: 1500,
        alt: alt.to_owned(),
        variants_json: variants.to_owned(),
        uploaded_by: None,
        created_at: 1,
        sha256: None,
    }))
    .unwrap()
}

#[test]
fn a_cover_becomes_an_absolute_social_card() {
    let db = seeded();
    run(db.put_setting("origin", "\"https://blog.example\"", 1)).unwrap();
    let id = media(&db, "A \"quoted\" view", VARIANTS);
    published(&db, PostKind::Post, "with-cover", Some(id));
    published(&db, PostKind::Page, "about-cover", Some(id));
    published(&db, PostKind::Post, "no-cover", None);
    // Largest WebP up to 1200 px (AVIF is not a card type for most networks).
    let img = format!("https://blog.example/media/{}/cover-960.webp", id.0);
    for slug in ["with-cover", "about-cover"] {
        let html = get(&db, &format!("/{slug}")).1;
        for tag in [
            format!("<meta property=\"og:image\" content=\"{img}\">"),
            "<meta property=\"og:image:width\" content=\"960\">".to_owned(),
            "<meta property=\"og:image:height\" content=\"480\">".to_owned(),
            "<meta property=\"og:image:alt\" content=\"A &quot;quoted&quot; view\">".to_owned(),
            "<meta name=\"twitter:card\" content=\"summary_large_image\">".to_owned(),
            format!("<meta name=\"twitter:image\" content=\"{img}\">"),
            "<meta name=\"twitter:image:alt\" content=\"A &quot;quoted&quot; view\">".to_owned(),
            format!("\"image\":[\"{img}\"]"),
        ] {
            assert!(html.contains(&tag), "{slug}: missing {tag}");
        }
    }
    let plain = get(&db, "/no-cover").1;
    for gone in ["og:image", "twitter:image", "\"image\":"] {
        assert!(!plain.contains(gone), "{gone} without a cover");
    }
    assert!(plain.contains("<meta name=\"twitter:card\" content=\"summary\">"));
}

#[test]
fn a_cover_without_variants_or_alt_uses_the_original_and_no_alt_tags() {
    let db = seeded();
    run(db.put_setting("origin", "\"https://blog.example\"", 1)).unwrap();
    let id = media(&db, "", "[]");
    published(&db, PostKind::Post, "plain-cover", Some(id));
    let html = get(&db, "/plain-cover").1;
    let img = format!("https://blog.example/media/{}/cover.png", id.0);
    assert!(
        html.contains(&format!("og:image\" content=\"{img}\"")),
        "{html}"
    );
    assert!(html.contains("og:image:width\" content=\"3000\""));
    assert!(html.contains("og:image:height\" content=\"1500\""));
    assert!(!html.contains("og:image:alt") && !html.contains("twitter:image:alt"));
}

#[test]
fn no_origin_means_no_image_tags() {
    let db = seeded();
    // The sample sets an origin; an empty one is the same as none.
    run(db.put_setting("origin", "\"\"", 1)).unwrap();
    let id = media(&db, "x", VARIANTS);
    published(&db, PostKind::Post, "with-cover", Some(id));
    let html = get(&db, "/with-cover").1;
    assert!(
        !html.contains("og:image") && !html.contains("twitter:image"),
        "{html}"
    );
    assert!(html.contains("<meta name=\"twitter:card\" content=\"summary\">"));
}

/// A published post in `lang`, `at` ms after the epoch (the newest has the largest).
fn publish(db: &MemoryStore, lang: &str, slug: &str, at: i64) {
    let author = run(plumblog_store::UserStore::list_users(db)).unwrap()[0].id;
    run(db.save_post(SavePost {
        id: None,
        expected_version: None,
        translation_group: None,
        kind: PostKind::Post,
        lang: lang.to_owned(),
        slug: slug.to_owned(),
        status: PostStatus::Published,
        published_at: Some(at),
        author_id: author,
        title: format!("Title {slug}"),
        markdown: String::new(),
        html: "<p>x</p>".to_owned(),
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
        search_text: String::new(),
        revision: None,
        client_id: None,
        now: at,
    }))
    .unwrap();
}

/// The sample site with 7 vi posts in all (5 per page: two pages).
fn seeded_vi() -> MemoryStore {
    let db = seeded();
    for i in 0..6 {
        publish(&db, "vi", &format!("vi-{i}"), 1_900_000_000_000 + i * 1000);
    }
    db
}

#[test]
fn a_language_home_lists_its_language_with_paging_and_alternates() {
    let db = seeded_vi();
    let (status, p1) = get(&db, "/vi/");
    assert_eq!(status, 200, "{p1}");
    assert!(p1.contains("<html lang=\"vi\""));
    assert!(p1.contains("href=\"/vi/vi-5\""));
    assert!(!p1.contains("href=\"/first-post\""), "an en post on /vi/");
    assert!(p1.contains("<link rel=\"canonical\" href=\"https://blog.test/vi/\">"));
    // Its own feeds, not the default language's.
    for f in ["feed.xml", "rss.xml", "feed.json"] {
        assert!(p1.contains(&format!("href=\"/vi/{f}\"")), "{f}");
        assert!(!p1.contains(&format!("href=\"/{f}\"")), "{f}");
    }
    assert!(p1.contains("href=\"/vi/page/2\" rel=\"next\""));
    // Page 1 carries the alternates, back to the default home too.
    assert!(p1.contains("hreflang=\"en\" href=\"https://blog.test/\""));
    assert!(p1.contains("hreflang=\"vi\" href=\"https://blog.test/vi/\""));
    assert!(p1.contains("hreflang=\"x-default\" href=\"https://blog.test/\""));

    let (status, p2) = get(&db, "/vi/page/2");
    assert_eq!(status, 200);
    assert!(p2.contains("bai-viet-dau-tien"), "the oldest vi post");
    assert!(!p2.contains("rel=\"alternate\" hreflang"), "only page 1");
    assert!(p2.contains("<link rel=\"canonical\" href=\"https://blog.test/vi/page/2\">"));
    assert!(p2.contains("href=\"/vi/\" rel=\"prev\""));
    assert_eq!(get(&db, "/vi/page/3").0, 404);
    assert_eq!(get(&db, "/vi/page/1"), (301, "/vi/".to_owned()));
    assert_eq!(get(&db, "/vi/page/1001").0, 404, "past the offset cap");
    assert_eq!(get(&db, "/vi/page/0").0, 404);

    // The default home links to the language home.
    let home = get(&db, "/").1;
    assert!(home.contains("hreflang=\"vi\" href=\"https://blog.test/vi/\""));
    assert!(home.contains("hreflang=\"en\" href=\"https://blog.test/\""));
    assert!(!get(&db, "/page/2").1.contains("rel=\"alternate\" hreflang"));
}

#[test]
fn the_default_language_home_has_one_url() {
    let db = seeded_vi();
    assert_eq!(get(&db, "/en/"), (301, "/".to_owned()));
    assert_eq!(get(&db, "/en/page/2"), (301, "/page/2".to_owned()));
    assert_eq!(get(&db, "/en/page/1"), (301, "/".to_owned()));
}

#[test]
fn only_a_site_language_has_a_home() {
    let db = seeded_vi();
    assert_eq!(get(&db, "/xx/").0, 404);
    assert_eq!(get(&db, "/xx/page/2").0, 404);
    // `/fr/page/1` is not a language's first page: no redirect into a 404.
    assert_eq!(get(&db, "/fr/page/1").0, 404);
    assert_eq!(get(&db, "/vi/page/1"), (301, "/vi/".to_owned()));
    assert_eq!(get(&db, "/fr/").0, 404);
    // A segment the router owns is never a language, even when it looks like one.
    assert_eq!(get(&db, "/api/"), (301, "/api".to_owned()));
    // A trailing slash on a default-language post still redirects to the post.
    publish(&db, "en", "abc", 5);
    assert_eq!(get(&db, "/abc/"), (301, "/abc".to_owned()));
}

#[test]
fn a_configured_language_with_no_posts_has_an_empty_home() {
    let db = seeded();
    run(db.put_setting("site_languages", "[\"en\",\"vi\",\"de\"]", 1)).unwrap();
    let (status, html) = get(&db, "/de/");
    assert_eq!(status, 200);
    assert!(html.contains("<html lang=\"de\""));
    assert_eq!(get(&db, "/de/page/2").0, 404);
    let sitemap = get(&db, "/sitemap.xml").1;
    assert!(
        !sitemap.contains("blog.test/de/"),
        "no posts: not in the sitemap"
    );
    assert!(sitemap.contains("<loc>https://blog.test/vi/</loc>"));
    // No alternate points at the empty home, and it carries none of its own.
    for path in ["/", "/vi/"] {
        assert!(!get(&db, path).1.contains("hreflang=\"de\""), "{path}");
    }
    assert!(!sitemap.contains("hreflang=\"de\""));
    assert!(!html.contains("rel=\"alternate\" hreflang"));
    assert_eq!(get(&db, "/de/page/1"), (301, "/de/".to_owned()));
    assert!(get(&db, "/").1.contains("hreflang=\"vi\""));
}

#[test]
fn a_language_home_is_the_home_link_of_its_own_pages() {
    let db = seeded_vi();
    for path in ["/vi/", "/vi/page/2"] {
        let html = get(&db, path).1;
        assert!(
            html.contains("class=\"site-name\" href=\"/vi/\""),
            "{path}: the site name links to the vi home"
        );
        assert!(
            html.contains("\"@type\":\"Blog\"")
                && html.contains("\"url\":\"https://blog.test/vi/\""),
            "{path}: Blog.url"
        );
    }
    assert!(get(&db, "/").1.contains("class=\"site-name\" href=\"/\""));
}
