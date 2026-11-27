//! D1-only cases (feature `contract-tests`), run by the `/__contract` route next to the shared
//! contract. They check the mechanics the shared suite cannot reach from outside: a guard
//! aborts its whole batch, and a save that read a row another request then changed writes
//! nothing and is retried on fresh data, a save from an older version of the same path
//! writes nothing, and a demotion prepared while another owner existed cannot leave none.

use plumblog_model::{PostId, Role, UserId};
use plumblog_store::{
    NewRevision, NewUser, PostKind, PostStatus, PostStore, RedirectStore, SavePost, SettingsStore,
    SiteStore, TaxonomyStore, UserStore,
};

use super::post_save::Old;
use super::{D1Store, Failure, guard, text};

pub const CASES: &[&str] = &[
    "d1_guard_aborts_whole_batch",
    "d1_stale_save_writes_nothing",
    "d1_stale_version_writes_nothing",
    "d1_last_owner_guard_in_batch",
];

/// `None` if `name` is not a D1-only case.
pub async fn run(name: &str, db: &D1Store) -> Option<Result<(), String>> {
    let out = match name {
        "d1_guard_aborts_whole_batch" => guard_aborts_whole_batch(db).await,
        "d1_stale_save_writes_nothing" => stale_save_writes_nothing(db).await,
        "d1_stale_version_writes_nothing" => stale_version_writes_nothing(db).await,
        "d1_last_owner_guard_in_batch" => last_owner_guard_in_batch(db).await,
        _ => return None,
    };
    Some(out.map_err(|e| format!("{name}: {e}")))
}

fn check(what: &str, ok: bool) -> Result<(), String> {
    if ok { Ok(()) } else { Err(what.to_owned()) }
}

async fn guard_aborts_whole_batch(db: &D1Store) -> Result<(), String> {
    let put = db
        .stmt(
            "selftest_put",
            "INSERT INTO settings (key, value_json, updated_at) VALUES (?, '1', 0)",
            &[text("k")],
        )
        .map_err(|e| e.to_string())?;
    let never = guard(db, "selftest_guard", "0", &[]).map_err(|e| e.to_string())?;
    let out = db.batch_guarded("selftest", vec![put, never]).await;
    check(
        "a false guard is reported as Failure::Guard",
        matches!(out, Err(Failure::Guard)),
    )?;
    let left = db.get_setting("k").await.map_err(|e| e.to_string())?;
    check(
        "the statement before the guard was rolled back",
        left.is_none(),
    )?;
    let ok = guard(db, "selftest_guard", "1", &[]).map_err(|e| e.to_string())?;
    check(
        "a true guard lets the batch commit",
        db.batch_guarded("selftest", vec![ok]).await.is_ok(),
    )
}

fn live_post(author: UserId, slug: &str) -> SavePost {
    SavePost {
        id: None,
        expected_version: None,
        translation_group: None,
        kind: PostKind::Post,
        lang: "vi".to_owned(),
        slug: slug.to_owned(),
        status: PostStatus::Published,
        published_at: Some(1_000),
        author_id: author,
        title: slug.to_owned(),
        markdown: String::new(),
        html: String::new(),
        toc_json: "[]".to_owned(),
        excerpt: String::new(),
        word_count: 0,
        reading_minutes: 0,
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
    }
}

async fn owner(db: &D1Store, login: &str) -> Result<UserId, String> {
    db.create_user(NewUser {
        login: login.to_owned(),
        slug: login.to_owned(),
        display_name: login.to_owned(),
        bio: String::new(),
        role: Role::Owner,
        password_hash: None,
        created_at: 1_000,
    })
    .await
    .map_err(|e| e.to_string())
}

async fn stale_save_writes_nothing(db: &D1Store) -> Result<(), String> {
    let e = |e: plumblog_model::Error| e.to_string();
    let author = owner(db, "a").await?;
    let id: PostId = db.save_post(live_post(author, "now")).await.map_err(e)?;
    let v = db.content_version().await.map_err(e)?;
    // As if this request read the post before another request renamed it to /vi/now.
    let stale = Old {
        version: 1,
        status: PostStatus::Published,
        lang: "vi".to_owned(),
        slug: "before".to_owned(),
    };
    let mut next = live_post(author, "after");
    next.id = Some(id);
    next.expected_version = Some(1);
    let out = db.update_post(&next, id, &stale).await;
    check(
        "a stale read fires the guard",
        matches!(out, Err(Failure::Guard)),
    )?;
    let post = db.get_post(id).await.map_err(e)?;
    check("the post is unchanged", post.slug == "now")?;
    check(
        "no redirect was written from the stale path",
        db.list_redirects().await.map_err(e)?.is_empty(),
    )?;
    check(
        "the version was not bumped",
        db.content_version().await.map_err(e)? == v,
    )?;
    // The public method re-reads and succeeds, redirecting from the path really there.
    db.save_post(next).await.map_err(e)?;
    check(
        "the retried save redirects from the real old path",
        db.find_redirect("/vi/now").await.map_err(e)?.as_deref() == Some("/vi/after"),
    )
}

/// Only the version tells the read apart from the row: status, lang and slug still match.
async fn stale_version_writes_nothing(db: &D1Store) -> Result<(), String> {
    let e = |e: plumblog_model::Error| e.to_string();
    let author = owner(db, "a").await?;
    let kept = db.ensure_tag("kept", "Kept").await.map_err(e)?;
    let lost = db.ensure_tag("lost", "Lost").await.map_err(e)?;
    let revision = Some(NewRevision {
        author_id: Some(author),
        created_at: 1_000,
    });
    let mut first = live_post(author, "same");
    first.tag_ids = vec![kept];
    first.revision = revision;
    let id = db.save_post(first.clone()).await.map_err(e)?;
    // Another request saves once more after this request read version 1.
    let mut second = first.clone();
    second.id = Some(id);
    second.expected_version = Some(1);
    "second".clone_into(&mut second.title);
    db.save_post(second).await.map_err(e)?;
    let v = db.content_version().await.map_err(e)?;
    let stale = Old {
        version: 1,
        status: PostStatus::Published,
        lang: "vi".to_owned(),
        slug: "same".to_owned(),
    };
    let mut next = first;
    next.id = Some(id);
    next.expected_version = Some(1);
    "stale".clone_into(&mut next.title);
    next.tag_ids = vec![lost];
    let out = db.update_post(&next, id, &stale).await;
    check(
        "an older version fires the guard",
        matches!(out, Err(Failure::Guard)),
    )?;
    let post = db.get_post(id).await.map_err(e)?;
    check("the title is unchanged", post.title == "second")?;
    check("the version is unchanged", post.version == 2)?;
    check("the tags are unchanged", post.tag_ids == vec![kept])?;
    check(
        "no revision was added",
        db.list_revisions(id).await.map_err(e)?.len() == 2,
    )?;
    check(
        "the content version was not bumped",
        db.content_version().await.map_err(e)? == v,
    )
}

/// A's demotion is built while B is an owner; B is demoted before it runs.
async fn last_owner_guard_in_batch(db: &D1Store) -> Result<(), String> {
    let e = |e: plumblog_model::Error| e.to_string();
    let a = owner(db, "a").await?;
    let b = owner(db, "b").await?;
    let demote_a = db.demote_stmts(a, Role::Writer, 2_000).map_err(e)?;
    db.set_role(b, Role::Writer, 2_000).await.map_err(e)?;
    let out = db.batch_guarded("set_role", demote_a).await;
    check(
        "demoting the last owner fires the guard",
        matches!(out, Err(Failure::Guard)),
    )?;
    check(
        "A is still owner",
        db.get_user(a).await.map_err(e)?.role == Role::Owner,
    )?;
    check(
        "B was demoted",
        db.get_user(b).await.map_err(e)?.role == Role::Writer,
    )
}
