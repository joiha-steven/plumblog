use super::routes::{Route, parse};
use super::source::{
    Filter, Neighbours, ReaderSource, SampleSource, Settings, Slice, StoredPost, Tag,
};
use super::{Outcome, render};
use plumblog_model::Result;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

/// The sample source never awaits, so one poll finishes; no executor dependency for tests.
pub(super) fn run<F: Future>(f: F) -> F::Output {
    match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("sample source future was pending"),
    }
}

fn get(path: &str) -> (u16, String) {
    match run(render(SampleSource::get(), &super::NoSearch, path, "")).unwrap() {
        Outcome::Page { status, body, .. } => (status, body),
        Outcome::Redirect(to) => (301, to),
    }
}

pub(super) fn get_typed(path: &str) -> (u16, &'static str, String) {
    get_from(SampleSource::get(), path)
}

pub(super) fn get_from(src: &impl ReaderSource, path: &str) -> (u16, &'static str, String) {
    match run(render(src, &super::NoSearch, path, "")).unwrap() {
        Outcome::Page {
            status,
            content_type,
            body,
        } => (status, content_type, body),
        Outcome::Redirect(to) => panic!("{path} redirected to {to}"),
    }
}

/// The sample posts with changes: other languages, hostile titles, a configured origin.
pub(super) struct Fixture {
    posts: SampleSource,
    settings: Settings,
}

impl Fixture {
    pub(super) fn new(edit: impl FnOnce(&mut Vec<StoredPost>, &mut Settings)) -> Self {
        let sample = SampleSource::get();
        let mut posts = run(sample.posts(Filter::All, 0, usize::MAX)).unwrap().posts;
        let mut settings = run(sample.settings()).unwrap();
        edit(&mut posts, &mut settings);
        let posts = SampleSource::from_posts(posts);
        // The sample's own rule for the site date, applied to the edited posts.
        settings.updated_at = run(posts.settings()).unwrap().updated_at;
        Self { posts, settings }
    }
}

impl ReaderSource for Fixture {
    async fn content_version(&self) -> Result<u64> {
        self.posts.content_version().await
    }
    fn settings(&self) -> impl Future<Output = Result<Settings>> {
        std::future::ready(Ok(self.settings.clone()))
    }
    async fn posts(&self, filter: Filter<'_>, offset: usize, limit: usize) -> Result<Slice> {
        self.posts.posts(filter, offset, limit).await
    }
    async fn post(&self, slug: &str) -> Result<Option<Neighbours>> {
        self.posts.post(slug).await
    }
    async fn tag(&self, slug: &str) -> Result<Option<Tag>> {
        self.posts.tag(slug).await
    }
    async fn tags(&self) -> Result<Vec<Tag>> {
        self.posts.tags().await
    }
    async fn years(&self) -> Result<Vec<i32>> {
        self.posts.years().await
    }
}

#[test]
fn routes_parse_to_one_canonical_url() {
    assert_eq!(parse("/"), Route::Home { page: 1 });
    assert_eq!(parse("/page/2"), Route::Home { page: 2 });
    assert_eq!(parse("/page/1"), Route::Redirect("/".into()));
    assert_eq!(parse("/page/02"), Route::NotFound);
    assert_eq!(parse("/page/x"), Route::NotFound);
    assert_eq!(
        parse("/hello-world/"),
        Route::Redirect("/hello-world".into())
    );
    assert_eq!(
        parse("/hello-world"),
        Route::Post {
            slug: "hello-world".into()
        }
    );
    assert_eq!(parse("/Hello"), Route::NotFound);
    assert_eq!(parse("/tag"), Route::NotFound);
    assert_eq!(
        parse("/tag/rust"),
        Route::Tag {
            slug: "rust".into(),
            page: 1
        }
    );
    assert_eq!(
        parse("/tag/rust/page/3"),
        Route::Tag {
            slug: "rust".into(),
            page: 3
        }
    );
    assert_eq!(
        parse("/tag/rust/page/1"),
        Route::Redirect("/tag/rust".into())
    );
    assert_eq!(
        parse("/archive/2026"),
        Route::Archive {
            year: 2026,
            page: 1
        }
    );
    assert_eq!(
        parse("/archive/2026/page/2"),
        Route::Archive {
            year: 2026,
            page: 2
        }
    );
    assert_eq!(
        parse("/archive/2026/page/1"),
        Route::Redirect("/archive/2026".into())
    );
    assert_eq!(parse("/archive/26"), Route::NotFound);
    assert_eq!(parse("/a/b"), Route::NotFound);
}

/// Request paths that once became off-site or self-looping redirects (review of #29).
#[test]
fn hostile_paths_never_redirect_off_site_or_to_themselves() {
    for path in [
        "//evil.com/",
        "//evil.com",
        "/\\evil.com/",
        "//",
        "///",
        "/%2F%2Fevil.com/",
        "/%2f%2fevil.com/",
        "/%5Cevil.com/",
        "/post//",
        "/tag//rust/",
    ] {
        assert_eq!(parse(path), Route::NotFound, "{path}");
    }
    // Every redirect the router does produce is a same-site absolute path.
    for path in ["/post/", "/page/1", "/tag/rust/page/1", "/archive/2026/"] {
        match parse(path) {
            Route::Redirect(to) => assert!(
                to.starts_with('/') && !to.starts_with("//") && to != path,
                "{path} -> {to}"
            ),
            other => panic!("{path}: {other:?}"),
        }
    }
}

#[test]
fn home_pages_list_pinned_first_and_link_older() {
    let (status, html) = get("/");
    assert_eq!(status, 200);
    assert!(html.contains("<title>Plum Notes</title>"));
    let pinned = html.find("Why I keep my tools small").unwrap();
    let newest = html.find("Reading on screens, done properly").unwrap();
    assert!(pinned < newest, "pinned post comes first");
    assert!(html.contains("href=\"/page/2\" rel=\"next\""));
    assert!(!html.contains("First post"), "page 1 holds per_page posts");

    let (status, html) = get("/page/2");
    assert_eq!(status, 200);
    assert!(html.contains("First post"));
    assert!(html.contains("href=\"/\" rel=\"prev\""));
    assert!(!html.contains("rel=\"next\">"));

    assert_eq!(get("/page/9").0, 404);
    assert_eq!(get("/page/1"), (301, "/".into()));
}

#[test]
fn post_page_has_content_neighbours_and_tags() {
    let (status, html) = get("/reading-on-screens");
    assert_eq!(status, 200);
    assert!(html.contains("<title>Reading on screens, done properly - Plum Notes</title>"));
    assert!(
        html.contains("<link rel=\"canonical\" href=\"https://blog.test/reading-on-screens\">")
    );
    assert!(
        html.contains("<section class=\"footnotes\""),
        "stored HTML is inserted as is"
    );
    assert!(html.contains("Oct 2, 2026"));
    assert!(html.contains("href=\"/tag/typography\" rel=\"tag\""));
    // Older post is "previous", newer is "next".
    assert!(html.contains("class=\"prev\" href=\"/a-tiny-http-server\""));
    assert!(html.contains("class=\"next\" href=\"/why-small-tools\""));
    assert_eq!(get("/no-such-post").0, 404);
}

#[test]
fn sample_posts_go_through_the_markdown_renderer() {
    let (status, html) = get("/a-tiny-http-server");
    assert_eq!(status, 200);
    assert!(html.contains("<div class=\"code\" data-lang=\"rust\">"));
    assert!(
        html.contains("1 min read"),
        "reading time comes from Rendered"
    );
}

#[test]
fn tag_and_archive_pages() {
    let (status, html) = get("/tag/tools");
    assert_eq!(status, 200);
    assert!(html.contains("<h1 class=\"list-title\">#tools</h1>"));
    assert!(html.contains("Notes in plain text"));
    assert!(!html.contains("Slow mornings"), "only posts with the tag");
    assert_eq!(get("/tag/nope").0, 404);

    let (status, html) = get("/archive/2025");
    assert_eq!(status, 200);
    assert!(html.contains("Posts from 2025"));
    assert!(html.contains("Static sites, revisited"));
    assert!(!html.contains("Slow mornings"));
    assert!(html.contains("href=\"/archive/2026\""));
    assert_eq!(get("/archive/1999").0, 404);
    // Paged like home: 2026 has six sample posts, five per page.
    let (status, html) = get("/archive/2026");
    assert_eq!(status, 200);
    assert!(html.contains("href=\"/archive/2026/page/2\" rel=\"next\">"));
    assert!(!html.contains("Slow mornings"));
    let (status, html) = get("/archive/2026/page/2");
    assert_eq!(status, 200);
    assert!(html.contains("Slow mornings"));
    assert!(html.contains("href=\"/archive/2026\" rel=\"prev\">"));
    assert_eq!(get("/archive/2026/page/3").0, 404);
    assert_eq!(get("/archive/2026/page/1"), (301, "/archive/2026".into()));
}

#[test]
fn not_found_is_friendly_and_suggests_posts() {
    let (status, html) = get("/a/b/c");
    assert_eq!(status, 404);
    assert!(html.contains("Page not found"));
    assert!(html.contains("noindex"));
    assert!(
        html.contains("Why I keep my tools small"),
        "suggests recent posts"
    );
}

#[test]
fn every_page_renders_well_under_the_cpu_budget() {
    // Native code is several times faster than wasm, so this is a smoke bound, not the budget:
    // the Worker reports real per-page time in its Server-Timing header.
    for path in [
        "/",
        "/reading-on-screens",
        "/tag/tools",
        "/archive/2026",
        "/x/y",
    ] {
        get(path);
        let start = std::time::Instant::now();
        for _ in 0..100 {
            get(path);
        }
        let per = start.elapsed() / 100;
        println!("render {path}: {per:?}");
        assert!(per.as_micros() < 2_000, "{path} took {per:?} natively");
    }
}
