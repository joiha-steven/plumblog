//! `list_posts`: filters, the three orders and keyset paging; page size limits.

use plumblog_model::{PostId, UserId};
use plumblog_syslog::EventKind;

use super::kit::{Check, Got, T0, eq, event, fails, media, ok, post, save, that, user};
use crate::{
    LogQuery, MAX_PAGE, MediaQuery, PostCursor, PostKind, PostOrder, PostQuery, PostStatus, Store,
};

/// Walks every page of `q` with page size `size`; returns the ids in order.
async fn walk<S: Store>(db: &S, mut q: PostQuery, size: u32) -> Got<Vec<PostId>> {
    q.limit = size;
    q.after = None;
    let mut out = Vec::new();
    // Bounded so a store that ignores the cursor fails instead of looping.
    for _ in 0..50 {
        let rows = ok("list_posts", db.list_posts(&q).await)?;
        if rows.len() > crate::page_len(size) {
            return Err(format!(
                "list_posts: page of {} rows for limit {size}",
                rows.len()
            ));
        }
        let Some(last) = rows.last() else {
            return Ok(out);
        };
        q.after = Some(q.order.cursor(last));
        out.extend(rows.iter().map(|p| p.id));
    }
    Err("list_posts: cursor never reached the end".into())
}

/// Saves posts with `(published_at, updated_at, series_position)`; drafts get no
/// `published_at`.
async fn seed<S: Store>(
    db: &S,
    u: UserId,
    rows: &[(Option<i64>, i64, Option<i64>)],
) -> Got<Vec<PostId>> {
    let series = ok(
        "create_series",
        db.create_series(crate::NewSeries {
            slug: "s".into(),
            title: "S".into(),
            description: String::new(),
        })
        .await,
    )?;
    let mut ids = Vec::new();
    for (i, (published_at, now, position)) in rows.iter().enumerate() {
        let status = if published_at.is_some() {
            PostStatus::Published
        } else {
            PostStatus::Draft
        };
        let mut p = post(u, "vi", &format!("p{i}"), status);
        p.published_at = *published_at;
        p.now = *now;
        p.series_id = Some(series);
        p.series_position = *position;
        ids.push(save(db, p).await?);
    }
    Ok(ids)
}

/// Sorts `(key, id)` pairs and returns the ids.
fn order_by<K: Ord + Copy>(mut v: Vec<(K, PostId)>) -> Vec<PostId> {
    v.sort_unstable();
    v.into_iter().map(|(_, id)| id).collect()
}

pub async fn post_paging_published_desc<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let rows = [
        (Some(100), T0, None),
        (Some(300), T0, None),
        (Some(300), T0, None),
        (None, T0, None),
        (Some(300), T0, None),
        (Some(200), T0, None),
    ];
    let ids = seed(db, u, &rows).await?;
    let want = order_by(
        rows.iter()
            .zip(&ids)
            .filter_map(|(r, id)| r.0.map(|at| (std::cmp::Reverse((at, *id)), *id)))
            .collect(),
    );
    for size in [1, 2, 4, 100] {
        let got = walk(db, PostQuery::new(PostOrder::PublishedDesc, size), size).await?;
        eq!(
            &format!("PublishedDesc pages of {size}, NULL published_at excluded"),
            got,
            want.clone(),
        )?;
    }
    let mut q = PostQuery::new(PostOrder::PublishedDesc, 10);
    q.after = Some(PostCursor {
        key: None,
        id: ids[0],
    });
    fails(
        "PublishedDesc cursor without key",
        db.list_posts(&q).await,
        plumblog_model::ErrorCode::InvalidInput,
    )
}

pub async fn post_paging_updated_desc<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let rows = [
        (None, 500, None),
        (Some(1), 700, None),
        (None, 700, None),
        (None, 600, None),
        (None, 700, None),
    ];
    let ids = seed(db, u, &rows).await?;
    let want = order_by(
        rows.iter()
            .zip(&ids)
            .map(|(r, id)| (std::cmp::Reverse((r.1, *id)), *id))
            .collect(),
    );
    for size in [1, 2, 3] {
        let got = walk(db, PostQuery::new(PostOrder::UpdatedDesc, size), size).await?;
        eq!(&format!("UpdatedDesc pages of {size}"), got, want.clone())?;
    }
    Ok(())
}

/// `series_position` ASC with NULLs last, then id; the cursor crosses into the NULLs.
pub async fn post_paging_series_asc<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let rows = [
        (None, T0, Some(2)),
        (None, T0, None),
        (None, T0, Some(1)),
        (None, T0, Some(2)),
        (None, T0, None),
        (None, T0, Some(3)),
    ];
    let ids = seed(db, u, &rows).await?;
    let want = order_by(
        rows.iter()
            .zip(&ids)
            .map(|(r, id)| ((r.2.is_none(), r.2, *id), *id))
            .collect(),
    );
    for size in [1, 2, 4] {
        let got = walk(db, PostQuery::new(PostOrder::SeriesAsc, size), size).await?;
        eq!(&format!("SeriesAsc pages of {size}"), got, want.clone())?;
    }
    Ok(())
}

pub async fn post_filters<S: Store>(db: &S) -> Check {
    let alice = user(db, "alice").await?;
    let bob = user(db, "bob").await?;
    let tag = ok("ensure_tag", db.ensure_tag("t", "T").await)?;
    let series = ok(
        "create_series",
        db.create_series(crate::NewSeries {
            slug: "s".into(),
            title: "S".into(),
            description: String::new(),
        })
        .await,
    )?;
    let base = save(db, post(alice, "vi", "base", PostStatus::Published)).await?;
    let mut p = post(alice, "vi", "page", PostStatus::Published);
    p.kind = PostKind::Page;
    let page = save(db, p).await?;
    let draft = save(db, post(alice, "vi", "draft", PostStatus::Draft)).await?;
    let en = save(db, post(alice, "en", "english", PostStatus::Published)).await?;
    let by_b = save(db, post(bob, "vi", "by-b", PostStatus::Published)).await?;
    let mut p = post(alice, "vi", "tagged", PostStatus::Published);
    p.tag_ids = vec![tag];
    let tagged = save(db, p).await?;
    let mut p = post(alice, "vi", "in-series", PostStatus::Published);
    p.series_id = Some(series);
    let in_series = save(db, p).await?;
    let mut p = post(alice, "vi", "pinned", PostStatus::Published);
    p.pinned = true;
    let pinned = save(db, p).await?;
    let all = [base, page, draft, en, by_b, tagged, in_series, pinned];

    let run = |q: PostQuery| async move {
        let mut ids: Vec<PostId> = ok("list_posts", db.list_posts(&q).await)?
            .iter()
            .map(|p| p.id)
            .collect();
        ids.sort_unstable();
        Ok::<_, String>(ids)
    };
    let q = || PostQuery::new(PostOrder::UpdatedDesc, 100);
    let without = |x: &[PostId]| {
        let mut v: Vec<PostId> = all.iter().copied().filter(|id| !x.contains(id)).collect();
        v.sort_unstable();
        v
    };
    eq!("no filter", run(q()).await?, without(&[]))?;
    let mut f = q();
    f.kind = Some(PostKind::Page);
    eq!("kind", run(f).await?, vec![page])?;
    let mut f = q();
    f.status = Some(PostStatus::Draft);
    eq!("status", run(f).await?, vec![draft])?;
    let mut f = q();
    f.lang = Some("en".into());
    eq!("lang", run(f).await?, vec![en])?;
    let mut f = q();
    f.author = Some(bob);
    eq!("author", run(f).await?, vec![by_b])?;
    let mut f = q();
    f.tag = Some(tag);
    eq!("tag", run(f).await?, vec![tagged])?;
    let mut f = q();
    f.series = Some(series);
    eq!("series", run(f).await?, vec![in_series])?;
    let mut f = q();
    f.pinned = Some(true);
    eq!("pinned", run(f).await?, vec![pinned])?;
    let mut f = q();
    f.pinned = Some(false);
    eq!("not pinned", run(f).await?, without(&[pinned]))?;
    let mut f = q();
    f.status = Some(PostStatus::Published);
    f.lang = Some("vi".into());
    f.author = Some(alice);
    f.kind = Some(PostKind::Post);
    eq!(
        "filters combine (AND)",
        run(f).await?,
        without(&[page, draft, en, by_b]),
    )?;
    let mut f = q();
    f.limit = 0;
    eq!("limit 0", run(f).await?, Vec::new())?;
    let summary = ok("list_posts", db.list_posts(&q()).await)?;
    that(
        "summaries carry tag ids",
        summary
            .iter()
            .any(|p| p.id == tagged && p.tag_ids == vec![tag]),
    )
}

/// A limit above [`MAX_PAGE`] returns [`MAX_PAGE`] rows.
pub async fn limits_cap_at_max_page<S: Store>(db: &S) -> Check {
    let over = MAX_PAGE + 1;
    for i in 0..over {
        ok(
            "create_media",
            db.create_media(media(&format!("k{i}"), i64::from(i))).await,
        )?;
        ok(
            "append",
            db.append(event(u64::from(i), EventKind::Deploy, None)?)
                .await,
        )?;
    }
    let m = ok(
        "list_media",
        db.list_media(MediaQuery {
            before: None,
            missing_alt_only: false,
            unused_only: false,
            limit: 1000,
        })
        .await,
    )?;
    eq!("list_media capped", m.len(), crate::page_len(MAX_PAGE))?;
    let l = ok(
        "query_log",
        db.query_log(LogQuery {
            kind: None,
            actor: None,
            from_ms: None,
            to_ms: None,
            before: None,
            limit: u32::MAX,
        })
        .await,
    )?;
    eq!("query_log capped", l.len(), crate::page_len(MAX_PAGE))
}

/// `count_posts_by_status`: zeros, kind and lang filters, trash moves a post's count.
pub async fn post_counts_by_status<S: Store>(db: &S) -> Check {
    let counts = async |kind, lang| {
        let c = ok(
            "count_posts_by_status",
            db.count_posts_by_status(kind, lang).await,
        )?;
        Ok::<_, String>((c.draft, c.scheduled, c.published, c.trashed))
    };
    eq!("empty store", counts(None, None).await?, (0, 0, 0, 0))?;

    let u = user(db, "alice").await?;
    let mut ids = Vec::new();
    for (slug, status, kind, lang) in [
        ("d1", PostStatus::Draft, PostKind::Post, "vi"),
        ("d2", PostStatus::Draft, PostKind::Post, "en"),
        ("p1", PostStatus::Published, PostKind::Post, "vi"),
        ("p2", PostStatus::Published, PostKind::Page, "vi"),
        ("s1", PostStatus::Scheduled, PostKind::Post, "en"),
    ] {
        let mut p = post(u, lang, slug, status);
        p.kind = kind;
        ids.push(save(db, p).await?);
    }
    eq!("no filter", counts(None, None).await?, (2, 1, 2, 0))?;
    eq!(
        "kind",
        counts(Some(PostKind::Post), None).await?,
        (2, 1, 1, 0)
    )?;
    eq!("lang", counts(None, Some("vi")).await?, (1, 0, 2, 0))?;
    eq!(
        "kind and lang, none match",
        counts(Some(PostKind::Page), Some("en")).await?,
        (0, 0, 0, 0)
    )?;

    ok("trash_post", db.trash_post(ids[2], T0 + 1).await)?;
    eq!(
        "trash moves the count",
        counts(None, None).await?,
        (2, 1, 1, 1)
    )?;
    eq!(
        "trash with filters",
        counts(Some(PostKind::Post), Some("vi")).await?,
        (1, 0, 0, 1)
    )?;
    Ok(())
}
