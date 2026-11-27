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
