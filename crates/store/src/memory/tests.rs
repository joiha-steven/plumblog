//! Smoke tests only; the full behaviour is pinned by the contract suite. Vietnamese is
//! escaped because code must be ASCII (see search.rs tests for the words).

use std::pin::pin;
use std::task::{Context, Poll, Waker};

use plumblog_model::{ErrorCode, PostId, Role, TagId, UserId};

use super::MemoryStore;
use crate::{
    NewRevision, NewUser, PostKind, PostOrder, PostQuery, PostStatus, PostStore, RedirectStore,
    SavePost, SiteStore, TaxonomyStore, UserStore,
};

/// The memory store never awaits anything, so one poll finishes it; avoids an executor
/// dependency just for tests.
fn run<F: Future>(f: F) -> F::Output {
    match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("memory store future was pending"),
    }
}

fn user(s: &MemoryStore, login: &str) -> UserId {
    run(s.create_user(NewUser {
        login: login.into(),
        slug: login.to_lowercase(),
        display_name: login.into(),
        bio: String::new(),
        role: Role::Writer,
        password_hash: None,
        created_at: 1,
    }))
    .unwrap()
}

fn post(author: UserId, slug: &str, status: PostStatus) -> SavePost {
    SavePost {
        id: None,
        expected_version: None,
        translation_group: None,
        kind: PostKind::Post,
        lang: "vi".into(),
        slug: slug.into(),
        status,
        published_at: Some(10),
        author_id: author,
        title: "\u{0110}\u{00E0} L\u{1EA1}t".into(),
        markdown: "# x".into(),
        html: "<h1>x</h1>".into(),
        toc_json: "[]".into(),
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
        search_text: "Con \u{0111}\u{01B0}\u{1EDD}ng NG\u{01AF}\u{1EDC}I \u{0111}i".into(),
        revision: Some(NewRevision {
            author_id: Some(author),
            created_at: 5,
        }),
        client_id: None,
        now: 5,
    }
}

#[test]
fn unawaited_write_does_nothing() {
    let s = MemoryStore::default();
    drop(s.create_user(NewUser {
        login: "a".into(),
        slug: "a".into(),
        display_name: "A".into(),
        bio: String::new(),
        role: Role::Owner,
        password_hash: None,
        created_at: 1,
    }));
    assert!(run(s.list_users()).unwrap().is_empty());
}

#[test]
fn login_is_unique_ignoring_ascii_case() {
    let s = MemoryStore::default();
    let id = user(&s, "Ann");
    let err = run(s.create_user(NewUser {
        login: "ANN".into(),
        slug: "other".into(),
        display_name: "x".into(),
        bio: String::new(),
        role: Role::Writer,
        password_hash: None,
        created_at: 1,
    }))
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    assert_eq!(run(s.find_user_by_login("aNN")).unwrap().unwrap().id, id);
}

#[test]
fn publish_search_rename_and_failed_save() {
    let s = MemoryStore::default();
    let a = user(&s, "ann");
    assert_eq!(run(s.content_version()).unwrap(), 1);

    let id = run(s.save_post(post(a, "da-lat", PostStatus::Published))).unwrap();
    assert_eq!(run(s.content_version()).unwrap(), 2);
    let found = run(s.search("vi", "duong nguoi \"da\" OR", 10)).unwrap();
    assert!(found.is_empty(), "OR is a word the post lacks");
    let found = run(s.search("vi", "\u{0111}uong NGUOI lat", 10)).unwrap();
    assert_eq!(found.iter().map(|p| p.id).collect::<Vec<_>>(), [id]);
    assert!(run(s.search("vi", "  *  ", 10)).unwrap().is_empty());

    // Renaming a published post leaves a redirect from the old path.
    let mut edit = post(a, "dalat", PostStatus::Published);
    edit.id = Some(id);
    edit.expected_version = Some(1);
    edit.now = 6;
    run(s.save_post(edit)).unwrap();
    assert_eq!(
        run(s.find_redirect("/vi/da-lat")).unwrap().as_deref(),
        Some("/vi/dalat")
    );
    assert_eq!(run(s.get_post(id)).unwrap().created_at, 5);
    assert_eq!(run(s.list_revisions(id)).unwrap().len(), 2);

    // A save that fails late (unknown tag) changes nothing at all.
    let version = run(s.content_version()).unwrap();
    let mut bad = post(a, "new", PostStatus::Published);
    bad.tag_ids = vec![TagId(999)];
    let err = run(s.save_post(bad)).unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidInput);
    assert_eq!(run(s.content_version()).unwrap(), version);
    assert!(run(s.find_post("vi", "new")).unwrap().is_none());

    // Ids are never reused, and failed inserts do not consume one.
    let t = run(s.ensure_tag("t", "T")).unwrap();
    let mut ok = post(a, "new", PostStatus::Draft);
    ok.tag_ids = vec![t, t];
    ok.now = 7;
    let id2 = run(s.save_post(ok)).unwrap();
    assert_eq!(id2, PostId(id.0 + 1));
    assert_eq!(run(s.get_post(id2)).unwrap().tag_ids, [t]);

    let page = run(s.list_posts(&PostQuery::new(PostOrder::UpdatedDesc, 1))).unwrap();
    assert_eq!(page[0].id, id2);
    let mut next = PostQuery::new(PostOrder::UpdatedDesc, 1);
    next.after = Some(PostOrder::UpdatedDesc.cursor(&page[0]));
    assert_eq!(run(s.list_posts(&next)).unwrap()[0].id, id);
}
