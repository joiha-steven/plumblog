//! Comments, settings, extension options, plugins, webhooks and the system log.

use plumblog_model::{CommentId, ErrorCode, PostId};
use plumblog_syslog::EventKind;

use super::kit::{
    Check, Got, T0, desc, eq, event, fails, ok, post, save, user, version, version_is,
};
use super::posts::comment;
use crate::{
    CommentStatus, LogQuery, NewComment, NewWebhook, OptionOwner, PluginState, PostStatus, Store,
    Webhook,
};

fn at(mut c: NewComment, created_at: i64) -> NewComment {
    c.created_at = created_at;
    c
}

pub async fn comments_round_trip<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let p = save(db, post(u, "vi", "p", PostStatus::Published)).await?;
    let q = save(db, post(u, "vi", "q", PostStatus::Published)).await?;
    let c1 = ok(
        "add_comment",
        db.add_comment(at(comment(p, None, CommentStatus::Pending), 200))
            .await,
    )?;
    let c2 = ok(
        "add_comment",
        db.add_comment(at(comment(p, None, CommentStatus::Pending), 100))
            .await,
    )?;
    let c3 = ok(
        "add_comment",
        db.add_comment(at(comment(p, Some(c1), CommentStatus::Pending), 100))
            .await,
    )?;
    ok(
        "add_comment",
        db.add_comment(at(comment(p, None, CommentStatus::Spam), 50))
            .await,
    )?;
    let other = ok(
        "add_comment",
        db.add_comment(comment(q, None, CommentStatus::Pending))
            .await,
    )?;

    let ids = |v: Vec<crate::Comment>| v.iter().map(|c| c.id).collect::<Vec<_>>();
    let mut want = super::kit::asc(&[c2, c3]);
    want.push(c1);
    eq!(
        "list_comments oldest first (created_at, id), one post, one status",
        ids(ok(
            "list_comments",
            db.list_comments(p, CommentStatus::Pending).await,
        )?),
        want,
    )?;
    let reply = ok(
        "list_comments",
        db.list_comments(p, CommentStatus::Pending).await,
    )?
    .into_iter()
    .find(|c| c.id == c3)
    .ok_or("reply missing")?;
    eq!(
        "reply fields",
        (
            reply.post_id,
            reply.parent_id,
            reply.author_name.as_str(),
            reply.body.as_str(),
        ),
        (p, Some(c1), "Reader", "Nice"),
    )?;

    comment_errors(db, p, q, other).await?;
    ok(
        "set_comment_status",
        db.set_comment_status(c2, CommentStatus::Spam).await,
    )?;
    eq!(
        "status changed",
        ids(ok(
            "list_comments",
            db.list_comments(p, CommentStatus::Spam).await,
        )?)
        .contains(&c2),
        true,
    )?;

    // Moderation queue across posts, paged by (created_at, id).
    let mut all = Vec::new();
    let mut after = None;
    for _ in 0..10 {
        let page = ok(
            "list_comments_by_status",
            db.list_comments_by_status(CommentStatus::Pending, after, 1)
                .await,
        )?;
        let Some(last) = page.last() else { break };
        after = Some((last.created_at, last.id));
        all.push((last.created_at, last.id));
    }
    let mut want = vec![(100, c3), (200, c1), (T0, other)];
    want.sort_unstable();
    eq!("queue pages", all, want)?;
    eq!(
        "queue limit 0",
        ok(
            "list_comments_by_status",
            db.list_comments_by_status(CommentStatus::Pending, None, 0)
                .await,
        )?
        .len(),
        0,
    )
}

/// Missing post or parent, a parent on another post, and unknown ids.
async fn comment_errors<S: Store>(db: &S, p: PostId, q: PostId, other: CommentId) -> Check {
    let missing_post = PostId(p.0.max(q.0) + 1000);
    fails(
        "add_comment missing post",
        db.add_comment(comment(missing_post, None, CommentStatus::Pending))
            .await,
        ErrorCode::InvalidInput,
    )?;
    let missing = CommentId(other.0 + 1000);
    fails(
        "add_comment missing parent",
        db.add_comment(comment(p, Some(missing), CommentStatus::Pending))
            .await,
        ErrorCode::InvalidInput,
    )?;
    fails(
        "add_comment parent on another post",
        db.add_comment(comment(p, Some(other), CommentStatus::Pending))
            .await,
        ErrorCode::InvalidInput,
    )?;
    fails(
        "set_comment_status missing",
        db.set_comment_status(missing, CommentStatus::Approved)
            .await,
        ErrorCode::NotFound,
    )?;
    fails(
        "delete_comment missing",
        db.delete_comment(missing).await,
        ErrorCode::NotFound,
    )
}

/// Replies at any depth go with their parent; other comments stay.
pub async fn comment_delete_cascades<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let p = save(db, post(u, "vi", "p", PostStatus::Draft)).await?;
    let root = ok(
        "add_comment",
        db.add_comment(comment(p, None, CommentStatus::Pending))
            .await,
    )?;
    let r1 = ok(
        "add_comment",
        db.add_comment(comment(p, Some(root), CommentStatus::Pending))
            .await,
    )?;
    ok(
        "add_comment",
        db.add_comment(comment(p, Some(r1), CommentStatus::Spam))
            .await,
    )?;
    let keep = ok(
        "add_comment",
        db.add_comment(comment(p, None, CommentStatus::Pending))
            .await,
    )?;
    ok("delete_comment", db.delete_comment(root).await)?;
    let mut left = Vec::new();
    for st in CommentStatus::ALL {
        left.extend(
            ok("list_comments", db.list_comments(p, *st).await)?
                .iter()
                .map(|c| c.id),
        );
    }
    eq!("only the unrelated comment is left", left, vec![keep])?;
    fails(
        "deleted reply",
        db.delete_comment(r1).await,
        ErrorCode::NotFound,
    )
}

/// Bumps when the old or new status is approved, and only then.
pub async fn comment_versions<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let p = save(db, post(u, "vi", "p", PostStatus::Published)).await?;
    let mut v = version(db).await?;
    let pending = ok(
        "add_comment",
        db.add_comment(comment(p, None, CommentStatus::Pending))
            .await,
    )?;
    version_is(db, "add pending comment", v).await?;
    let approved = ok(
        "add_comment",
        db.add_comment(comment(p, None, CommentStatus::Approved))
            .await,
    )?;
    v += 1;
    version_is(db, "add approved comment", v).await?;
    ok(
        "set_comment_status",
        db.set_comment_status(pending, CommentStatus::Spam).await,
    )?;
    version_is(db, "pending -> spam", v).await?;
    ok(
        "set_comment_status",
        db.set_comment_status(pending, CommentStatus::Approved)
            .await,
    )?;
    v += 1;
    version_is(db, "spam -> approved", v).await?;
    ok(
        "set_comment_status",
        db.set_comment_status(pending, CommentStatus::Trashed).await,
    )?;
    v += 1;
    version_is(db, "approved -> trashed", v).await?;
    ok("delete_comment", db.delete_comment(pending).await)?;
    version_is(db, "delete a trashed comment", v).await?;
    let reply = ok(
        "add_comment",
        db.add_comment(comment(p, Some(approved), CommentStatus::Pending))
            .await,
    )?;
    version_is(db, "add pending reply", v).await?;
    ok("delete_comment", db.delete_comment(reply).await)?;
    version_is(db, "delete a pending reply", v).await?;
    let pend_root = ok(
        "add_comment",
        db.add_comment(comment(p, None, CommentStatus::Pending))
            .await,
    )?;
    ok(
        "add_comment",
        db.add_comment(comment(p, Some(pend_root), CommentStatus::Approved))
            .await,
    )?;
    v += 1;
    ok("delete_comment", db.delete_comment(pend_root).await)?;
    v += 1;
    version_is(db, "delete a pending comment with an approved reply", v).await?;
    ok("delete_comment", db.delete_comment(approved).await)?;
    version_is(db, "delete an approved comment", v + 1).await
}

pub async fn settings_round_trip<S: Store>(db: &S) -> Check {
    let mut v = version(db).await?;
    ok("put_setting", db.put_setting("title", "\"Blog\"", T0).await)?;
    ok("put_setting", db.put_setting("lang", "\"vi\"", T0).await)?;
    v += 2;
    version_is(db, "put_setting twice", v).await?;
    ok(
        "put_setting upsert",
        db.put_setting("title", "\"New\"", T0 + 1).await,
    )?;
    v += 1;
    version_is(db, "put_setting upsert", v).await?;
    eq!(
        "all_settings by key",
        ok("all_settings", db.all_settings().await)?,
        vec![
            ("lang".to_owned(), "\"vi\"".to_owned()),
            ("title".to_owned(), "\"New\"".to_owned()),
        ],
    )?;
    eq!(
        "get_setting",
        ok("get_setting", db.get_setting("title").await)?,
        Some("\"New\"".into()),
    )?;
    eq!(
        "get_setting absent",
        ok("get_setting", db.get_setting("x").await)?,
        None,
    )?;
    ok("delete_setting", db.delete_setting("lang").await)?;
    v += 1;
    version_is(db, "delete_setting present", v).await?;
    ok("delete_setting absent", db.delete_setting("lang").await)?;
    version_is(db, "delete_setting absent", v).await?;
    eq!(
        "deleted",
        ok("get_setting", db.get_setting("lang").await)?,
        None,
    )
}

pub async fn options_round_trip<S: Store>(db: &S) -> Check {
    let mut v = version(db).await?;
    ok(
        "put_option",
        db.put_option(OptionOwner::Theme, "plum", "b", "2", T0)
            .await,
    )?;
    ok(
        "put_option",
        db.put_option(OptionOwner::Theme, "plum", "a", "1", T0)
            .await,
    )?;
    ok(
        "put_option",
        db.put_option(OptionOwner::Plugin, "plum", "a", "9", T0)
            .await,
    )?;
    ok(
        "put_option",
        db.put_option(OptionOwner::Theme, "other", "a", "5", T0)
            .await,
    )?;
    ok(
        "put_option upsert",
        db.put_option(OptionOwner::Theme, "plum", "b", "3", T0)
            .await,
    )?;
    v += 5;
    version_is(db, "put_option x5", v).await?;
    let kv = |k: &str, x: &str| (k.to_owned(), x.to_owned());
    eq!(
        "options by key, per owner kind and id",
        ok("options", db.options(OptionOwner::Theme, "plum").await)?,
        vec![kv("a", "1"), kv("b", "3")],
    )?;
    eq!(
        "plugin with the same id is separate",
        ok("options", db.options(OptionOwner::Plugin, "plum").await)?,
        vec![kv("a", "9")],
    )?;
    eq!(
        "delete_options count",
        ok(
            "delete_options",
            db.delete_options(OptionOwner::Theme, "plum").await,
        )?,
        2,
    )?;
    v += 1;
    version_is(db, "delete_options with rows", v).await?;
    eq!(
        "delete_options none",
        ok(
            "delete_options",
            db.delete_options(OptionOwner::Theme, "plum").await,
        )?,
        0,
    )?;
    version_is(db, "delete_options without rows", v).await?;
    eq!(
        "others kept",
        (
            ok("options", db.options(OptionOwner::Plugin, "plum").await)?.len(),
            ok("options", db.options(OptionOwner::Theme, "other").await)?.len(),
        ),
        (1, 1),
    )
}

pub async fn plugins_round_trip<S: Store>(db: &S) -> Check {
    let v = version(db).await?;
    let p = |id: &str, enabled, reason: Option<&str>, at| PluginState {
        id: id.into(),
        enabled,
        disabled_reason: reason.map(Into::into),
        updated_at: at,
    };
    ok("put_plugin", db.put_plugin(p("zeta", true, None, 1)).await)?;
    ok("put_plugin", db.put_plugin(p("alpha", true, None, 1)).await)?;
    ok(
        "put_plugin upsert",
        db.put_plugin(p("zeta", false, Some("crashed"), 2)).await,
    )?;
    version_is(db, "put_plugin x3", v + 3).await?;
    eq!(
        "list_plugins by id",
        ok("list_plugins", db.list_plugins().await)?,
        vec![
            p("alpha", true, None, 1),
            p("zeta", false, Some("crashed"), 2),
        ],
    )
}

pub async fn webhooks_round_trip<S: Store>(db: &S) -> Check {
    let nw = |url: &str| NewWebhook {
        url: url.into(),
        events: "content.changed".into(),
        secret: "s3cret".into(),
        enabled: true,
        created_at: T0,
    };
    let a = ok(
        "create_webhook",
        db.create_webhook(nw("https://a.example")).await,
    )?;
    let b = ok(
        "create_webhook",
        db.create_webhook(nw("https://b.example")).await,
    )?;
    let list = ok("list_webhooks", db.list_webhooks().await)?;
    eq!(
        "list_webhooks by id",
        list.iter().map(|w| w.id).collect::<Vec<_>>(),
        super::kit::asc(&[a, b]),
    )?;
    let changed = Webhook {
        id: a,
        url: "https://c.example".into(),
        events: "a,b".into(),
        secret: "new".into(),
        enabled: false,
        created_at: T0 + 50,
    };
    ok("update_webhook", db.update_webhook(&changed).await)?;
    let got = ok("list_webhooks", db.list_webhooks().await)?
        .into_iter()
        .find(|w| w.id == a)
        .ok_or("webhook missing")?;
    eq!(
        "update_webhook writes all but created_at",
        got,
        Webhook {
            created_at: T0,
            ..changed.clone()
        },
    )?;
    let missing = a.max(b) + 1000;
    fails(
        "update_webhook missing",
        db.update_webhook(&Webhook {
            id: missing,
            ..changed
        })
        .await,
        ErrorCode::NotFound,
    )?;
    ok("delete_webhook", db.delete_webhook(a).await)?;
    fails(
        "delete_webhook again",
        db.delete_webhook(a).await,
        ErrorCode::NotFound,
    )?;
    eq!(
        "one left",
        ok("list_webhooks", db.list_webhooks().await)?.len(),
        1,
    )
}

fn log_query() -> LogQuery {
    LogQuery {
        kind: None,
        actor: None,
        from_ms: None,
        to_ms: None,
        before: None,
        limit: 100,
    }
}

async fn log_ids<S: Store>(db: &S, q: LogQuery) -> Got<Vec<i64>> {
    Ok(ok("query_log", db.query_log(q).await)?
        .iter()
        .map(|e| e.id)
        .collect())
}

pub async fn log_query_and_cursor<S: Store>(db: &S) -> Check {
    let a = user(db, "alice").await?;
    let missing = plumblog_model::UserId(a.0 + 1000);
    fails(
        "append with missing actor",
        db.append(event(1, EventKind::SignIn, Some(missing))?).await,
        ErrorCode::InvalidInput,
    )?;
    // Appended out of time order: ids do not follow at_ms.
    let mut rows = Vec::new();
    for (t, kind, actor) in [
        (300, EventKind::SignIn, Some(a)),
        (100, EventKind::Deploy, None),
        (300, EventKind::Deploy, None),
        (200, EventKind::SignIn, Some(a)),
        (300, EventKind::SignIn, None),
    ] {
        let mut e = event(t, kind, actor)?;
        if kind == EventKind::Deploy {
            e.error = Some(ErrorCode::Storage);
        }
        let id = ok("append", db.append(e.clone()).await)?;
        rows.push(((t, id), e));
    }
    let entries = ok("query_log", db.query_log(log_query()).await)?;
    let first = rows.first().ok_or("no rows")?;
    let stored = entries
        .iter()
        .find(|x| x.id == first.0.1)
        .ok_or("first entry missing")?;
    eq!("event round trip", &stored.event, &first.1)?;
    let newest: Vec<(u64, i64)> = desc(&rows.iter().map(|r| r.0).collect::<Vec<_>>());
    let pick = |f: &dyn Fn(&(u64, i64)) -> bool| -> Vec<i64> {
        newest.iter().filter(|x| f(x)).map(|x| x.1).collect()
    };
    eq!(
        "newest first (at_ms, id)",
        log_ids(db, log_query()).await?,
        pick(&|_| true),
    )?;

    let kind_of = |id: i64| rows.iter().find(|r| r.0.1 == id).map(|r| r.1.kind);
    let actor_of = |id: i64| rows.iter().find(|r| r.0.1 == id).and_then(|r| r.1.actor);
    let mut q = log_query();
    q.kind = Some(EventKind::SignIn);
    eq!(
        "kind filter",
        log_ids(db, q).await?,
        pick(&|x| kind_of(x.1) == Some(EventKind::SignIn)),
    )?;
    let mut q = log_query();
    q.actor = Some(a);
    eq!(
        "actor filter",
        log_ids(db, q).await?,
        pick(&|x| actor_of(x.1) == Some(a)),
    )?;
    let mut q = log_query();
    q.from_ms = Some(200);
    q.to_ms = Some(300);
    eq!(
        "from inclusive, to exclusive",
        log_ids(db, q).await?,
        pick(&|x| x.0 == 200),
    )?;

    let mut paged = Vec::new();
    let mut q = log_query();
    q.limit = 2;
    for _ in 0..10 {
        let page = ok("query_log", db.query_log(q).await)?;
        let Some(last) = page.last() else { break };
        q.before = Some((last.event.at_ms, last.id));
        paged.extend(page.iter().map(|e| e.id));
    }
    eq!("cursor pages", paged, pick(&|_| true))?;
    let mut q = log_query();
    q.limit = 0;
    eq!("limit 0", log_ids(db, q).await?, Vec::new())
}

pub async fn log_prune<S: Store>(db: &S) -> Check {
    let mut ids = Vec::new();
    for t in [10, 20, 30, 30, 40, 50] {
        ids.push((
            t,
            ok(
                "append",
                db.append(event(t, EventKind::Deploy, None)?).await,
            )?,
        ));
    }
    eq!(
        "prune by age (at_ms < 30)",
        ok("prune_log", db.prune_log(30, 100).await)?,
        2,
    )?;
    eq!(
        "prune by count keeps the newest 2",
        ok("prune_log", db.prune_log(0, 2).await)?,
        2,
    )?;
    let newest = desc(&ids);
    eq!(
        "left",
        log_ids(db, log_query()).await?,
        newest[..2].iter().map(|x| x.1).collect::<Vec<_>>(),
    )?;
    eq!(
        "nothing to prune",
        ok("prune_log", db.prune_log(0, 2).await)?,
        0,
    )?;
    eq!(
        "keep 0 empties",
        ok("prune_log", db.prune_log(0, 0).await)?,
        2,
    )
}

pub async fn site_starts_at_version_1<S: Store>(db: &S) -> Check {
    version_is(db, "migration", 1).await
}

/// Author profiles, image alt text and redirects all show in reader responses, so writing
/// them bumps the content version; failed writes and no-op deletes do not.
pub async fn reader_metadata_versions<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let v = version(db).await?;
    let profile = ok("get_user", db.get_user(u).await)?;
    let edit = crate::ProfileUpdate {
        login: profile.login,
        slug: profile.slug,
        display_name: profile.display_name,
        bio: "New bio".into(),
    };
    ok(
        "update_profile",
        db.update_profile(u, edit.clone(), T0 + 1).await,
    )?;
    version_is(db, "update_profile", v + 1).await?;
    fails(
        "update_profile missing",
        db.update_profile(plumblog_model::UserId(u.0 + 1000), edit, T0 + 1)
            .await,
        ErrorCode::UserNotFound,
    )?;
    version_is(db, "failed update_profile", v + 1).await?;

    let m = ok(
        "create_media",
        db.create_media(super::kit::media("a.jpg", T0)).await,
    )?;
    version_is(db, "create_media", v + 1).await?;
    ok("set_media_alt", db.set_media_alt(m, "A cat").await)?;
    version_is(db, "set_media_alt", v + 2).await?;
    let missing = plumblog_model::MediaId(m.0 + 1000);
    fails(
        "set_media_alt missing",
        db.set_media_alt(missing, "x").await,
        ErrorCode::NotFound,
    )?;
    version_is(db, "failed set_media_alt", v + 2).await?;

    ok("put_redirect", db.put_redirect("/vi/a", "/vi/b", T0).await)?;
    version_is(db, "put_redirect", v + 3).await?;
    fails(
        "put_redirect to itself",
        db.put_redirect("/vi/c", "/vi/c", T0).await,
        ErrorCode::InvalidInput,
    )?;
    version_is(db, "failed put_redirect", v + 3).await?;
    ok("delete_redirect", db.delete_redirect("/vi/a").await)?;
    version_is(db, "delete_redirect", v + 4).await?;
    ok("delete_redirect absent", db.delete_redirect("/vi/a").await)?;
    version_is(db, "delete_redirect of an absent path", v + 4).await
}
