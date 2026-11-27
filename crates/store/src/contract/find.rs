//! Reader search and redirects. Vietnamese is escaped because source must be ASCII:
//! "\u{0110}\u{00E0} L\u{1EA1}t" is "Da Lat" with its marks, "\u{0111}\u{01B0}\u{1EDD}ng"
//! is "duong" (street) with a stroked d, "d\u{01B0}\u{1EDD}ng" the same with a plain d, and
//! "NG\u{01AF}\u{1EDC}I" is "NGUOI" (person) in capitals.

use plumblog_model::{ErrorCode, PostId};

use super::kit::{Check, Got, T0, eq, fails, ok, post, save, update, user};
use crate::{PostStatus, Store};

const TITLE: &str = "\u{0110}\u{00E0} L\u{1EA1}t";
const BODY: &str =
    "Con \u{0111}\u{01B0}\u{1EDD}ng NG\u{01AF}\u{1EDC}I d\u{01B0}\u{1EDD}ng tr\u{01B0}\u{1EDB}c";

async fn hits<S: Store>(db: &S, lang: &str, q: &str) -> Got<Vec<PostId>> {
    Ok(ok(&format!("search {q:?}"), db.search(lang, q, 100).await)?
        .iter()
        .map(|p| p.id)
        .collect())
}

pub async fn search_vietnamese<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let mut p = post(u, "vi", "da-lat", PostStatus::Published);
    p.title = TITLE.into();
    p.search_text = BODY.into();
    let id = save(db, p).await?;
    let one = vec![id];
    for q in [
        "duong",
        "da lat",
        "DA LAT",
        "nguoi",
        "\u{0110}\u{01B0}\u{1EDD}ng",
        "\u{0111}\u{00E0}",
        "NG\u{01AF}\u{1EDC}I",
        "lat duong",
    ] {
        eq!(
            &format!("search {q:?}"),
            hits(db, "vi", q).await?,
            one.clone(),
        )?;
    }
    eq!("prefix", hits(db, "vi", "ngu").await?, one.clone())?;
    eq!(
        "prefix of title word",
        hits(db, "vi", "la").await?,
        one.clone(),
    )?;
    eq!(
        "every word must match",
        hits(db, "vi", "da xyz").await?,
        Vec::new(),
    )?;
    eq!(
        "infix is not a prefix",
        hits(db, "vi", "uong").await?,
        Vec::new(),
    )?;
    eq!("other lang", hits(db, "en", "duong").await?, Vec::new())
}

pub async fn search_scope_and_order<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let mut ids = Vec::new();
    for (slug, at) in [("a", 100), ("b", 300), ("c", 300), ("d", 200)] {
        let mut p = post(u, "vi", slug, PostStatus::Published);
        p.published_at = Some(at);
        p.search_text = "common words".into();
        ids.push((at, save(db, p).await?));
    }
    for (slug, status) in [
        ("draft", PostStatus::Draft),
        ("sched", PostStatus::Scheduled),
    ] {
        let mut p = post(u, "vi", slug, status);
        p.search_text = "common words".into();
        save(db, p).await?;
    }
    let mut trashed = post(u, "vi", "trashed", PostStatus::Published);
    trashed.search_text = "common words".into();
    let t = save(db, trashed).await?;
    ok("trash_post", db.trash_post(t, T0).await)?;
    let mut en = post(u, "en", "en", PostStatus::Published);
    en.search_text = "common words".into();
    save(db, en).await?;

    ids.sort_unstable_by(|a, b| b.cmp(a));
    let want: Vec<PostId> = ids.iter().map(|x| x.1).collect();
    eq!(
        "only published, only vi, newest first (published_at, id)",
        hits(db, "vi", "common").await?,
        want.clone(),
    )?;
    let first2 = ok("search", db.search("vi", "common", 2).await)?;
    eq!(
        "limit",
        first2.iter().map(|p| p.id).collect::<Vec<_>>(),
        want[..2].to_vec(),
    )?;
    eq!(
        "limit 0",
        ok("search", db.search("vi", "common", 0).await)?.len(),
        0,
    )?;
    // The index follows saves: old words go, new words come.
    let (_, first) = ids[0];
    let got = ok("get_post", db.get_post(first).await)?;
    let mut p = post(u, "vi", &got.slug, PostStatus::Published);
    p.published_at = got.published_at;
    p.search_text = "fresh".into();
    save(db, update(first, p)).await?;
    eq!(
        "new text found",
        hits(db, "vi", "fresh").await?,
        vec![first]
    )?;
    eq!(
        "old text gone",
        hits(db, "vi", "common").await?,
        want[1..].to_vec(),
    )
}

/// FTS syntax and punctuation are plain text: never an error, never a different meaning.
pub async fn search_hostile_input<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let mut p = post(u, "vi", "x", PostStatus::Published);
    p.title = "Near or far".into();
    p.search_text = "x marks the spot".into();
    let id = save(db, p).await?;
    for q in ["", "   ", "\"", "*", "!!!", "-", "\"\"*", "()", "^", ":"] {
        eq!(
            &format!("search {q:?}"),
            hits(db, "vi", q).await?,
            Vec::new(),
        )?;
    }
    for q in [
        "-x",
        "x*",
        "\"x",
        "NEAR(",
        "near(x",
        "OR",
        "x OR",
        "x AND spot",
        "x:spot",
    ] {
        hits(db, "vi", q).await?;
    }
    eq!("-x is the word x", hits(db, "vi", "-x").await?, vec![id])?;
    eq!(
        "OR is a word, not an operator",
        hits(db, "vi", "or").await?,
        vec![id],
    )?;
    eq!(
        "x OR nothing is still AND",
        hits(db, "vi", "x OR zzz").await?,
        Vec::new(),
    )?;
    eq!(
        "NEAR( is the word near",
        hits(db, "vi", "NEAR(").await?,
        vec![id],
    )
}

pub async fn redirects_round_trip<S: Store>(db: &S) -> Check {
    ok("put_redirect", db.put_redirect("/b", "/x", 10).await)?;
    ok("put_redirect", db.put_redirect("/a", "/x", 11).await)?;
    eq!(
        "find_redirect",
        ok("find_redirect", db.find_redirect("/a").await)?,
        Some("/x".into()),
    )?;
    eq!(
        "find_redirect unknown",
        ok("find_redirect", db.find_redirect("/zz").await)?,
        None,
    )?;
    ok("upsert", db.put_redirect("/a", "/y", 12).await)?;
    let list = ok("list_redirects", db.list_redirects().await)?;
    eq!(
        "list by from_path, upserted",
        list.iter()
            .map(|r| (r.from_path.as_str(), r.to_path.as_str(), r.created_at))
            .collect::<Vec<_>>(),
        vec![("/a", "/y", 12), ("/b", "/x", 10)],
    )?;
    fails(
        "from == to",
        db.put_redirect("/q", "/q", 13).await,
        ErrorCode::InvalidInput,
    )?;
    ok("delete_redirect", db.delete_redirect("/a").await)?;
    eq!(
        "deleted",
        ok("find_redirect", db.find_redirect("/a").await)?,
        None,
    )?;
    ok("delete_redirect absent", db.delete_redirect("/a").await)?;
    eq!(
        "one left",
        ok("list_redirects", db.list_redirects().await)?.len(),
        1,
    )
}

/// No chains (`a -> b -> c` becomes `a -> c`) and no loops (`b -> a` drops `a -> b`).
pub async fn redirect_chains_and_loops<S: Store>(db: &S) -> Check {
    ok("put a->b", db.put_redirect("/a", "/b", 1).await)?;
    ok("put b->c", db.put_redirect("/b", "/c", 2).await)?;
    let pairs = |v: Vec<crate::Redirect>| {
        v.into_iter()
            .map(|r| (r.from_path, r.to_path))
            .collect::<Vec<_>>()
    };
    let p = |a: &str, b: &str| (a.to_owned(), b.to_owned());
    eq!(
        "chain collapsed",
        pairs(ok("list_redirects", db.list_redirects().await)?),
        vec![p("/a", "/c"), p("/b", "/c")],
    )?;
    ok("put c->a", db.put_redirect("/c", "/a", 3).await)?;
    eq!(
        "loop removed",
        pairs(ok("list_redirects", db.list_redirects().await)?),
        vec![p("/b", "/a"), p("/c", "/a")],
    )
}

/// Renaming a published post redirects the old path; renaming a draft does not.
pub async fn post_rename_redirects<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let p = post(u, "vi", "one", PostStatus::Published);
    let id = save(db, p.clone()).await?;
    let mut two = update(id, p.clone());
    two.slug = "two".into();
    save(db, two.clone()).await?;
    eq!(
        "old path redirects",
        ok("find_redirect", db.find_redirect("/vi/one").await)?,
        Some("/vi/two".into()),
    )?;
    let mut three = two.clone();
    three.slug = "three".into();
    save(db, three.clone()).await?;
    eq!(
        "chain collapsed on second rename",
        (
            ok("find_redirect", db.find_redirect("/vi/one").await)?,
            ok("find_redirect", db.find_redirect("/vi/two").await)?,
        ),
        (Some("/vi/three".into()), Some("/vi/three".into())),
    )?;
    let mut back = three;
    back.slug = "one".into();
    save(db, back).await?;
    eq!(
        "renaming back frees the path",
        ok("find_redirect", db.find_redirect("/vi/one").await)?,
        None,
    )?;
    eq!(
        "others follow",
        ok("find_redirect", db.find_redirect("/vi/three").await)?,
        Some("/vi/one".into()),
    )?;
    let d = post(u, "vi", "draft-a", PostStatus::Draft);
    let did = save(db, d.clone()).await?;
    let mut moved = update(did, d);
    moved.slug = "draft-b".into();
    save(db, moved).await?;
    eq!(
        "renaming a draft adds no redirect",
        ok("find_redirect", db.find_redirect("/vi/draft-a").await)?,
        None,
    )
}

/// A draft or scheduled post on a redirected path keeps the redirect; going live removes it.
pub async fn redirect_kept_until_live<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    ok(
        "put_redirect",
        db.put_redirect("/vi/old", "/vi/elsewhere", 1).await,
    )?;
    let d = post(u, "vi", "old", PostStatus::Draft);
    let id = save(db, d.clone()).await?;
    eq!(
        "draft keeps the redirect",
        ok("find_redirect", db.find_redirect("/vi/old").await)?,
        Some("/vi/elsewhere".into()),
    )?;
    save(db, update(id, post(u, "vi", "old", PostStatus::Published))).await?;
    eq!(
        "published save removes it",
        ok("find_redirect", db.find_redirect("/vi/old").await)?,
        None,
    )?;

    ok(
        "put_redirect",
        db.put_redirect("/vi/later", "/vi/elsewhere", 1).await,
    )?;
    let mut sch = post(u, "vi", "later", PostStatus::Scheduled);
    sch.published_at = Some(5000);
    save(db, sch).await?;
    eq!(
        "scheduled keeps the redirect",
        ok("find_redirect", db.find_redirect("/vi/later").await)?,
        Some("/vi/elsewhere".into()),
    )?;
    ok("publish_due", db.publish_due(4999).await)?;
    eq!(
        "not yet due keeps it",
        ok("find_redirect", db.find_redirect("/vi/later").await)?,
        Some("/vi/elsewhere".into()),
    )?;
    ok("publish_due", db.publish_due(5000).await)?;
    eq!(
        "publish_due removes it",
        ok("find_redirect", db.find_redirect("/vi/later").await)?,
        None,
    )?;
    eq!(
        "no redirect left",
        ok("list_redirects", db.list_redirects().await)?.len(),
        0,
    )
}

/// A post restored to published takes its path back from a redirect; one restored to draft
/// leaves the redirect working.
pub async fn restore_removes_redirect<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let live = save(db, post(u, "vi", "live", PostStatus::Published)).await?;
    let draft = save(db, post(u, "vi", "draft", PostStatus::Draft)).await?;
    ok("trash_post", db.trash_post(live, T0 + 1).await)?;
    ok("trash_post", db.trash_post(draft, T0 + 1).await)?;
    ok(
        "put_redirect",
        db.put_redirect("/vi/live", "/vi/x", T0 + 2).await,
    )?;
    ok(
        "put_redirect",
        db.put_redirect("/vi/draft", "/vi/y", T0 + 2).await,
    )?;
    ok("restore_post", db.restore_post(draft, T0 + 3).await)?;
    eq!(
        "restore to draft keeps the redirect",
        ok("find_redirect", db.find_redirect("/vi/draft").await)?,
        Some("/vi/y".into()),
    )?;
    ok("restore_post", db.restore_post(live, T0 + 3).await)?;
    eq!(
        "restore to published removes it",
        ok("find_redirect", db.find_redirect("/vi/live").await)?,
        None,
    )?;
    eq!(
        "only the draft's redirect is left",
        ok("list_redirects", db.list_redirects().await)?.len(),
        1,
    )
}
