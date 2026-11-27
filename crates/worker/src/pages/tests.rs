use super::routes::{Route, parse};
use super::source::SampleSource;
use super::{Outcome, render};
use std::pin::pin;
use std::task::{Context, Poll, Waker};

/// The sample source never awaits, so one poll finishes; no executor dependency for tests.
fn run<F: Future>(f: F) -> F::Output {
    match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("sample source future was pending"),
    }
}

fn get(path: &str) -> (u16, String) {
    match run(render(SampleSource::get(), "https://blog.test", path)).unwrap() {
        Outcome::Page { status, html } => (status, html),
        Outcome::Redirect(to) => (301, to),
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
    assert_eq!(parse("/archive/2026"), Route::Archive { year: 2026 });
    assert_eq!(parse("/archive/26"), Route::NotFound);
    assert_eq!(parse("/a/b"), Route::NotFound);
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
        html.contains("<section class=\"footnotes\">"),
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
