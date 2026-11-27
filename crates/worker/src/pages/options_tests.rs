//! Stored theme options reaching rendered pages through the store (in-memory here; D1 in
//! tools/worker-test/options.test.mjs).

use super::db::{Db, ExtSource};
use super::source::SampleSource;
use super::tests_support::run;
use super::{Outcome, render};
use plumblog_store::memory::MemoryStore;
use plumblog_store::{OptionOwner, SettingsStore};

fn page(db: &MemoryStore, path: &str) -> String {
    match run(render(SampleSource::get(), &Db(db), &Db(db), path, "")).unwrap() {
        Outcome::Page { body, .. } => body,
        Outcome::Redirect(to) => panic!("{path} redirected to {to}"),
    }
}

fn put(db: &MemoryStore, key: &str, json: &str) {
    run(db.put_option(OptionOwner::Theme, "minimal", key, json, 1)).unwrap();
}

#[test]
fn stored_options_change_the_page_and_bump_the_version() {
    let db = MemoryStore::default();
    let before = page(&db, "/");
    let v0 = run(Db(&db).content_version()).unwrap();
    assert!(before.contains("<p class=\"excerpt\">"));
    put(&db, "excerpt_words", "0");
    put(
        &db,
        "accent",
        "{\"light\":\"#123456\",\"dark\":\"#abcdef\"}",
    );
    assert!(
        run(Db(&db).content_version()).unwrap() > v0,
        "cache keys change"
    );
    let after = page(&db, "/");
    // 0 hides cut excerpts; ones the author wrote stay.
    let excerpts = |html: &str| html.matches("<p class=\"excerpt\">").count();
    assert!(
        excerpts(&after) < excerpts(&before),
        "{} -> {}",
        excerpts(&before),
        excerpts(&after)
    );
    assert!(after.contains("--c-accent:light-dark(#123456,#abcdef)"));
}

#[test]
fn a_bad_stored_value_never_breaks_a_page() {
    let db = MemoryStore::default();
    let default = page(&db, "/");
    put(&db, "excerpt_words", "\"lots\"");
    put(
        &db,
        "accent",
        "{\"light\":\"red}body{display:none\",\"dark\":\"#000\"}",
    );
    put(&db, "style", "\"magazine\"");
    assert_eq!(
        page(&db, "/"),
        default,
        "every value fell back to its default"
    );
}

/// The store answers the content version but fails option and plugin reads.
struct Flaky;

impl ExtSource for Flaky {
    fn content_version(&self) -> impl Future<Output = Result<u64, String>> {
        std::future::ready(Ok(7))
    }
    fn theme_options(
        &self,
        _: &str,
    ) -> impl Future<Output = Result<Vec<(String, String)>, String>> {
        std::future::ready(Err("d1 timeout".to_owned()))
    }
    fn enabled_plugins(
        &self,
    ) -> impl Future<Output = Result<Vec<(String, Vec<(String, String)>)>, String>> {
        std::future::ready(Err("d1 timeout".to_owned()))
    }
}

#[test]
fn a_failed_option_read_serves_defaults_that_nothing_caches() {
    let db = MemoryStore::default();
    for path in ["/", "/reading-on-screens", "/feed.xml", "/no-such-page"] {
        let out = run(render(SampleSource::get(), &Flaky, &Db(&db), path, "")).unwrap();
        let Outcome::Page {
            status, cacheable, ..
        } = out
        else {
            panic!("{path} redirected");
        };
        assert!(!cacheable, "{path}");
        assert_eq!(
            super::cache_policy(status, cacheable),
            (false, "no-store"),
            "{path}: no ETag, no edge copy"
        );
    }
    // The same page from a healthy store may be cached.
    let Outcome::Page { cacheable, .. } =
        run(render(SampleSource::get(), &Db(&db), &Db(&db), "/", "")).unwrap()
    else {
        panic!("redirect");
    };
    assert!(cacheable);
    assert!(super::cache_policy(200, true).0);
    assert!(!super::cache_policy(404, true).0, "404s are never stored");
}

#[test]
fn a_preview_is_the_post_page_with_the_site_theme_and_options() {
    let db = MemoryStore::default();
    put(
        &db,
        "accent",
        "{\"light\":\"#123456\",\"dark\":\"#abcdef\"}",
    );
    let md = "## Ch\u{1b0}\u{1a1}ng m\u{1ed9}t\n\nC\u{f3} $x^2$ v\u{e0} <b>raw</b>.\n\n```rust\nfn main() {}\n```\n";
    let html = run(super::preview(
        SampleSource::get(),
        &Db(&db),
        "B\u{e0}i <th\u{1ed1}ng>",
        md,
        "vi",
    ))
    .unwrap();
    assert!(html.contains("<html lang=\"vi\""), "the draft's language");
    assert!(
        html.contains("B\u{e0}i &lt;th\u{1ed1}ng&gt;"),
        "the title is escaped"
    );
    assert!(html.contains("<math"), "math is MathML, as when published");
    assert!(
        html.contains("data-lang=\"rust\""),
        "code keeps its language for the highlighter"
    );
    assert!(!html.contains("<b>"), "raw HTML in Markdown is dropped");
    assert!(
        html.contains("light-dark(#123456,#abcdef)"),
        "stored theme options apply"
    );
    assert!(
        !html.to_ascii_lowercase().contains("<script"),
        "the preview frame runs no theme code"
    );
    assert!(
        html.contains("</html>"),
        "nothing after the last script is lost"
    );
}
