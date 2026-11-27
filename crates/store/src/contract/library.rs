//! Media, tags and series.

use plumblog_model::{ErrorCode, MediaId, PostId, SeriesId, TagId};

use super::kit::{
    Check, Got, T0, asc, eq, fails, media, ok, post, save, that, update, user, version, version_is,
};
use crate::{MediaQuery, NewSeries, PostOrder, PostQuery, PostStatus, Series, Store, Tag};

fn page(before: Option<(i64, MediaId)>, missing_alt_only: bool, limit: u32) -> MediaQuery {
    MediaQuery {
        before,
        missing_alt_only,
        unused_only: false,
        limit,
    }
}

pub async fn media_round_trip<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let mut m = media("a.png", T0);
    m.uploaded_by = Some(u);
    m.alt = "A cat".into();
    let id = ok("create_media", db.create_media(m.clone()).await)?;
    let got = ok("get_media", db.get_media(id).await)?;
    eq!(
        "media fields",
        (
            got.r2_key.as_str(),
            got.mime.as_str(),
            got.bytes,
            got.width,
            got.height,
            got.alt.as_str(),
            got.variants_json.as_str(),
            got.uploaded_by,
            got.created_at,
        ),
        ("a.png", "image/png", 10, 4, 3, "A cat", "{}", Some(u), T0),
    )?;
    ok("set_media_alt", db.set_media_alt(id, "A dog").await)?;
    eq!(
        "alt after set",
        ok("get_media", db.get_media(id).await)?.alt,
        "A dog".to_owned(),
    )?;
    let missing = MediaId(id.0 + 1000);
    fails(
        "get_media missing",
        db.get_media(missing).await,
        ErrorCode::NotFound,
    )?;
    fails(
        "set_media_alt missing",
        db.set_media_alt(missing, "x").await,
        ErrorCode::NotFound,
    )?;
    fails(
        "delete_media missing",
        db.delete_media(missing).await,
        ErrorCode::NotFound,
    )?;
    fails(
        "create_media duplicate r2_key",
        db.create_media(media("a.png", T0)).await,
        ErrorCode::Conflict,
    )?;
    let mut bad = media("b.png", T0);
    bad.bytes = -1;
    fails(
        "negative bytes",
        db.create_media(bad).await,
        ErrorCode::InvalidInput,
    )?;
    let mut bad = media("c.png", T0);
    bad.width = 0;
    fails(
        "zero width",
        db.create_media(bad).await,
        ErrorCode::InvalidInput,
    )?;
    let mut bad = media("d.png", T0);
    bad.height = 0;
    fails(
        "zero height",
        db.create_media(bad).await,
        ErrorCode::InvalidInput,
    )?;
    let mut bad = media("e.png", T0);
    bad.uploaded_by = Some(plumblog_model::UserId(u.0 + 1000));
    fails(
        "missing uploader",
        db.create_media(bad).await,
        ErrorCode::InvalidInput,
    )?;
    let mut zero = media("f.png", T0);
    zero.bytes = 0;
    ok("zero bytes is allowed", db.create_media(zero).await)?;
    ok("delete_media", db.delete_media(id).await)?;
    fails(
        "get deleted media",
        db.get_media(id).await,
        ErrorCode::NotFound,
    )
}

/// Newest first with equal `created_at`; the cursor starts strictly after the last row.
pub async fn media_paging<S: Store>(db: &S) -> Check {
    let mut ids = Vec::new();
    for (i, at) in [100, 200, 200, 200, 300].into_iter().enumerate() {
        let mut m = media(&format!("m{i}.png"), at);
        if i % 2 == 0 {
            m.alt = "described".into();
        }
        ids.push((at, ok("create_media", db.create_media(m).await)?));
    }
    let mut want = ids.clone();
    want.sort_unstable_by(|a, b| b.cmp(a));
    let mut seen = Vec::new();
    let mut before = None;
    for _ in 0..5 {
        let rows = ok("list_media", db.list_media(page(before, false, 2)).await)?;
        that("page size <= 2", rows.len() <= 2)?;
        let Some(last) = rows.last() else { break };
        before = Some((last.created_at, last.id));
        seen.extend(rows.iter().map(|m| (m.created_at, m.id)));
    }
    eq!("media pages, newest first", seen, want.clone())?;
    let missing: Vec<MediaId> = ok("list_media", db.list_media(page(None, true, 10)).await)?
        .iter()
        .map(|m| m.id)
        .collect();
    let want_missing: Vec<MediaId> = want
        .iter()
        .filter(|(_, id)| {
            ids.iter()
                .position(|x| x.1 == *id)
                .is_some_and(|i| i % 2 == 1)
        })
        .map(|x| x.1)
        .collect();
    eq!("missing_alt_only", missing, want_missing)?;
    eq!(
        "limit 0",
        ok("list_media", db.list_media(page(None, false, 0)).await)?.len(),
        0,
    )
}

fn unused(limit: u32) -> MediaQuery {
    MediaQuery {
        unused_only: true,
        ..page(None, false, limit)
    }
}

async fn usage<S: Store>(db: &S, id: MediaId) -> Got<Vec<PostId>> {
    ok("media_usage", db.media_usage(id).await)
}

/// Body media and covers both count as use; saves replace the body list; a save with
/// unknown media writes nothing; a permanently deleted post frees its media.
pub async fn media_usage_tracks_saves<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let m1 = ok("create_media", db.create_media(media("1.png", 100)).await)?;
    let m2 = ok("create_media", db.create_media(media("2.png", 200)).await)?;
    let m3 = ok("create_media", db.create_media(media("3.png", 300)).await)?;
    let m4 = ok("create_media", db.create_media(media("4.png", 400)).await)?;
    let mut first = post(u, "vi", "a", PostStatus::Draft);
    first.media_ids = vec![m2, m1, m1];
    let a_id = save(db, first.clone()).await?;
    let mut second = post(u, "vi", "b", PostStatus::Published);
    second.cover_media_id = Some(m1);
    second.media_ids = vec![m3, m1];
    let b_id = save(db, second).await?;
    eq!(
        "m1: body and cover, ascending, once",
        usage(db, m1).await?,
        asc(&[a_id, b_id])
    )?;
    eq!("m2: body", usage(db, m2).await?, vec![a_id])?;
    eq!("m3: body", usage(db, m3).await?, vec![b_id])?;
    eq!("m4: unused", usage(db, m4).await?, Vec::new())?;
    eq!(
        "unknown media",
        usage(db, MediaId(m4.0 + 1000)).await?,
        Vec::new()
    )?;
    let ids = |rows: Vec<crate::Media>| rows.iter().map(|m| m.id).collect::<Vec<_>>();
    eq!(
        "unused_only",
        ids(ok("list_media", db.list_media(unused(10)).await)?),
        vec![m4]
    )?;

    let mut edit = update(db, a_id, first).await?;
    edit.media_ids = vec![m2];
    save(db, edit.clone()).await?;
    eq!(
        "save replaces the body list",
        usage(db, m1).await?,
        vec![b_id]
    )?;

    // Unknown media: nothing written, on update and on insert.
    let before = ok("get_post", db.get_post(a_id).await)?;
    let v = version(db).await?;
    let mut bad = update(db, a_id, edit).await?;
    bad.title = "Changed".into();
    bad.media_ids = vec![m4, MediaId(m4.0 + 1000)];
    fails(
        "update with unknown media",
        db.save_post(bad).await,
        ErrorCode::InvalidInput,
    )?;
    eq!(
        "post unchanged",
        ok("get_post", db.get_post(a_id).await)?,
        before
    )?;
    eq!("m4 still unused", usage(db, m4).await?, Vec::new())?;
    let mut bad = post(u, "vi", "c", PostStatus::Published);
    bad.media_ids = vec![m4, MediaId(m4.0 + 1000)];
    fails(
        "insert with unknown media",
        db.save_post(bad).await,
        ErrorCode::InvalidInput,
    )?;
    eq!(
        "no post",
        ok("find_post", db.find_post("vi", "c").await)?,
        None
    )?;
    eq!(
        "m4 still unused after insert",
        usage(db, m4).await?,
        Vec::new()
    )?;
    version_is(db, "saves with unknown media", v).await?;

    ok("trash_post", db.trash_post(b_id, T0).await)?;
    eq!(
        "a trashed post still uses its media",
        usage(db, m3).await?,
        vec![b_id]
    )?;
    ok("delete_post", db.delete_post(b_id).await)?;
    eq!(
        "deleted post frees body media",
        usage(db, m3).await?,
        Vec::new()
    )?;
    eq!(
        "deleted post frees its cover",
        usage(db, m1).await?,
        Vec::new()
    )?;
    eq!(
        "unused_only, newest first",
        ids(ok("list_media", db.list_media(unused(10)).await)?),
        vec![m4, m3, m1],
    )
}

/// Media in use (body or cover, any status) cannot be deleted: `Conflict` listing the
/// posts, nothing deleted, no bump. Unused media deletes without a bump.
pub async fn media_in_use_cannot_be_deleted<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let body = ok("create_media", db.create_media(media("1.png", T0)).await)?;
    let cover = ok("create_media", db.create_media(media("2.png", T0)).await)?;
    let free = ok("create_media", db.create_media(media("3.png", T0)).await)?;
    let mut live = post(u, "vi", "live", PostStatus::Published);
    live.media_ids = vec![body];
    let live_id = save(db, live.clone()).await?;
    let mut gone = post(u, "vi", "gone", PostStatus::Draft);
    gone.media_ids = vec![body];
    let gone_id = save(db, gone).await?;
    ok("trash_post", db.trash_post(gone_id, T0).await)?;
    let mut d = post(u, "vi", "draft", PostStatus::Draft);
    d.cover_media_id = Some(cover);
    let d_id = save(db, d.clone()).await?;
    let v = version(db).await?;
    for (what, id, users) in [
        ("body media", body, asc(&[live_id, gone_id])),
        ("cover media", cover, vec![d_id]),
    ] {
        match db.delete_media(id).await {
            Err(e) if e.code == ErrorCode::Conflict => {
                for p in &users {
                    that(
                        &format!("{what}: detail {:?} lists post {}", e.detail, p.0),
                        e.detail.contains(&p.0.to_string()),
                    )?;
                }
            }
            other => return Err(format!("delete {what}: expected Conflict, got {other:?}")),
        }
        ok("media kept", db.get_media(id).await)?;
        eq!("usage kept", usage(db, id).await?, users)?;
    }
    eq!(
        "cover kept",
        ok("get_post", db.get_post(d_id).await)?.cover_media_id,
        Some(cover),
    )?;
    version_is(db, "refused deletes", v).await?;
    ok("delete unused media", db.delete_media(free).await)?;
    version_is(db, "delete unused media", v).await?;

    let mut e = update(db, live_id, live).await?;
    e.media_ids = Vec::new();
    save(db, e).await?;
    ok("delete_post", db.delete_post(gone_id).await)?;
    let mut e = update(db, d_id, d).await?;
    e.cover_media_id = None;
    save(db, e).await?;
    ok("delete freed body media", db.delete_media(body).await)?;
    ok("delete freed cover", db.delete_media(cover).await)?;
    fails(
        "delete deleted media",
        db.delete_media(cover).await,
        ErrorCode::NotFound,
    )
}

pub async fn tags_round_trip<S: Store>(db: &S) -> Check {
    let a = ok("ensure_tag", db.ensure_tag("rust", "Rust").await)?;
    let again = ok("ensure_tag existing", db.ensure_tag("rust", "Other").await)?;
    eq!("ensure_tag returns the existing id", again, a)?;
    let found = ok("find_tag", db.find_tag("rust").await)?;
    eq!(
        "existing tag keeps its name",
        found,
        Some(Tag {
            id: a,
            slug: "rust".into(),
            name: "Rust".into(),
        }),
    )?;
    eq!(
        "find_tag unknown",
        ok("find_tag", db.find_tag("go").await)?,
        None,
    )?;
    let b = ok("ensure_tag", db.ensure_tag("b", "Same").await)?;
    let c = ok("ensure_tag", db.ensure_tag("c", "Same").await)?;
    let z = ok("ensure_tag", db.ensure_tag("z", "Alpha").await)?;
    let order: Vec<TagId> = ok("list_tags", db.list_tags().await)?
        .iter()
        .map(|t| t.id)
        .collect();
    let mut want = vec![z, a];
    want.extend(asc(&[b, c]));
    eq!("list_tags by name, then id", order, want)?;
    ok(
        "update_tag",
        db.update_tag(&Tag {
            id: a,
            slug: "rust-lang".into(),
            name: "Rust lang".into(),
        })
        .await,
    )?;
    eq!(
        "renamed",
        ok("find_tag", db.find_tag("rust-lang").await)?.map(|t| t.name),
        Some("Rust lang".into()),
    )?;
    eq!(
        "old slug gone",
        ok("find_tag", db.find_tag("rust").await)?,
        None,
    )?;
    fails(
        "update_tag to taken slug",
        db.update_tag(&Tag {
            id: a,
            slug: "b".into(),
            name: "x".into(),
        })
        .await,
        ErrorCode::Conflict,
    )?;
    let missing = TagId(a.0.max(z.0) + 1000);
    fails(
        "update_tag missing",
        db.update_tag(&Tag {
            id: missing,
            slug: "q".into(),
            name: "q".into(),
        })
        .await,
        ErrorCode::NotFound,
    )?;
    fails(
        "delete_tag missing",
        db.delete_tag(missing).await,
        ErrorCode::NotFound,
    )?;
    ok("delete_tag", db.delete_tag(z).await)?;
    eq!("deleted tag", ok("find_tag", db.find_tag("z").await)?, None)
}

/// Tag updates and deletes bump only when a published post has the tag; delete unlinks.
pub async fn tag_links_and_versions<S: Store>(db: &S) -> Check {
    let author = user(db, "alice").await?;
    let t_draft = ok("ensure_tag", db.ensure_tag("d", "D").await)?;
    let t_live = ok("ensure_tag", db.ensure_tag("l", "L").await)?;
    let mut d = post(author, "vi", "draft", PostStatus::Draft);
    d.tag_ids = vec![t_live, t_draft, t_draft];
    let d = save(db, d).await?;
    let mut p = post(author, "vi", "live", PostStatus::Published);
    p.tag_ids = vec![t_live];
    let p = save(db, p).await?;
    eq!(
        "tag_ids ascending, deduplicated",
        ok("get_post", db.get_post(d).await)?.tag_ids,
        asc(&[t_draft, t_live]),
    )?;
    let v = version(db).await?;
    let rename = |id, slug: &str| Tag {
        id,
        slug: slug.into(),
        name: slug.to_uppercase(),
    };
    ok(
        "update_tag (draft only)",
        db.update_tag(&rename(t_draft, "dd")).await,
    )?;
    version_is(db, "update_tag on a draft-only tag", v).await?;
    ok(
        "update_tag (published)",
        db.update_tag(&rename(t_live, "ll")).await,
    )?;
    version_is(db, "update_tag on a published post'db tag", v + 1).await?;
    ok("delete_tag (draft only)", db.delete_tag(t_draft).await)?;
    version_is(db, "delete_tag on a draft-only tag", v + 1).await?;
    eq!(
        "deleted tag unlinked",
        ok("get_post", db.get_post(d).await)?.tag_ids,
        vec![t_live],
    )?;
    ok("delete_tag (published)", db.delete_tag(t_live).await)?;
    version_is(db, "delete_tag on a published post'db tag", v + 2).await?;
    eq!(
        "published post unlinked",
        ok("get_post", db.get_post(p).await)?.tag_ids,
        Vec::new(),
    )?;
    let mut q = PostQuery::new(PostOrder::UpdatedDesc, 10);
    q.tag = Some(t_live);
    eq!(
        "no post lists under a deleted tag",
        ok("list_posts", db.list_posts(&q).await)?.len(),
        0,
    )
}

fn series(slug: &str, title: &str) -> NewSeries {
    NewSeries {
        slug: slug.into(),
        title: title.into(),
        description: format!("about {slug}"),
    }
}

pub async fn series_round_trip<S: Store>(db: &S) -> Check {
    let a = ok("create_series", db.create_series(series("a", "Same")).await)?;
    let b = ok("create_series", db.create_series(series("b", "Same")).await)?;
    let c = ok(
        "create_series",
        db.create_series(series("c", "Alpha")).await,
    )?;
    fails(
        "create_series duplicate slug",
        db.create_series(series("a", "x")).await,
        ErrorCode::Conflict,
    )?;
    let order: Vec<SeriesId> = ok("list_series", db.list_series().await)?
        .iter()
        .map(|x| x.id)
        .collect();
    let mut want = vec![c];
    want.extend(asc(&[a, b]));
    eq!("list_series by title, then id", order, want)?;
    let found = ok("find_series", db.find_series("a").await)?;
    eq!(
        "find_series",
        found,
        Some(Series {
            id: a,
            slug: "a".into(),
            title: "Same".into(),
            description: "about a".into(),
        }),
    )?;
    eq!(
        "find_series unknown",
        ok("find_series", db.find_series("q").await)?,
        None,
    )?;
    let changed = Series {
        id: a,
        slug: "a2".into(),
        title: "New".into(),
        description: "d".into(),
    };
    ok("update_series", db.update_series(&changed).await)?;
    eq!(
        "updated series",
        ok("find_series", db.find_series("a2").await)?,
        Some(changed),
    )?;
    fails(
        "update_series to taken slug",
        db.update_series(&Series {
            id: a,
            slug: "b".into(),
            title: "t".into(),
            description: String::new(),
        })
        .await,
        ErrorCode::Conflict,
    )?;
    let missing = SeriesId(a.0.max(b.0).max(c.0) + 1000);
    fails(
        "update_series missing",
        db.update_series(&Series {
            id: missing,
            slug: "zz".into(),
            title: "t".into(),
            description: String::new(),
        })
        .await,
        ErrorCode::NotFound,
    )?;
    fails(
        "delete_series missing",
        db.delete_series(missing).await,
        ErrorCode::NotFound,
    )?;
    ok("delete_series", db.delete_series(c).await)?;
    eq!(
        "deleted series",
        ok("find_series", db.find_series("c").await)?,
        None,
    )
}

/// Series updates and deletes bump only when a published post is in it; delete clears
/// `series_id` and `series_position`.
pub async fn series_links_and_versions<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let sd = ok("create_series", db.create_series(series("d", "D")).await)?;
    let sl = ok("create_series", db.create_series(series("l", "L")).await)?;
    let mut d = post(u, "vi", "draft", PostStatus::Draft);
    d.series_id = Some(sd);
    d.series_position = Some(1);
    let d = save(db, d).await?;
    let mut p = post(u, "vi", "live", PostStatus::Published);
    p.series_id = Some(sl);
    p.series_position = Some(2);
    let p = save(db, p.clone()).await?;
    let v = version(db).await?;
    let edit = |id, slug: &str| Series {
        id,
        slug: slug.into(),
        title: slug.into(),
        description: String::new(),
    };
    ok(
        "update_series (draft only)",
        db.update_series(&edit(sd, "d2")).await,
    )?;
    version_is(db, "update_series with only drafts", v).await?;
    ok(
        "update_series (published)",
        db.update_series(&edit(sl, "l2")).await,
    )?;
    version_is(db, "update_series with a published post", v + 1).await?;
    ok("delete_series (draft only)", db.delete_series(sd).await)?;
    version_is(db, "delete_series with only drafts", v + 1).await?;
    let got = ok("get_post", db.get_post(d).await)?;
    eq!(
        "draft leaves the series",
        (got.series_id, got.series_position),
        (None, None),
    )?;
    ok("delete_series (published)", db.delete_series(sl).await)?;
    version_is(db, "delete_series with a published post", v + 2).await?;
    let got = ok("get_post", db.get_post(p).await)?;
    eq!(
        "published post leaves the series",
        (got.series_id, got.series_position),
        (None, None),
    )?;
    let empty = ok("list_series", db.list_series().await)?;
    eq!("no series left", empty.len(), 0)?;
    // Saving with a deleted series is a missing reference.
    let mut again = post(u, "vi", "live", PostStatus::Published);
    again.series_id = Some(sl);
    fails(
        "save_post with deleted series",
        db.save_post(update(db, p, again).await?).await,
        ErrorCode::InvalidInput,
    )?;
    Ok(())
}
