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
    eq!(
        "settings_updated_at without settings",
        ok("settings_updated_at", db.settings_updated_at().await)?,
        None
    )?;
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
        "settings_updated_at: the newest write",
        ok("settings_updated_at", db.settings_updated_at().await)?,
        Some(T0 + 1)
    )?;
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
            e = e.error(ErrorCode::Storage);
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

    let kind_of = |id: i64| rows.iter().find(|r| r.0.1 == id).map(|r| r.1.kind());
    let actor_of = |id: i64| {
        rows.iter()
            .find(|r| r.0.1 == id)
            .and_then(|r| r.1.actor_id())
    };
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
        q.before = Some((last.event.at_ms(), last.id));
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
    // Bounded: one row per call, oldest first; the next call continues.
    eq!(
        "limit 1 deletes one",
        ok("prune_log", db.prune_log(30, 100, 1).await)?,
        1,
    )?;
    let after_one = log_ids(db, log_query()).await?;
    eq!(
        "the oldest went first",
        after_one.contains(&ids[0].1),
        false
    )?;
    eq!("the next stays", after_one.contains(&ids[1].1), true)?;
    eq!(
        "prune by age (at_ms < 30) continues",
        ok("prune_log", db.prune_log(30, 100, 100).await)?,
        1,
    )?;
    eq!(
        "prune by count keeps the newest 2",
        ok("prune_log", db.prune_log(0, 2, 100).await)?,
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
        ok("prune_log", db.prune_log(0, 2, 100).await)?,
        0,
    )?;
    eq!(
        "keep 0 empties",
        ok("prune_log", db.prune_log(0, 0, 100).await)?,
        2,
    )
}

/// Subject and fields survive storage byte for byte (newlines, tabs, backslashes), and what
/// redaction removed before the write is still gone after the read.
pub async fn log_detail_redacted<S: Store>(db: &S) -> Check {
    let r = plumblog_syslog::REDACTED;
    let e = event(1, EventKind::SettingsChanged, None)?
        .subject("line1\nline2\ttab \\n")
        .field("from", "a\\b")
        .field("setupCode", "K7QX-29PL")
        .field("x-forwarded-for", "203.0.113.9, 198.51.100.7")
        .field(
            "note",
            "reach me at reader@example.com from 203.0.113.9:443",
        )
        .field("body", r#"{"password":"hunter2","lang":"vi"}"#)
        .field("url", "https://example.com/cb?token=abc123&page=2")
        .field("to", "");
    let id = ok("append", db.append(e.clone()).await)?;
    let got = ok("query_log", db.query_log(log_query()).await)?;
    let stored = got.iter().find(|x| x.id == id).ok_or("entry missing")?;
    eq!("detail round trip", &stored.event, &e)?;
    let values: Vec<String> = stored.event.fields().iter().map(|f| f.1.clone()).collect();
    eq!(
        "secrets stay redacted",
        values,
        vec![
            "a\\b".to_owned(),
            r.to_owned(),
            r.to_owned(),
            format!("reach me at {r} from {r}:443"),
            format!(r#"{{"password":"{r}","lang":"vi"}}"#),
            format!("https://example.com/cb?token={r}&page=2"),
            String::new(),
        ],
    )
}

/// The daily job applies both caps and leaves one `job.run` entry saying what it did.
pub async fn log_retention_job<S: Store>(db: &S) -> Check {
    const DAY: u64 = 24 * 60 * 60 * 1000;
    let now = 100 * DAY;
    // Two rows older than 30 days, four within; the cap of 3 then drops the oldest recent one.
    let mut recent = Vec::new();
    for t in [10 * DAY, 60 * DAY, 80 * DAY, 90 * DAY, 95 * DAY, 99 * DAY] {
        let id = ok(
            "append",
            db.append(event(t, EventKind::Deploy, None)?).await,
        )?;
        if t >= 90 * DAY {
            recent.push((t, id));
        }
    }
    let retention = plumblog_syslog::Retention {
        max_age_days: 30,
        max_rows: 3,
        per_run: 100,
    };
    let scope = plumblog_syslog::Scope::new(
        plumblog_model::RequestId::parse("job-1").ok_or("request id rejected")?,
        None,
    );
    eq!(
        "deleted by age and by count",
        ok(
            "retention_job",
            crate::retention_job(db, &scope, retention, now, |_| ()).await,
        )?,
        3,
    )?;
    let log = ok("query_log", db.query_log(log_query()).await)?;
    let job = log.first().ok_or("job event missing")?;
    eq!("job event kind", job.event.kind(), EventKind::ScheduledJob)?;
    eq!(
        "job event subject",
        job.event.subject_text(),
        crate::RETENTION_JOB
    )?;
    eq!(
        "job event request id",
        job.event.request_id().as_str(),
        "job-1"
    )?;
    eq!(
        "job event fields",
        job.event.fields(),
        &[("deleted".to_owned(), "3".to_owned())][..],
    )?;
    eq!(
        "rows kept: the newest 3, then the job event",
        log.iter().skip(1).map(|e| e.id).collect::<Vec<_>>(),
        desc(&recent).iter().map(|x| x.1).collect::<Vec<_>>(),
    )
}

/// A failure to record the job's own entry never undoes or stops the prune: the rows are
/// gone, the job succeeds, and the failure is reported once with the job's request id.
pub async fn log_retention_job_record_failure<S: Store>(db: &S) -> Check {
    ok(
        "append",
        db.append(event(1, EventKind::Deploy, None)?).await,
    )?;
    let ghost = plumblog_model::UserId(999_999);
    let scope = plumblog_syslog::Scope::new(
        plumblog_model::RequestId::parse("job-2").ok_or("request id rejected")?,
        Some(ghost),
    );
    let retention = plumblog_syslog::Retention {
        max_age_days: 1,
        ..plumblog_syslog::Retention::default()
    };
    let mut reports = Vec::new();
    eq!(
        "the job still succeeds",
        ok(
            "retention_job",
            crate::retention_job(db, &scope, retention, 10 * 24 * 60 * 60 * 1000, |m| {
                reports.push(m.to_owned());
            })
            .await,
        )?,
        1,
    )?;
    eq!("one report", reports.len(), 1)?;
    eq!(
        "report names the request",
        reports[0].starts_with("[req=job-2] syslog.record_failed kind=job.run"),
        true,
    )?;
    eq!("pruned anyway", log_ids(db, log_query()).await?, Vec::new())
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

/// `get_comment` and the shared moderation function on each store: permission first, one
/// `comment.moderated` event with ids only, the content version bumped when readers see it.
pub async fn comment_moderation<S: Store>(db: &S) -> Check {
    use crate::comment::moderate;
    use crate::{Actor, Moderation};
    use plumblog_model::{RequestId, Role};
    use plumblog_syslog::Scope;

    let u = user(db, "alice").await?;
    let pid = save(db, post(u, "vi", "p", PostStatus::Published)).await?;
    let cid = ok(
        "add_comment",
        db.add_comment(comment(pid, None, CommentStatus::Pending))
            .await,
    )?;
    let got = ok("get_comment", db.get_comment(cid).await)?;
    eq!(
        "get_comment",
        (got.id, got.post_id, got.status, got.author_name.as_str()),
        (cid, pid, CommentStatus::Pending, "Reader"),
    )?;
    fails(
        "get_comment unknown",
        db.get_comment(CommentId(cid.0 + 99)).await,
        ErrorCode::NotFound,
    )?;
    let scope = Scope::new(RequestId::parse("req-mod").ok_or("request id")?, Some(u));
    let writer = Actor {
        id: u,
        role: Role::Writer,
    };
    let ver = version(db).await?;
    fails(
        "moderate without an actor",
        moderate(db, &scope, None, cid, Moderation::Approve, 5, |_| {}).await,
        ErrorCode::Unauthenticated,
    )?;
    version_is(db, "refused moderation changes nothing", ver).await?;
    ok(
        "moderate approve (writer)",
        moderate(
            db,
            &scope,
            Some(writer),
            cid,
            Moderation::Approve,
            6,
            |_| {},
        )
        .await,
    )?;
    version_is(db, "approve bumps", ver + 1).await?;
    eq!(
        "approved",
        ok("get_comment", db.get_comment(cid).await)?.status,
        CommentStatus::Approved,
    )?;
    fails(
        "moderate unknown",
        moderate(
            db,
            &scope,
            Some(writer),
            CommentId(cid.0 + 99),
            Moderation::Spam,
            7,
            |_| {},
        )
        .await,
        ErrorCode::NotFound,
    )?;
    ok(
        "moderate delete",
        moderate(db, &scope, Some(writer), cid, Moderation::Delete, 8, |_| {}).await,
    )?;
    fails("deleted", db.get_comment(cid).await, ErrorCode::NotFound)?;
    moderation_log(db, u, cid, pid).await
}

/// The `comment.moderated` entries `comment_moderation` leaves: ids and status only.
async fn moderation_log<S: Store>(
    db: &S,
    u: plumblog_model::UserId,
    cid: CommentId,
    pid: PostId,
) -> Check {
    let log = ok(
        "query_log",
        db.query_log(LogQuery {
            kind: Some(EventKind::CommentModerated),
            actor: None,
            from_ms: None,
            to_ms: None,
            before: None,
            limit: 10,
        })
        .await,
    )?;
    let seen: Vec<_> = log
        .iter()
        .map(|e| (e.event.actor_id(), e.event.fields().to_vec()))
        .collect();
    let fields = |status: &str| {
        vec![
            ("comment".to_owned(), cid.0.to_string()),
            ("post".to_owned(), pid.0.to_string()),
            ("status".to_owned(), status.to_owned()),
        ]
    };
    eq!(
        "one event per action, newest first, ids only",
        seen,
        vec![(Some(u), fields("deleted")), (Some(u), fields("approved"))],
    )
}

/// `submit` stores the comment pending with its HTML rendered once; `latest_comments` keeps
/// the newest `limit`, oldest first, and says whether older ones were left out.
pub async fn comment_submit_and_latest<S: Store>(db: &S) -> Check {
    use crate::comment::{check, submit};
    use plumblog_markdown::render_comment;
    use plumblog_model::RequestId;
    use plumblog_syslog::Scope;

    let u = user(db, "alice").await?;
    let pid = save(db, post(u, "vi", "p", PostStatus::Published)).await?;
    let scope = Scope::new(RequestId::unknown(), None);
    let body = "Hi *there* [x](https://a.example/) <b>b</b>";
    let checked = check("Ann", body).map_err(|e| format!("check: {e}"))?;
    let cid = ok("submit", submit(db, &scope, pid, checked, 7, |_| {}).await)?;
    let got = ok("get_comment", db.get_comment(cid).await)?;
    eq!(
        "submitted comment",
        (got.status, got.body.as_str(), got.body_html.as_deref()),
        (
            CommentStatus::Pending,
            body,
            Some(render_comment(body).html.as_str())
        ),
    )?;

    let mut ids = Vec::new();
    for t in 0..5 {
        ids.push(ok(
            "add_comment",
            db.add_comment(at(comment(pid, None, CommentStatus::Approved), 100 + t))
                .await,
        )?);
    }
    let latest = |limit| db.latest_comments(pid, CommentStatus::Approved, limit);
    let (rows, more) = ok("latest_comments 3", latest(3).await)?;
    eq!(
        "newest 3, oldest first, more left",
        (rows.iter().map(|c| c.id).collect::<Vec<_>>(), more),
        (ids[2..].to_vec(), true),
    )?;
    let (rows, more) = ok("latest_comments 5", latest(5).await)?;
    eq!("all 5, none left", (rows.len(), more), (5, false))?;
    let (rows, more) = ok(
        "latest_comments pending",
        db.latest_comments(pid, CommentStatus::Pending, 10).await,
    )?;
    eq!(
        "one status only",
        (rows.iter().map(|c| c.id).collect::<Vec<_>>(), more),
        (vec![cid], false),
    )
}
