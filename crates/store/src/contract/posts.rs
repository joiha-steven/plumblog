//! Saving posts, their lifecycle (trash, restore, delete, `publish_due`), revisions and
//! translations.

use plumblog_model::{ErrorCode, MediaId, PostId, RevisionId, SeriesId, TagId, UserId};

use super::kit::{
    Check, T0, asc, both, desc, eq, fails, ok, post, save, that, update, user, version, version_is,
};
use crate::{
    CommentStatus, NewComment, NewRevision, PostKind, PostOrder, PostQuery, PostStatus,
    PostSummary, SavePost, Store,
};

pub async fn post_insert_and_update<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let tag = ok("ensure_tag", db.ensure_tag("t", "T").await)?;
    let mut p = post(u, "vi", "hello", PostStatus::Draft);
    p.kind = PostKind::Page;
    p.excerpt = "ex".into();
    p.word_count = 42;
    p.reading_minutes = 3;
    p.pinned = true;
    p.comments_enabled = false;
    p.tag_ids = vec![tag];
    p.now = 500;
    let id = save(db, p.clone()).await?;
    let got = ok("get_post", db.get_post(id).await)?;
    eq!("translation_group = own id", got.translation_group, id)?;
    eq!(
        "created_at = updated_at = now",
        (got.created_at, got.updated_at),
        (500, 500),
    )?;
    eq!("trashed_from", got.trashed_from, None)?;
    eq!(
        "fields",
        (
            got.kind,
            got.lang.as_str(),
            got.slug.as_str(),
            got.status,
            got.published_at,
            got.author_id,
            got.title.as_str(),
            got.markdown.as_str(),
            got.html.as_str(),
            got.toc_json.as_str(),
        ),
        (
            PostKind::Page,
            "vi",
            "hello",
            PostStatus::Draft,
            None,
            u,
            "Title hello",
            "# hello",
            "<h1>hello</h1>",
            "[]",
        ),
    )?;
    eq!(
        "more fields",
        (
            got.excerpt.as_str(),
            got.word_count,
            got.reading_minutes,
            got.pinned,
            got.comments_enabled,
            got.tag_ids.clone(),
        ),
        ("ex", 42, 3, true, false, vec![tag]),
    )?;
    let found = ok("find_post", db.find_post("vi", "hello").await)?;
    eq!("find_post", found, Some(got.clone()))?;
    eq!(
        "find_post other lang",
        ok("find_post", db.find_post("en", "hello").await)?,
        None,
    )?;

    let mut edit = update(db, id, p).await?;
    edit.status = PostStatus::Published;
    edit.published_at = Some(700);
    edit.title = "New".into();
    edit.tag_ids = Vec::new();
    edit.translation_group = Some(PostId(id.0 + 1000));
    edit.now = 800;
    eq!("update returns the id", save(db, edit).await?, id)?;
    let after = ok("get_post", db.get_post(id).await)?;
    eq!("created_at kept", after.created_at, 500)?;
    eq!("updated_at = now", after.updated_at, 800)?;
    eq!("group kept on update", after.translation_group, id)?;
    eq!(
        "updated fields",
        (
            after.status,
            after.published_at,
            after.title.as_str(),
            after.tag_ids,
        ),
        (PostStatus::Published, Some(700), "New", Vec::new()),
    )?;
    fails(
        "get_post missing",
        db.get_post(PostId(id.0 + 1000)).await,
        ErrorCode::PostNotFound,
    )
}

/// Breaks one field of a valid post: `(post, author, an id no row has)`.
type Edit = fn(&mut SavePost, UserId, i64);

/// Every rejection of `save_post`, each leaving the store as it was.
pub async fn post_save_errors<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let existing = save(db, post(u, "vi", "taken", PostStatus::Draft)).await?;
    let v = version(db).await?;
    let base = || post(u, "vi", "new", PostStatus::Published);
    let far = existing.0 + 1000;

    let invalid: [(&str, Edit); 9] = [
        ("status trashed", |p, _, _| p.status = PostStatus::Trashed),
        ("published without published_at", |p, _, _| {
            p.published_at = None;
        }),
        ("scheduled without published_at", |p, _, _| {
            p.status = PostStatus::Scheduled;
            p.published_at = None;
        }),
        ("missing author", |p, u, _| p.author_id = UserId(u.0 + 1000)),
        ("missing cover", |p, _, far| {
            p.cover_media_id = Some(MediaId(far));
        }),
        ("missing series", |p, _, far| {
            p.series_id = Some(SeriesId(far));
        }),
        ("missing tag", |p, _, far| p.tag_ids = vec![TagId(far)]),
        ("missing revision author", |p, u, _| {
            p.revision = Some(NewRevision {
                author_id: Some(UserId(u.0 + 1000)),
                created_at: T0,
            });
        }),
        ("missing translation group", |p, _, far| {
            p.translation_group = Some(PostId(far));
        }),
    ];
    for (what, edit) in invalid {
        let mut p = base();
        edit(&mut p, u, far);
        fails(what, db.save_post(p).await, ErrorCode::InvalidInput)?;
    }
    let mut p = base();
    p.slug = "taken".into();
    fails(
        "(lang, slug) taken",
        db.save_post(p).await,
        ErrorCode::Conflict,
    )?;
    fails(
        "update missing post",
        db.save_post(update(db, PostId(far), base()).await?).await,
        ErrorCode::PostNotFound,
    )?;
    ok("trash_post", db.trash_post(existing, T0).await)?;
    fails(
        "update a trashed post",
        db.save_post(update(db, existing, post(u, "vi", "taken", PostStatus::Draft)).await?)
            .await,
        ErrorCode::InvalidInput,
    )?;
    version_is(db, "rejected saves", v).await?;
    let all = ok(
        "list_posts",
        db.list_posts(&PostQuery::new(PostOrder::UpdatedDesc, 100))
            .await,
    )?;
    eq!(
        "rejected saves add no post",
        all.iter().map(|p| p.id).collect::<Vec<_>>(),
        vec![existing],
    )
}

/// A save that fails late leaves no post, revision, search hit, redirect change or bump.
pub async fn post_save_is_atomic<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let good = ok("ensure_tag", db.ensure_tag("ok", "Ok").await)?;
    let bad = TagId(good.0 + 1000);
    let v = version(db).await?;
    let mut p = post(u, "vi", "atomic", PostStatus::Published);
    p.search_text = "zebra".into();
    p.tag_ids = vec![good, bad];
    p.revision = Some(NewRevision {
        author_id: Some(u),
        created_at: T0,
    });
    fails(
        "insert with a missing tag",
        db.save_post(p.clone()).await,
        ErrorCode::InvalidInput,
    )?;
    eq!(
        "no post",
        ok("find_post", db.find_post("vi", "atomic").await)?,
        None,
    )?;
    eq!(
        "no search hit",
        ok("search", db.search("vi", "zebra", 10).await)?.len(),
        0,
    )?;
    let mut by_tag = PostQuery::new(PostOrder::UpdatedDesc, 10);
    by_tag.tag = Some(good);
    eq!(
        "no tag link",
        ok("list_posts", db.list_posts(&by_tag).await)?.len(),
        0,
    )?;
    version_is(db, "failed insert", v).await?;

    // Update of a published post: rename + new text, failing on the tag.
    let mut live = post(u, "vi", "old", PostStatus::Published);
    live.search_text = "apple".into();
    live.revision = Some(NewRevision {
        author_id: Some(u),
        created_at: T0,
    });
    let id = save(db, live.clone()).await?;
    let before = ok("get_post", db.get_post(id).await)?;
    let v = version(db).await?;
    let mut edit = update(db, id, live).await?;
    edit.slug = "renamed".into();
    edit.search_text = "zebra".into();
    edit.title = "Changed".into();
    edit.tag_ids = vec![bad];
    fails(
        "update with a missing tag",
        db.save_post(edit).await,
        ErrorCode::InvalidInput,
    )?;
    eq!(
        "post unchanged",
        ok("get_post", db.get_post(id).await)?,
        before,
    )?;
    eq!(
        "one revision only",
        ok("list_revisions", db.list_revisions(id).await)?.len(),
        1,
    )?;
    eq!(
        "no redirect from the old path",
        ok("find_redirect", db.find_redirect("/vi/old").await)?,
        None,
    )?;
    eq!(
        "old words still found",
        ok("search", db.search("vi", "apple", 10).await)?.len(),
        1,
    )?;
    eq!(
        "new words not found",
        ok("search", db.search("vi", "zebra", 10).await)?.len(),
        0,
    )?;
    version_is(db, "failed update", v).await
}

/// Bumps when the old or new status is published, and only then.
pub async fn post_save_versions<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let mut v = version(db).await?;
    let d = post(u, "vi", "d", PostStatus::Draft);
    let id = save(db, d.clone()).await?;
    version_is(db, "insert draft", v).await?;
    save(db, update(db, id, d.clone()).await?).await?;
    version_is(db, "draft -> draft", v).await?;
    let mut sch = update(db, id, post(u, "vi", "d", PostStatus::Scheduled)).await?;
    sch.published_at = Some(5000);
    save(db, sch).await?;
    version_is(db, "draft -> scheduled", v).await?;
    save(
        db,
        update(db, id, post(u, "vi", "d", PostStatus::Published)).await?,
    )
    .await?;
    v += 1;
    version_is(db, "scheduled -> published", v).await?;
    save(
        db,
        update(db, id, post(u, "vi", "d", PostStatus::Published)).await?,
    )
    .await?;
    v += 1;
    version_is(db, "published -> published", v).await?;
    save(db, update(db, id, d).await?).await?;
    v += 1;
    version_is(db, "published -> draft", v).await?;
    save(db, post(u, "vi", "p", PostStatus::Published)).await?;
    version_is(db, "insert published", v + 1).await
}

pub async fn trash_restore_delete<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let live = save(db, post(u, "vi", "live", PostStatus::Published)).await?;
    let draft = save(db, post(u, "vi", "draft", PostStatus::Draft)).await?;
    let missing = PostId(live.0.max(draft.0) + 1000);
    let mut v = version(db).await?;

    ok("trash_post published", db.trash_post(live, 2000).await)?;
    v += 1;
    version_is(db, "trash published", v).await?;
    let t = ok("get_post", db.get_post(live).await)?;
    eq!(
        "trashed",
        (t.status, t.trashed_from, t.updated_at),
        (PostStatus::Trashed, Some(PostStatus::Published), 2000),
    )?;
    fails(
        "trash twice",
        db.trash_post(live, 2001).await,
        ErrorCode::InvalidInput,
    )?;
    fails(
        "trash missing",
        db.trash_post(missing, 1).await,
        ErrorCode::PostNotFound,
    )?;
    ok("trash_post draft", db.trash_post(draft, 2000).await)?;
    version_is(db, "trash draft", v).await?;

    ok("restore_post published", db.restore_post(live, 3000).await)?;
    v += 1;
    version_is(db, "restore to published", v).await?;
    let r = ok("get_post", db.get_post(live).await)?;
    eq!(
        "restored",
        (r.status, r.trashed_from, r.updated_at),
        (PostStatus::Published, None, 3000),
    )?;
    fails(
        "restore not trashed",
        db.restore_post(live, 3001).await,
        ErrorCode::InvalidInput,
    )?;
    fails(
        "restore missing",
        db.restore_post(missing, 1).await,
        ErrorCode::PostNotFound,
    )?;
    ok("restore_post draft", db.restore_post(draft, 3000).await)?;
    version_is(db, "restore to draft", v).await?;
    eq!(
        "draft restored",
        ok("get_post", db.get_post(draft).await)?.status,
        PostStatus::Draft,
    )?;

    fails(
        "delete not trashed",
        db.delete_post(live).await,
        ErrorCode::InvalidInput,
    )?;
    fails(
        "delete missing",
        db.delete_post(missing).await,
        ErrorCode::PostNotFound,
    )?;
    ok("trash_post", db.trash_post(draft, 4000).await)?;
    ok("delete_post", db.delete_post(draft).await)?;
    version_is(db, "delete a trashed post", v).await?;
    fails(
        "get deleted",
        db.get_post(draft).await,
        ErrorCode::PostNotFound,
    )?;
    eq!(
        "deleted path is free",
        ok("find_post", db.find_post("vi", "draft").await)?,
        None,
    )?;
    save(db, post(u, "vi", "draft", PostStatus::Draft))
        .await
        .map(drop)
}

/// Deleting a post removes its tags, revisions, comments and search row, nothing else.
pub async fn post_delete_cascades<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let tag = ok("ensure_tag", db.ensure_tag("t", "T").await)?;
    let mut p = post(u, "en", "gone", PostStatus::Published);
    p.tag_ids = vec![tag];
    p.search_text = "walrus".into();
    p.revision = Some(NewRevision {
        author_id: Some(u),
        created_at: T0,
    });
    let id = save(db, p).await?;
    let mut sib = post(u, "vi", "stays", PostStatus::Published);
    sib.translation_group = Some(id);
    sib.tag_ids = vec![tag];
    sib.search_text = "walrus".into();
    let sib = save(db, sib).await?;
    let rev = ok("list_revisions", db.list_revisions(id).await)?
        .first()
        .map(|r| r.id)
        .ok_or("no revision saved")?;
    let c = ok(
        "add_comment",
        db.add_comment(comment(id, None, CommentStatus::Approved))
            .await,
    )?;
    ok(
        "add_comment",
        db.add_comment(comment(id, Some(c), CommentStatus::Pending))
            .await,
    )?;
    ok(
        "add_comment",
        db.add_comment(comment(sib, None, CommentStatus::Approved))
            .await,
    )?;
    ok("trash_post", db.trash_post(id, 2000).await)?;
    ok("delete_post", db.delete_post(id).await)?;

    let mut by_tag = PostQuery::new(PostOrder::UpdatedDesc, 10);
    by_tag.tag = Some(tag);
    let tagged: Vec<PostId> = ok("list_posts", db.list_posts(&by_tag).await)?
        .iter()
        .map(|p| p.id)
        .collect();
    eq!("post_tags cascade", tagged, vec![sib])?;
    that(
        "tag itself stays",
        ok("find_tag", db.find_tag("t").await)?.is_some(),
    )?;
    eq!(
        "revisions cascade",
        ok("list_revisions", db.list_revisions(id).await)?.len(),
        0,
    )?;
    fails(
        "get_revision of deleted post",
        db.get_revision(rev).await,
        ErrorCode::NotFound,
    )?;
    for st in [CommentStatus::Approved, CommentStatus::Pending] {
        eq!(
            "comments cascade",
            ok("list_comments", db.list_comments(id, st).await)?.len(),
            0,
        )?;
    }
    eq!(
        "sibling comments stay",
        ok(
            "list_comments",
            db.list_comments(sib, CommentStatus::Approved).await,
        )?
        .len(),
        1,
    )?;
    let hits: Vec<PostId> = ok("search", db.search("vi", "walrus", 10).await)?
        .iter()
        .map(|p| p.id)
        .collect();
    eq!("sibling still searchable", hits, vec![sib])?;
    eq!(
        "deleted post not searchable",
        ok("search", db.search("en", "walrus", 10).await)?.len(),
        0,
    )?;
    let group: Vec<PostId> = ok("list_translations", db.list_translations(id).await)?
        .iter()
        .map(|p| p.id)
        .collect();
    eq!("translation sibling untouched", group, vec![sib])
}

pub fn comment(
    post: PostId,
    parent: Option<plumblog_model::CommentId>,
    status: CommentStatus,
) -> NewComment {
    NewComment {
        post_id: post,
        parent_id: parent,
        author_name: "Reader".into(),
        body: "Nice".into(),
        body_html: None,
        status,
        created_at: T0,
    }
}

pub async fn publish_due<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let mut ids = Vec::new();
    for (slug, at) in [("a", 100), ("b", 200), ("c", 300)] {
        let mut p = post(u, "vi", slug, PostStatus::Scheduled);
        p.published_at = Some(at);
        p.now = 50;
        ids.push(save(db, p).await?);
    }
    let mut d = post(u, "vi", "draft", PostStatus::Draft);
    d.published_at = Some(10);
    let draft = save(db, d).await?;
    let v = version(db).await?;
    let done = ok("publish_due", db.publish_due(200, 100).await)?;
    eq!(
        "publishes published_at <= now, ascending",
        done,
        asc(&ids[..2]),
    )?;
    version_is(db, "publish_due with due posts (once)", v + 1).await?;
    for id in &ids[..2] {
        let p = ok("get_post", db.get_post(*id).await)?;
        eq!(
            "published, updated_at kept",
            (p.status, p.updated_at),
            (PostStatus::Published, 50),
        )?;
    }
    eq!(
        "later post stays scheduled",
        ok("get_post", db.get_post(ids[2]).await)?.status,
        PostStatus::Scheduled,
    )?;
    eq!(
        "draft is never published",
        ok("get_post", db.get_post(draft).await)?.status,
        PostStatus::Draft,
    )?;
    eq!(
        "nothing due",
        ok("publish_due", db.publish_due(299, 100).await)?,
        Vec::new(),
    )?;
    version_is(db, "publish_due with nothing due", v + 1).await
}

/// A limit publishes the earliest due first and leaves the rest due for the next run.
pub async fn publish_due_limit<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let mut ids = Vec::new();
    // Saved out of due order, so id order and due order differ.
    for (slug, at) in [("late", 300), ("early", 100), ("mid", 200), ("tie", 100)] {
        let mut p = post(u, "vi", slug, PostStatus::Scheduled);
        p.published_at = Some(at);
        p.now = 50;
        ids.push(save(db, p).await?);
    }
    let (late, early, mid, tie) = (ids[0], ids[1], ids[2], ids[3]);
    eq!(
        "earliest two (published_at, id), ascending ids",
        ok("publish_due", db.publish_due(400, 2).await)?,
        asc(&[early, tie]),
    )?;
    eq!(
        "the rest stay scheduled",
        (
            ok("get_post", db.get_post(mid).await)?.status,
            ok("get_post", db.get_post(late).await)?.status,
        ),
        (PostStatus::Scheduled, PostStatus::Scheduled),
    )?;
    eq!(
        "limit 0 publishes nothing",
        ok("publish_due", db.publish_due(400, 0).await)?,
        Vec::new(),
    )?;
    eq!(
        "next run takes the rest",
        ok("publish_due", db.publish_due(400, 2).await)?,
        asc(&[late, mid]),
    )
}

pub async fn revisions<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let mut p = post(u, "vi", "r", PostStatus::Draft);
    p.revision = Some(NewRevision {
        author_id: Some(u),
        created_at: 100,
    });
    let id = save(db, p.clone()).await?;
    let mut e = update(db, id, p.clone()).await?;
    e.title = "Second".into();
    e.markdown = "two".into();
    e.revision = Some(NewRevision {
        author_id: None,
        created_at: 100,
    });
    save(db, e).await?;
    let mut e = update(db, id, p).await?;
    e.title = "No snapshot".into();
    e.revision = None;
    save(db, e).await?;
    let revs = ok("list_revisions", db.list_revisions(id).await)?;
    let ids: Vec<RevisionId> = revs.iter().map(|r| r.id).collect();
    eq!(
        "two revisions, newest first (created_at, id)",
        ids.clone(),
        desc(&ids),
    )?;
    eq!("revision count", ids.len(), 2)?;
    let newest = ok("get_revision", db.get_revision(ids[0]).await)?;
    eq!(
        "newest snapshot is the new title and markdown",
        (
            newest.post_id,
            newest.author_id,
            newest.title.as_str(),
            newest.markdown.as_str(),
            newest.created_at,
        ),
        (id, None, "Second", "two", 100),
    )?;
    let oldest = ok("get_revision", db.get_revision(ids[1]).await)?;
    eq!(
        "oldest snapshot",
        (oldest.author_id, oldest.title.as_str()),
        (Some(u), "Title r"),
    )?;
    fails(
        "get_revision missing",
        db.get_revision(RevisionId(ids[0].0 + 1000)).await,
        ErrorCode::NotFound,
    )?;
    eq!(
        "list_revisions unknown post",
        ok(
            "list_revisions",
            db.list_revisions(PostId(id.0 + 1000)).await,
        )?
        .len(),
        0,
    )
}

pub async fn translations<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let vi = save(db, post(u, "vi", "xin-chao", PostStatus::Published)).await?;
    let mut en = post(u, "en", "hello", PostStatus::Draft);
    en.translation_group = Some(vi);
    let en = save(db, en).await?;
    let mut de = post(u, "de", "hallo", PostStatus::Draft);
    de.translation_group = Some(vi);
    let de = save(db, de).await?;
    let other = save(db, post(u, "vi", "other", PostStatus::Draft)).await?;
    let group: Vec<(PostId, String)> = ok("list_translations", db.list_translations(vi).await)?
        .into_iter()
        .map(|p: PostSummary| (p.id, p.lang))
        .collect();
    eq!(
        "list_translations by lang",
        group,
        vec![(de, "de".into()), (en, "en".into()), (vi, "vi".into())],
    )?;
    eq!(
        "group of a translation",
        ok("get_post", db.get_post(en).await)?.translation_group,
        vi,
    )?;
    let mut dup = post(u, "en", "hello-2", PostStatus::Draft);
    dup.translation_group = Some(vi);
    fails(
        "(group, lang) taken",
        db.save_post(dup).await,
        ErrorCode::Conflict,
    )?;
    // Moving an existing post into a group is not possible: the group is fixed.
    let mut moved = update(db, other, post(u, "en", "other", PostStatus::Draft)).await?;
    moved.translation_group = Some(vi);
    save(db, moved).await?;
    eq!(
        "update ignores translation_group",
        ok("get_post", db.get_post(other).await)?.translation_group,
        other,
    )?;
    // Changing lang into a taken one of the same group conflicts.
    fails(
        "update into a taken (group, lang)",
        db.save_post(update(db, en, post(u, "vi", "hello", PostStatus::Draft)).await?)
            .await,
        ErrorCode::Conflict,
    )?;
    // A group stays joinable after its first post is gone.
    ok("trash_post", db.trash_post(vi, 2000).await)?;
    ok("delete_post", db.delete_post(vi).await)?;
    let mut fr = post(u, "fr", "bonjour", PostStatus::Draft);
    fr.translation_group = Some(vi);
    let fr = save(db, fr).await?;
    let group: Vec<PostId> = ok("list_translations", db.list_translations(vi).await)?
        .iter()
        .map(|p| p.id)
        .collect();
    eq!("group after delete", group, vec![de, en, fr])?;
    eq!(
        "unknown group",
        ok(
            "list_translations",
            db.list_translations(PostId(fr.0 + 1000)).await,
        )?
        .len(),
        0,
    )
}

/// The current `version` of a post.
async fn row_version<S: Store>(db: &S, id: PostId) -> super::kit::Got<i64> {
    Ok(ok("get_post", db.get_post(id).await)?.version)
}

/// A save names the version it edited: insert must not, update must.
pub async fn post_version_rules<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let mut bad = post(u, "vi", "new", PostStatus::Draft);
    bad.expected_version = Some(1);
    fails(
        "insert with expected_version",
        db.save_post(bad).await,
        ErrorCode::InvalidInput,
    )?;
    eq!(
        "rejected insert adds no post",
        ok("find_post", db.find_post("vi", "new").await)?,
        None,
    )?;
    let id = save(db, post(u, "vi", "new", PostStatus::Draft)).await?;
    eq!("insert starts at version 1", row_version(db, id).await?, 1)?;
    let listed = ok(
        "list_posts",
        db.list_posts(&PostQuery::new(PostOrder::UpdatedDesc, 10))
            .await,
    )?;
    eq!(
        "summary carries the version",
        listed.iter().map(|p| p.version).collect::<Vec<_>>(),
        vec![1],
    )?;
    let mut blind = post(u, "vi", "new", PostStatus::Draft);
    blind.id = Some(id);
    blind.title = "Blind".into();
    fails(
        "update without expected_version",
        db.save_post(blind).await,
        ErrorCode::InvalidInput,
    )?;
    eq!(
        "rejected update keeps version",
        row_version(db, id).await?,
        1
    )?;
    let mut e = update(db, id, post(u, "vi", "new", PostStatus::Draft)).await?;
    e.title = "Second".into();
    save(db, e.clone()).await?;
    eq!("update adds 1", row_version(db, id).await?, 2)?;
    e.expected_version = Some(2);
    save(db, e).await?;
    eq!("every update adds 1", row_version(db, id).await?, 3)
}

/// Two editors start from the same version: the first save wins, the second is a
/// `Conflict` naming the current version and writes nothing at all.
pub async fn post_version_conflict<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let t1 = ok("ensure_tag", db.ensure_tag("one", "One").await)?;
    let t2 = ok("ensure_tag", db.ensure_tag("two", "Two").await)?;
    let base = post(u, "vi", "live", PostStatus::Published);
    let id = save(db, base.clone()).await?;
    let start = update(db, id, base).await?;
    let mut first = start.clone();
    first.title = "First".into();
    first.slug = "first".into();
    first.tag_ids = vec![t1];
    first.search_text = "alpha".into();
    first.revision = Some(NewRevision {
        author_id: Some(u),
        created_at: T0,
    });
    let mut second = start;
    second.title = "Second".into();
    second.slug = "second".into();
    second.tag_ids = vec![t2];
    second.search_text = "bravo".into();
    second.revision = Some(NewRevision {
        author_id: Some(u),
        created_at: T0,
    });
    save(db, first).await?;
    let after_first = ok("get_post", db.get_post(id).await)?;
    let v = version(db).await?;
    match db.save_post(second).await {
        Err(e) if e.code == ErrorCode::Conflict => that(
            &format!("conflict detail names version 2: {:?}", e.detail),
            e.detail.contains("version 2"),
        )?,
        other => return Err(format!("stale save: expected Conflict, got {other:?}")),
    }
    eq!(
        "stored row is the first save",
        ok("get_post", db.get_post(id).await)?,
        after_first,
    )?;
    eq!(
        "no revision from the stale save",
        ok("list_revisions", db.list_revisions(id).await)?.len(),
        1,
    )?;
    eq!(
        "no search hit from the stale save",
        ok("search", db.search("vi", "bravo", 10).await)?.len(),
        0,
    )?;
    eq!(
        "no redirect from the stale save",
        ok("find_redirect", db.find_redirect("/vi/first").await)?,
        None,
    )?;
    eq!(
        "stale save does not take the slug",
        ok("find_post", db.find_post("vi", "second").await)?,
        None,
    )?;
    version_is(db, "stale save", v).await
}

/// Trash, restore and `publish_due` change the row, so they move the version too: an
/// editor opened before them cannot overwrite them.
pub async fn post_version_status_changes<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let d = post(u, "vi", "d", PostStatus::Draft);
    let id = save(db, d.clone()).await?;
    let mut stale = update(db, id, d.clone()).await?;
    // An edit of its own: a stale save equal to the stored row is a no-op, not a conflict.
    stale.title = "Edited before the trash".into();
    ok("trash_post", db.trash_post(id, T0).await)?;
    eq!("trash adds 1", row_version(db, id).await?, 2)?;
    // Trashed first, whatever the version: the editor must restore, not retry.
    fails(
        "stale save of a trashed post",
        db.save_post(stale.clone()).await,
        ErrorCode::InvalidInput,
    )?;
    let fresh = update(db, id, d.clone()).await?;
    fails(
        "current-version save of a trashed post",
        db.save_post(fresh).await,
        ErrorCode::InvalidInput,
    )?;
    ok("restore_post", db.restore_post(id, T0).await)?;
    eq!("restore adds 1", row_version(db, id).await?, 3)?;
    fails(
        "save from before the trash",
        db.save_post(stale).await,
        ErrorCode::Conflict,
    )?;
    eq!(
        "still restored as draft",
        ok("get_post", db.get_post(id).await)?.status,
        PostStatus::Draft,
    )?;
    let mut s = post(u, "vi", "s", PostStatus::Scheduled);
    s.published_at = Some(100);
    let sid = save(db, s.clone()).await?;
    let before_publish = update(db, sid, s).await?;
    ok("publish_due", db.publish_due(100, 100).await)?;
    eq!("publish_due adds 1", row_version(db, sid).await?, 2)?;
    fails(
        "save from before publish_due",
        db.save_post(before_publish).await,
        ErrorCode::Conflict,
    )?;
    eq!(
        "stays published",
        ok("get_post", db.get_post(sid).await)?.status,
        PostStatus::Published,
    )
}

fn keyed(author: UserId, slug: &str, key: &str) -> SavePost {
    SavePost {
        client_id: Some(key.to_owned()),
        ..post(author, "vi", slug, PostStatus::Draft)
    }
}

async fn post_count<S: Store>(db: &S) -> super::kit::Got<usize> {
    let all = PostQuery::new(PostOrder::UpdatedDesc, 100);
    Ok(ok("list_posts", db.list_posts(&all).await)?.len())
}

/// A create retried with its client id (its answer was lost) returns the post the first one
/// made and writes nothing; the id is per author.
pub async fn post_client_id_replays<S: Store>(db: &S) -> Check {
    let a = user(db, "alice").await?;
    let b = user(db, "bob").await?;
    let first = save(db, keyed(a, "hello", "k-1")).await?;
    let made = ok("get_post", db.get_post(first).await)?;
    eq!(
        "find_client_post",
        ok("find_client_post", db.find_client_post(a, "k-1").await)?,
        Some(first),
    )?;
    let v = version(db).await?;
    // The same body (same slug): a replay, not a taken slug.
    eq!(
        "same body again",
        save(db, keyed(a, "hello", "k-1")).await?,
        first
    )?;
    // A different body, typed while the first answer was lost: still that post, unchanged.
    let mut later = keyed(a, "hello-2", "k-1");
    later.title = "Later".into();
    later.status = PostStatus::Published;
    later.published_at = Some(T0);
    eq!("other body, same key", save(db, later).await?, first)?;
    eq!("one post", post_count(db).await?, 1)?;
    eq!(
        "replay leaves the post",
        ok("get_post", db.get_post(first).await)?,
        made
    )?;
    eq!(
        "replay takes no slug",
        ok("find_post", db.find_post("vi", "hello-2").await)?,
        None,
    )?;
    version_is(db, "replays", v).await?;
    // Another author's same key is another request.
    let theirs = save(db, keyed(b, "theirs", "k-1")).await?;
    that("another author gets a new post", theirs != first)?;
    eq!(
        "find_client_post is per author",
        ok("find_client_post", db.find_client_post(b, "k-1").await)?,
        Some(theirs),
    )?;
    eq!(
        "unknown key",
        ok("find_client_post", db.find_client_post(a, "k-2").await)?,
        None,
    )?;
    // Without a key every create is new.
    save(db, post(a, "vi", "plain", PostStatus::Draft)).await?;
    save(db, post(a, "vi", "plain-2", PostStatus::Draft)).await?;
    eq!("keyless creates", post_count(db).await?, 4)?;
    // Deleted for good: the key is free again.
    ok("trash_post", db.trash_post(first, T0).await)?;
    ok("delete_post", db.delete_post(first).await)?;
    let again = save(db, keyed(a, "hello", "k-1")).await?;
    that("a deleted post's key makes a new post", again != first)
}

/// Two creates with one client id at the same moment make one post; both get its id.
pub async fn post_client_id_concurrent<S: Store>(db: &S) -> Check {
    let a = user(db, "alice").await?;
    // Same body (both race for the slug too) and different bodies (only the key collides).
    for (x, y, key) in [("same", "same", "race-1"), ("left", "right", "race-2")] {
        let (r1, r2) = both(
            db.save_post(keyed(a, x, key)),
            db.save_post(keyed(a, y, key)),
        )
        .await;
        let (r1, r2) = (ok("first racer", r1)?, ok("second racer", r2)?);
        eq!(&format!("{key}: one id for both"), r1, r2)?;
        eq!(
            &format!("{key}: find_client_post"),
            ok("find_client_post", db.find_client_post(a, key).await)?,
            Some(r1),
        )?;
    }
    eq!("one post per key", post_count(db).await?, 2)
}

/// A save from a stale version whose content the row already holds (the same save, its answer
/// lost) succeeds and writes nothing; any difference is still a `Conflict`.
pub async fn post_stale_equal_save<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let t1 = ok("ensure_tag", db.ensure_tag("one", "One").await)?;
    let t2 = ok("ensure_tag", db.ensure_tag("two", "Two").await)?;
    let id = save(db, post(u, "vi", "p", PostStatus::Published)).await?;
    let mut edit = update(db, id, post(u, "vi", "p", PostStatus::Published)).await?;
    edit.title = "Edited".into();
    edit.markdown = "# edited".into();
    edit.tag_ids = vec![t2, t1];
    edit.revision = Some(NewRevision {
        author_id: Some(u),
        created_at: T0,
    });
    save(db, edit.clone()).await?;
    let stored = ok("get_post", db.get_post(id).await)?;
    eq!("edited to version 2", stored.version, 2)?;
    let v = version(db).await?;
    // The retry: version 1 again, same content (tags in another order, repeated).
    let mut retry = edit.clone();
    retry.tag_ids = vec![t1, t2, t1];
    retry.now = T0 + 5;
    eq!("stale equal save", save(db, retry).await?, id)?;
    eq!(
        "nothing written",
        ok("get_post", db.get_post(id).await)?,
        stored
    )?;
    eq!(
        "no second revision",
        ok("list_revisions", db.list_revisions(id).await)?.len(),
        1,
    )?;
    version_is(db, "stale equal save", v).await?;
    // Another writer's different change from the same stale version.
    let mut other = edit.clone();
    other.markdown = "# someone else".into();
    fails(
        "stale, other markdown",
        db.save_post(other).await,
        ErrorCode::Conflict,
    )?;
    let mut other = edit.clone();
    other.title = "Someone else".into();
    fails(
        "stale, other title",
        db.save_post(other).await,
        ErrorCode::Conflict,
    )?;
    let mut other = edit.clone();
    other.pinned = true;
    fails(
        "stale, other flag",
        db.save_post(other).await,
        ErrorCode::Conflict,
    )?;
    let mut other = edit.clone();
    other.tag_ids = vec![t1];
    fails(
        "stale, other tags",
        db.save_post(other).await,
        ErrorCode::Conflict,
    )?;
    let mut other = edit;
    other.status = PostStatus::Draft;
    other.published_at = None;
    fails(
        "stale, other status",
        db.save_post(other).await,
        ErrorCode::Conflict,
    )?;
    eq!(
        "conflicts wrote nothing",
        ok("get_post", db.get_post(id).await)?,
        stored
    )?;
    version_is(db, "conflicts", v).await
}
