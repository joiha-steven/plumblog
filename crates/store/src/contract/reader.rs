//! `ReaderStore`: what the public site reads. Published posts only, pinned first on the home
//! listing alone, OFFSET paging behind a count, years and tags of published posts.

use plumblog_model::{PostId, TagId, UserId};

use super::kit::{Check, Got, eq, ok, post, save, user};
use crate::{
    PostKind, PostStatus, READER_MAX_OFFSET, ReaderFilter, ReaderPage, ReaderTag, SavePost, Store,
    year_bounds,
};

/// A published post (kind `post`) in `lang` at `at`.
fn at(u: UserId, lang: &str, slug: &str, at: i64) -> SavePost {
    let mut p = post(u, lang, slug, PostStatus::Published);
    p.published_at = Some(at);
    p
}

async fn page<S: Store>(db: &S, f: ReaderFilter<'_>, offset: u32, limit: u32) -> Got<ReaderPage> {
    ok(
        &format!("reader_posts {f:?} offset {offset} limit {limit}"),
        db.reader_posts(f, offset, limit).await,
    )
}

/// `(total, ids)` of a listing page.
async fn ids<S: Store>(
    db: &S,
    f: ReaderFilter<'_>,
    offset: u32,
    limit: u32,
) -> Got<(u64, Vec<PostId>)> {
    let p = page(db, f, offset, limit).await?;
    Ok((p.total, p.rows.iter().map(|r| r.post.id).collect()))
}

fn slugs<'a>(links: impl IntoIterator<Item = &'a crate::ReaderLink>) -> Vec<String> {
    links
        .into_iter()
        .map(|l| format!("{}/{}", l.lang, l.slug))
        .collect()
}

/// Statuses and kinds that are never listed, and the order of each filter.
pub async fn reader_posts_published_only<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let pa = save(db, at(u, "vi", "a", 100)).await?;
    let pb = save(db, at(u, "vi", "b", 300)).await?;
    let pc = save(db, at(u, "vi", "c", 300)).await?;
    let mut pinned = at(u, "vi", "d", 200);
    pinned.pinned = true;
    let pd = save(db, pinned).await?;
    let px = save(db, at(u, "en", "x", 250)).await?;
    save(db, post(u, "vi", "draft", PostStatus::Draft)).await?;
    let mut sched = post(u, "vi", "sched", PostStatus::Scheduled);
    sched.published_at = Some(500);
    save(db, sched).await?;
    let mut pg = at(u, "vi", "about", 400);
    pg.kind = PostKind::Page;
    save(db, pg).await?;
    let gone = save(db, at(u, "vi", "gone", 600)).await?;
    ok("trash_post", db.trash_post(gone, 700).await)?;

    eq!(
        "Home vi: pinned first, then newest, id DESC on a tie",
        ids(db, ReaderFilter::Home("vi"), 0, 10).await?,
        (4, vec![pd, pc, pb, pa])
    )?;
    eq!(
        "Home en: its own language only",
        ids(db, ReaderFilter::Home("en"), 0, 10).await?,
        (1, vec![px])
    )?;
    eq!(
        "Lang vi: by date only, pinned not first",
        ids(db, ReaderFilter::Lang("vi"), 0, 10).await?,
        (4, vec![pc, pb, pd, pa])
    )?;
    eq!(
        "Lang en",
        ids(db, ReaderFilter::Lang("en"), 0, 10).await?,
        (1, vec![px])
    )?;
    eq!(
        "Lang with no posts",
        ids(db, ReaderFilter::Lang("fr"), 0, 10).await?,
        (0, vec![])
    )?;

    // A page is found by its URL but never listed; other statuses are not found.
    let about = ok("reader_post about", db.reader_post("vi", "about").await)?;
    eq!(
        "reader_post finds a published page",
        about.map(|p| (p.post.kind, p.post.slug)),
        Some((PostKind::Page, "about".to_owned()))
    )?;
    for slug in ["draft", "sched", "gone"] {
        let got = ok(
            &format!("reader_post {slug}"),
            db.reader_post("vi", slug).await,
        )?;
        eq!(&format!("reader_post {slug}"), got.is_none(), true)?;
    }
    eq!(
        "reader_post in the wrong language",
        ok("reader_post en/a", db.reader_post("en", "a").await)?.is_none(),
        true
    )?;
    let got = ok("reader_post vi/a", db.reader_post("vi", "a").await)?;
    eq!(
        "reader_post vi/a",
        got.map(|p| (p.post.id, p.post.html, p.tags)),
        Some((pa, "<h1>a</h1>".to_owned(), vec![]))
    )
}

/// OFFSET and LIMIT, the true total on every page, and nothing past the end or the cap.
pub async fn reader_posts_offset_and_count<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let mut all = Vec::new();
    for i in 0..5 {
        all.push(save(db, at(u, "vi", &format!("p{i}"), 100 * (i + 1))).await?);
    }
    all.reverse();
    eq!(
        "offset 0 limit 2",
        ids(db, ReaderFilter::Home("vi"), 0, 2).await?,
        (5, all[0..2].to_vec())
    )?;
    eq!(
        "offset 2 limit 2",
        ids(db, ReaderFilter::Home("vi"), 2, 2).await?,
        (5, all[2..4].to_vec())
    )?;
    eq!(
        "offset 4 limit 10: the last row",
        ids(db, ReaderFilter::Home("vi"), 4, 10).await?,
        (5, all[4..].to_vec())
    )?;
    eq!(
        "limit 0",
        ids(db, ReaderFilter::Home("vi"), 0, 0).await?,
        (5, vec![])
    )?;
    for offset in [5, 6, READER_MAX_OFFSET, u32::MAX] {
        eq!(
            &format!("offset {offset}: past the end, total kept"),
            ids(db, ReaderFilter::Home("vi"), offset, 10).await?,
            (5, vec![])
        )?;
    }
    eq!(
        "Lang offset past the end",
        ids(db, ReaderFilter::Lang("vi"), 5, 10).await?,
        (5, vec![])
    )
}

/// Vietnamese with d-stroke, and CJK: names are stored and returned byte for byte.
const VIET: &str = "Ti\u{1ebf}ng Vi\u{1ec7}t \u{111} \u{65e5}\u{672c}";

fn tag(slug: &str, name: &str) -> ReaderTag {
    ReaderTag {
        slug: slug.to_owned(),
        name: name.to_owned(),
    }
}

/// The tag filter, tag rows with awkward names in id order, and the tag list.
pub async fn reader_tags_and_tag_filter<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let comma = ok("ensure_tag comma", db.ensure_tag("comma", "a, b").await)?;
    let quote = ok(
        "ensure_tag quote",
        db.ensure_tag("quote", "say \"hi\" 'there' \\ ]").await,
    )?;
    let viet = ok("ensure_tag viet", db.ensure_tag("viet", VIET).await)?;
    let draft_only = ok(
        "ensure_tag draft",
        db.ensure_tag("draft", "Draft only").await,
    )?;
    let page_only = ok("ensure_tag page", db.ensure_tag("page", "Page only").await)?;
    ok("ensure_tag unused", db.ensure_tag("unused", "Unused").await)?;
    let tagged = |slug: &str, when: i64, tags: &[TagId]| {
        let mut p = at(u, "vi", slug, when);
        p.tag_ids = tags.to_vec();
        p
    };
    let mut pinned = tagged("p1", 100, &[viet, comma]);
    pinned.pinned = true;
    let p1 = save(db, pinned).await?;
    let p2 = save(db, tagged("p2", 300, &[comma])).await?;
    let p3 = save(db, tagged("p3", 200, &[quote, comma])).await?;
    let mut d = tagged("d", 400, &[comma, draft_only]);
    d.status = PostStatus::Draft;
    d.published_at = None;
    save(db, d).await?;
    let mut pg = tagged("pg", 500, &[page_only, comma]);
    pg.kind = PostKind::Page;
    save(db, pg).await?;

    eq!(
        "Tag comma: by date only, no draft, no page",
        ids(db, ReaderFilter::Tag(comma), 0, 10).await?,
        (3, vec![p2, p3, p1])
    )?;
    eq!(
        "Tag comma offset 1",
        ids(db, ReaderFilter::Tag(comma), 1, 1).await?,
        (3, vec![p3])
    )?;
    eq!(
        "Tag comma past the end",
        ids(db, ReaderFilter::Tag(comma), 3, 10).await?,
        (3, vec![])
    )?;
    eq!(
        "Tag with only a draft",
        ids(db, ReaderFilter::Tag(draft_only), 0, 10).await?,
        (0, vec![])
    )?;

    let home = page(db, ReaderFilter::Home("vi"), 0, 10).await?;
    let row_tags: Vec<(PostId, Vec<ReaderTag>)> =
        home.rows.into_iter().map(|r| (r.post.id, r.tags)).collect();
    eq!(
        "row tags ascending by tag id, names exact",
        row_tags,
        vec![
            (p1, vec![tag("comma", "a, b"), tag("viet", VIET)]),
            (p2, vec![tag("comma", "a, b")]),
            (
                p3,
                vec![
                    tag("comma", "a, b"),
                    tag("quote", "say \"hi\" 'there' \\ ]")
                ]
            ),
        ]
    )?;
    let post = ok("reader_post p3", db.reader_post("vi", "p3").await)?;
    eq!(
        "reader_post tags",
        post.map(|p| p.tags),
        Some(vec![
            tag("comma", "a, b"),
            tag("quote", "say \"hi\" 'there' \\ ]")
        ])
    )?;

    let list = ok("reader_tags", db.reader_tags().await)?;
    eq!(
        "reader_tags: published posts only, by name",
        list.iter()
            .map(|t| (t.id, t.name.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (viet, VIET),
            (comma, "a, b"),
            (quote, "say \"hi\" 'there' \\ ]"),
        ]
    )
}

/// Year bounds at exactly Jan 1 00:00:00.000 UTC, and the year list.
pub async fn reader_years_at_the_edges<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let (y24, y25) = year_bounds(2024).ok_or("year_bounds(2024)")?;
    let last23 = save(db, at(u, "vi", "last23", y24 - 1)).await?;
    let first24 = save(db, at(u, "vi", "first24", y24)).await?;
    let last24 = save(db, at(u, "vi", "last24", y25 - 1)).await?;
    let first25 = save(db, at(u, "en", "first25", y25)).await?;
    // Year 10000 is outside 1..=9999: no year, in every store (SQLite's strftime says NULL).
    save(
        db,
        at(
            u,
            "vi",
            "far-future",
            year_bounds(9999).ok_or("year_bounds(9999)")?.1,
        ),
    )
    .await?;
    let mut d = post(u, "vi", "draft", PostStatus::Draft);
    d.published_at = None;
    save(db, d).await?;
    let mut sched = post(u, "vi", "sched", PostStatus::Scheduled);
    sched.published_at = Some(year_bounds(2030).ok_or("year_bounds(2030)")?.0);
    save(db, sched).await?;
    let mut pg = at(
        u,
        "vi",
        "page",
        year_bounds(2010).ok_or("year_bounds(2010)")?.0,
    );
    pg.kind = PostKind::Page;
    save(db, pg).await?;

    eq!(
        "Year 2024: both edges inside",
        ids(db, ReaderFilter::Year(2024), 0, 10).await?,
        (2, vec![last24, first24])
    )?;
    eq!(
        "Year 2023",
        ids(db, ReaderFilter::Year(2023), 0, 10).await?,
        (1, vec![last23])
    )?;
    eq!(
        "Year 2025, every language",
        ids(db, ReaderFilter::Year(2025), 0, 10).await?,
        (1, vec![first25])
    )?;
    for y in [0, 2010, 2030, 10_000, i32::MIN] {
        eq!(
            &format!("Year {y}"),
            ids(db, ReaderFilter::Year(y), 0, 10).await?,
            (0, vec![])
        )?;
    }
    eq!(
        "reader_years: published posts only, newest first",
        ok("reader_years", db.reader_years().await)?,
        vec![2025, 2024, 2023]
    )
}

/// Translations list published versions only.
pub async fn reader_translations_published_only<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let vi = save(db, at(u, "vi", "chao", 100)).await?;
    let in_group = |lang: &str, slug: &str, status: PostStatus| {
        let mut p = post(u, lang, slug, status);
        p.translation_group = Some(vi);
        p
    };
    save(db, in_group("fr", "salut", PostStatus::Published)).await?;
    save(db, in_group("en", "hello", PostStatus::Draft)).await?;
    save(db, in_group("de", "hallo", PostStatus::Scheduled)).await?;
    save(db, at(u, "en", "other", 100)).await?;
    let got = ok("reader_translations", db.reader_translations(vi).await)?;
    eq!(
        "published versions by lang",
        slugs(&got),
        vec!["fr/salut".to_owned(), "vi/chao".to_owned()]
    )?;
    eq!(
        "titles",
        got.iter().map(|l| l.title.as_str()).collect::<Vec<_>>(),
        vec!["Title salut", "Title chao"]
    )
}

/// Neighbours follow date (then id) within one language; pinned, drafts and pages do not
/// count.
pub async fn reader_neighbours_by_date<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let first = save(db, at(u, "vi", "first", 100)).await?;
    let mut pin = at(u, "vi", "pin", 150);
    pin.pinned = true;
    save(db, pin).await?;
    let tie1 = save(db, at(u, "vi", "tie1", 200)).await?;
    let tie2 = save(db, at(u, "vi", "tie2", 200)).await?;
    let last = save(db, at(u, "vi", "last", 300)).await?;
    let en = save(db, at(u, "en", "only", 200)).await?;
    let mut d = post(u, "vi", "draft", PostStatus::Draft);
    d.published_at = None;
    let draft = save(db, d).await?;
    let mut pg = at(u, "vi", "page", 250);
    pg.kind = PostKind::Page;
    let page_id = save(db, pg).await?;

    let around = |id: PostId| async move {
        let n = ok("reader_neighbours", db.reader_neighbours(id).await)?;
        Ok::<_, String>((n.older.map(|l| l.slug), n.newer.map(|l| l.slug)))
    };
    let s = |v: &str| Some(v.to_owned());
    eq!("first", around(first).await?, (None, s("pin")))?;
    eq!("tie1", around(tie1).await?, (s("pin"), s("tie2")))?;
    eq!(
        "tie2: same date, lower id is older",
        around(tie2).await?,
        (s("tie1"), s("last"))
    )?;
    eq!(
        "last: the page is skipped",
        around(last).await?,
        (s("tie2"), None)
    )?;
    eq!("only post in its language", around(en).await?, (None, None))?;
    eq!("a draft", around(draft).await?, (None, None))?;
    eq!("a page", around(page_id).await?, (None, None))?;
    eq!("missing post", around(PostId(999_999)).await?, (None, None))
}

/// Feed rows (one language, bodies) and sitemap rows (every published post and page).
pub async fn reader_feed_and_sitemap<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let pa = save(db, at(u, "en", "a", 100)).await?;
    let pb = save(db, at(u, "en", "b", 300)).await?;
    let pc = save(db, at(u, "vi", "c", 200)).await?;
    let mut pg = at(u, "en", "about", 400);
    pg.kind = PostKind::Page;
    let pd = save(db, pg).await?;
    save(db, post(u, "en", "draft", PostStatus::Draft)).await?;
    let mut sched = post(u, "en", "sched", PostStatus::Scheduled);
    sched.published_at = Some(500);
    save(db, sched).await?;
    let gone = save(db, at(u, "en", "gone", 600)).await?;
    ok("trash_post", db.trash_post(gone, 700).await)?;

    let feed = |lang: &'static str, n: u32| async move {
        let rows = ok("reader_feed", db.reader_feed(lang, n).await)?;
        Ok::<_, String>(rows.iter().map(|r| r.post.id).collect::<Vec<_>>())
    };
    eq!(
        "feed en: posts only, newest first",
        feed("en", 10).await?,
        vec![pb, pa]
    )?;
    eq!("feed en limit 1", feed("en", 1).await?, vec![pb])?;
    eq!("feed vi", feed("vi", 10).await?, vec![pc])?;
    eq!(
        "feed with no posts",
        feed("fr", 10).await?,
        Vec::<PostId>::new()
    )?;

    let map = ok("reader_sitemap", db.reader_sitemap(10).await)?;
    eq!(
        "sitemap: posts and pages, newest first",
        map.iter()
            .map(|r| format!("{}/{}", r.lang, r.slug))
            .collect::<Vec<_>>(),
        ["en/about", "en/b", "vi/c", "en/a"]
            .map(str::to_owned)
            .to_vec()
    )?;
    eq!(
        "sitemap rows carry the translation group",
        map.iter().map(|r| r.translation_group).collect::<Vec<_>>(),
        vec![pd, pb, pc, pa]
    )?;
    let two = ok("reader_sitemap limit", db.reader_sitemap(2).await)?;
    eq!("sitemap limit", two.len(), 2)
}
