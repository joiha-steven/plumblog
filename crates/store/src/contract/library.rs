//! Media, tags and series.

use plumblog_model::{ErrorCode, MediaId, PostId, SeriesId, TagId};

use super::kit::{
    Check, Got, T0, asc, eq, fails, media, ok, post, save, that, update, user, version, version_is,
};
use crate::{
    MAX_PAGE, MediaQuery, NewSeries, PostKind, PostOrder, PostQuery, PostStatus, Series, Store, Tag,
};

fn page(before: Option<(i64, MediaId)>, missing_alt_only: bool, limit: u32) -> MediaQuery {
    MediaQuery {
        before,
        missing_alt_only,
        unused_only: false,
        limit,
    }
}

/// `tags_page` / `series_page`: slug order (not name or insertion order), 101 rows over two
/// pages at the cap with the probe row intact, a cursor past the end, a limit of 0, and the
/// by-id reads.
pub async fn taxonomy_paging<S: Store>(db: &S) -> Check {
    let (empty, more) = ok("tags_page empty", db.tags_page(None, 10).await)?;
    eq!("empty page", (empty.len(), more), (0, false))?;
    // Names run against slugs, and insertion runs against both.
    let mut slugs = Vec::new();
    for i in (0..101).rev() {
        let slug = format!("t{i:03}");
        ok(
            "ensure_tag",
            db.ensure_tag(&slug, &format!("N{}", 100 - i)).await,
        )?;
        ok(
            "create_series",
            db.create_series(NewSeries {
                slug: slug.clone(),
                title: format!("N{}", 100 - i),
                description: String::new(),
            })
            .await,
        )?;
        slugs.push(slug);
    }
    slugs.sort();
    let first = slugs[..100].to_vec();

    let (p1, more) = ok("tags_page 1", db.tags_page(None, MAX_PAGE).await)?;
    eq!(
        "tags page 1 slugs",
        p1.iter().map(|t| t.slug.clone()).collect::<Vec<_>>(),
        first.clone()
    )?;
    that("tags page 1 has more", more)?;
    let (p2, more) = ok(
        "tags_page 2",
        db.tags_page(Some(&p1[99].slug), MAX_PAGE).await,
    )?;
    eq!("tags page 2", (p2.len(), more), (1, false))?;
    eq!("tags page 2 slug", p2[0].slug.as_str(), slugs[100].as_str())?;
    let (exact, more) = ok("tags_page exact", db.tags_page(Some("t000"), 100).await)?;
    eq!(
        "a page that ends the list has no more",
        (exact.len(), more),
        (100, false)
    )?;
    // A cursor is a slug value, not a row: deleting the last row of a page between two
    // requests must not stop the walk.
    ok("delete_tag", db.delete_tag(p1[99].id).await)?;
    let (after_gone, more) = ok(
        "tags_page after a deleted cursor",
        db.tags_page(Some(&p1[99].slug), MAX_PAGE).await,
    )?;
    eq!(
        "walk continues past a deleted cursor",
        (after_gone.iter().map(|t| t.id).collect::<Vec<_>>(), more),
        (vec![p2[0].id], false)
    )?;
    let (none, more) = ok("tags_page past", db.tags_page(Some("t100"), 5).await)?;
    eq!("past the end", (none.len(), more), (0, false))?;
    let (zero, _) = ok("tags_page 0", db.tags_page(None, 0).await)?;
    eq!("limit 0 returns nothing", zero.len(), 0)?;

    let (s1, more) = ok("series_page 1", db.series_page(None, MAX_PAGE).await)?;
    eq!(
        "series page 1 slugs",
        s1.iter().map(|t| t.slug.clone()).collect::<Vec<_>>(),
        first
    )?;
    that("series page 1 has more", more)?;
    let (s2, more) = ok(
        "series_page 2",
        db.series_page(Some(&s1[99].slug), MAX_PAGE).await,
    )?;
    eq!("series page 2", (s2.len(), more), (1, false))?;

    eq!(
        "get_tag",
        ok("get_tag", db.get_tag(p2[0].id).await)?,
        p2[0].clone()
    )?;
    eq!(
        "get_series",
        ok("get_series", db.get_series(s2[0].id).await)?,
        s2[0].clone()
    )?;
    fails(
        "get_tag missing",
        db.get_tag(TagId(999_999)).await,
        ErrorCode::NotFound,
    )?;
    fails(
        "get_series missing",
        db.get_series(SeriesId(999_999)).await,
        ErrorCode::NotFound,
    )
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

/// `replace_media_variants` is a compare and swap: two adds that read the same text cannot
/// both win, so the loser re-reads and keeps the winner's size; a swap bumps the version once,
/// a refused one not at all; `delete_media` hands back the text as it was at the delete.
pub async fn media_variants_swap<S: Store>(db: &S) -> Check {
    let id = ok("create_media", db.create_media(media("v.png", T0)).await)?;
    let read = ok("get_media", db.get_media(id).await)?.variants_json;
    let a = r#"[{"key":"a"}]"#;
    let b = r#"[{"key":"b"}]"#;
    let both = r#"[{"key":"a"},{"key":"b"}]"#;
    let v = version(db).await?;
    // Both requests read `read`; A swaps first.
    ok("swap a", db.replace_media_variants(id, &read, a).await)?;
    version_is(db, "a swap", v + 1).await?;
    fails(
        "swap b from the stale read",
        db.replace_media_variants(id, &read, b).await,
        ErrorCode::Conflict,
    )?;
    version_is(db, "a refused swap", v + 1).await?;
    eq!(
        "the winner's size is kept",
        ok("get_media", db.get_media(id).await)?.variants_json,
        a.to_owned()
    )?;
    // B re-reads and appends.
    ok("swap b again", db.replace_media_variants(id, a, both).await)?;
    eq!(
        "both sizes",
        ok("get_media", db.get_media(id).await)?.variants_json,
        both.to_owned()
    )?;
    fails(
        "swap missing",
        db.replace_media_variants(MediaId(id.0 + 1000), "[]", a)
            .await,
        ErrorCode::NotFound,
    )?;
    eq!(
        "delete returns the stored variants",
        ok("delete_media", db.delete_media(id).await)?,
        both.to_owned()
    )
}

/// `sha256` is unique per uploader and found again by (uploader, sha256); NULL never matches
/// or collides, and another uploader's equal hash is their own item.
pub async fn media_sha256_per_uploader<S: Store>(db: &S) -> Check {
    let a = user(db, "alice").await?;
    let b = user(db, "bob").await?;
    let h = "ab".repeat(32);
    let with = |key: &str, u, hash: Option<&str>| {
        let mut m = media(key, T0);
        m.uploaded_by = Some(u);
        m.sha256 = hash.map(str::to_owned);
        m
    };
    eq!(
        "find before any upload",
        ok("find", db.find_media_by_sha256(a, &h).await)?,
        None,
    )?;
    // Rows without a hash (old rows, clients that send none) neither match nor collide.
    ok("no hash", db.create_media(with("n1.png", a, None)).await)?;
    ok(
        "no hash again",
        db.create_media(with("n2.png", a, None)).await,
    )?;
    eq!(
        "a NULL sha256 is not found",
        ok("find", db.find_media_by_sha256(a, &h).await)?,
        None,
    )?;
    let first = ok(
        "create a",
        db.create_media(with("a.png", a, Some(&h))).await,
    )?;
    let got = ok("find a", db.find_media_by_sha256(a, &h).await)?;
    eq!(
        "found a",
        got.as_ref().map(|m| (m.id, m.sha256.clone())),
        Some((first, Some(h.clone()))),
    )?;
    eq!(
        "get_media returns sha256",
        ok("get_media", db.get_media(first).await)?.sha256,
        Some(h.clone()),
    )?;
    match db.create_media(with("a2.png", a, Some(&h))).await {
        Err(e) if e.code == ErrorCode::Conflict && e.field.as_deref() == Some("sha256") => {}
        other => {
            return Err(format!(
                "duplicate sha256: expected Conflict on sha256, got {other:?}"
            ));
        }
    }
    // The duplicate r2_key stays a Conflict without the sha256 field.
    match db.create_media(with("a.png", b, None)).await {
        Err(e) if e.code == ErrorCode::Conflict && e.field.is_none() => {}
        other => {
            return Err(format!(
                "duplicate r2_key: expected Conflict, no field, got {other:?}"
            ));
        }
    }
    let theirs = ok(
        "create b",
        db.create_media(with("b.png", b, Some(&h))).await,
    )?;
    that("another uploader gets their own item", theirs != first)?;
    eq!(
        "find b",
        ok("find b", db.find_media_by_sha256(b, &h).await)?.map(|m| m.id),
        Some(theirs),
    )?;
    eq!(
        "find a still",
        ok("find a", db.find_media_by_sha256(a, &h).await)?.map(|m| m.id),
        Some(first),
    )?;
    eq!(
        "another hash",
        ok("find", db.find_media_by_sha256(a, &"cd".repeat(32)).await)?,
        None,
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

/// Deleting a series changes its posts' rows, so their version moves: an editor holding the
/// old version gets `Conflict` instead of silently writing the deleted series back.
pub async fn series_delete_stales_editors<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let s = ok("create_series", db.create_series(series("s", "S")).await)?;
    let other = ok("create_series", db.create_series(series("o", "O")).await)?;
    let mut p = post(u, "vi", "in", PostStatus::Draft);
    p.series_id = Some(s);
    p.series_position = Some(1);
    let id = save(db, p.clone()).await?;
    let mut q = post(u, "vi", "out", PostStatus::Draft);
    q.series_id = Some(other);
    let out = save(db, q).await?;
    // The editor opened the post before the delete.
    let stale = update(db, id, p).await?;
    let before = ok("get_post", db.get_post(id).await)?.version;
    let out_before = ok("get_post", db.get_post(out).await)?.version;
    ok("delete_series", db.delete_series(s).await)?;
    eq!(
        "post leaving the series gets a new version",
        ok("get_post", db.get_post(id).await)?.version,
        before + 1,
    )?;
    eq!(
        "post in another series keeps its version",
        ok("get_post", db.get_post(out).await)?.version,
        out_before,
    )?;
    fails(
        "stale save after the series was deleted",
        db.save_post(stale).await,
        ErrorCode::Conflict,
    )
}

/// Deleting a tag changes its posts, so their version moves: an editor holding the old
/// version gets `Conflict` instead of a confusing missing-tag error.
pub async fn tag_delete_stales_editors<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let t = ok("ensure_tag", db.ensure_tag("t", "T").await)?;
    let other = ok("ensure_tag", db.ensure_tag("o", "O").await)?;
    let mut p = post(u, "vi", "in", PostStatus::Draft);
    p.tag_ids = vec![t, other];
    let id = save(db, p.clone()).await?;
    let mut q = post(u, "vi", "out", PostStatus::Draft);
    q.tag_ids = vec![other];
    let out = save(db, q).await?;
    let stale = update(db, id, p).await?;
    let before = ok("get_post", db.get_post(id).await)?.version;
    let out_before = ok("get_post", db.get_post(out).await)?.version;
    ok("delete_tag", db.delete_tag(t).await)?;
    eq!(
        "post losing the tag gets a new version",
        ok("get_post", db.get_post(id).await)?.version,
        before + 1,
    )?;
    eq!(
        "post without the tag keeps its version",
        ok("get_post", db.get_post(out).await)?.version,
        out_before,
    )?;
    fails(
        "stale save after the tag was deleted",
        db.save_post(stale).await,
        ErrorCode::Conflict,
    )
}

/// `public_media`: only media a `published` post or page uses (body or cover) comes back;
/// drafts, scheduled, trashed, unused and unknown all give `None`, and trashing or
/// unpublishing takes it away again.
pub async fn public_media_follows_live_posts<S: Store>(db: &S) -> Check {
    let alice = user(db, "alice").await?;
    let mut ids = Vec::new();
    for i in 0..8 {
        let key = format!("p{i}.png");
        ids.push(ok("create_media", db.create_media(media(&key, i)).await)?);
    }
    let [body, cover, page, draft, scheduled, trashed, unused, later] = ids[..] else {
        return Err("eight media".into());
    };
    let public = |id: MediaId| async move {
        ok("public_media", db.public_media(id).await).map(|m| m.map(|m| m.id))
    };
    let mut live = post(alice, "en", "live", PostStatus::Published);
    live.media_ids = vec![body];
    live.cover_media_id = Some(cover);
    let live_id = save(db, live.clone()).await?;
    let mut about = post(alice, "en", "about", PostStatus::Published);
    about.kind = PostKind::Page;
    about.media_ids = vec![page];
    save(db, about).await?;
    let mut drafted = post(alice, "en", "draft", PostStatus::Draft);
    drafted.media_ids = vec![draft, later];
    save(db, drafted).await?;
    let mut soon = post(alice, "en", "soon", PostStatus::Scheduled);
    soon.cover_media_id = Some(scheduled);
    save(db, soon).await?;
    let mut gone = post(alice, "en", "gone", PostStatus::Published);
    gone.media_ids = vec![trashed];
    let t_id = save(db, gone).await?;
    ok("trash_post", db.trash_post(t_id, T0 + 1).await)?;

    let row = ok("public_media body", db.public_media(body).await)?;
    let want = ok("get_media", db.get_media(body).await)?;
    eq!("body image: the whole row", row, Some(want))?;
    eq!("cover", public(cover).await?, Some(cover))?;
    eq!("published page", public(page).await?, Some(page))?;
    for (what, id) in [
        ("draft only", draft),
        ("scheduled only", scheduled),
        ("trashed only", trashed),
        ("unused", unused),
        ("unknown", MediaId(later.0 + 1000)),
    ] {
        eq!(what, public(id).await?, None)?;
    }

    // Also used by a live post: public; that post unpublished: not any more.
    let mut edit = update(db, live_id, live).await?;
    edit.media_ids = vec![body, later];
    save(db, edit.clone()).await?;
    eq!(
        "draft media now also live",
        public(later).await?,
        Some(later)
    )?;
    let mut off = update(db, live_id, edit).await?;
    off.status = PostStatus::Draft;
    off.published_at = None;
    save(db, off).await?;
    eq!("unpublished: body", public(body).await?, None)?;
    eq!("unpublished: cover", public(cover).await?, None)?;
    eq!("unpublished: shared", public(later).await?, None)
}
