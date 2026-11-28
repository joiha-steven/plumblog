//! Checks and fixtures shared by the cases. Checks return `Err(String)` instead of
//! panicking, because a panic in wasm aborts the Worker that runs the suite on D1.

use std::fmt::Debug;
use std::future::{Future, poll_fn};
use std::pin::pin;
use std::task::Poll;

use plumblog_model::{ErrorCode, PostId, RequestId, Result, Role, UserId};
use plumblog_syslog::{Event, EventKind};

use crate::{NewMedia, NewUser, PostKind, PostStatus, SavePost, Store};

/// What a case returns.
pub type Check = std::result::Result<(), String>;
/// A value or the reason the case failed.
pub type Got<T> = std::result::Result<T, String>;

/// Fixed time for rows whose time does not matter to the case.
pub const T0: i64 = 1_000;

/// Unwraps a store call that must succeed.
pub fn ok<T>(what: &str, r: Result<T>) -> Got<T> {
    r.map_err(|e| format!("{what}: expected Ok, got error {e}"))
}

/// A store call that must fail with `code`.
pub fn fails<T: Debug>(what: &str, r: Result<T>, code: ErrorCode) -> Check {
    match r {
        Err(e) if e.code == code => Ok(()),
        Err(e) => Err(format!("{what}: expected {}, got error {e}", code.as_str())),
        Ok(v) => Err(format!("{what}: expected {}, got Ok({v:?})", code.as_str())),
    }
}

/// `eq!(what, got, want)`: compares by reference so callers can pass temporaries.
macro_rules! eq {
    ($what:expr, $got:expr, $want:expr $(,)?) => {
        $crate::contract::kit::check_eq($what, &$got, &$want)
    };
}
pub(crate) use eq;

pub fn check_eq<T: PartialEq + Debug + ?Sized>(what: &str, got: &T, want: &T) -> Check {
    if got == want {
        Ok(())
    } else {
        Err(format!("{what}: expected {want:?}, got {got:?}"))
    }
}

pub fn that(what: &str, cond: bool) -> Check {
    if cond {
        Ok(())
    } else {
        Err(format!("{what}: expected true, got false"))
    }
}

pub async fn version<S: Store>(db: &S) -> Got<i64> {
    ok("content_version", db.content_version().await)
}

/// The content version must be `want` after `what`.
pub async fn version_is<S: Store>(db: &S, what: &str, want: i64) -> Check {
    let got = version(db).await?;
    if got == want {
        Ok(())
    } else {
        Err(format!(
            "content version after {what}: expected {want}, got {got}"
        ))
    }
}

pub fn new_user(login: &str) -> NewUser {
    NewUser {
        login: login.to_owned(),
        slug: login.to_lowercase(),
        display_name: login.to_owned(),
        bio: String::new(),
        role: Role::Writer,
        password_hash: None,
        created_at: T0,
    }
}

pub async fn user<S: Store>(db: &S, login: &str) -> Got<UserId> {
    ok(
        &format!("create_user {login}"),
        db.create_user(new_user(login)).await,
    )
}

/// A post of `kind` post; scheduled and published ones get `published_at = T0`.
pub fn post(author: UserId, lang: &str, slug: &str, status: PostStatus) -> SavePost {
    let live = matches!(status, PostStatus::Scheduled | PostStatus::Published);
    SavePost {
        id: None,
        expected_version: None,
        translation_group: None,
        kind: PostKind::Post,
        lang: lang.to_owned(),
        slug: slug.to_owned(),
        status,
        published_at: live.then_some(T0),
        author_id: author,
        title: format!("Title {slug}"),
        markdown: format!("# {slug}"),
        html: format!("<h1>{slug}</h1>"),
        toc_json: "[]".to_owned(),
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
        search_text: String::new(),
        revision: None,
        client_id: None,
        now: T0,
    }
}

pub async fn save<S: Store>(db: &S, p: SavePost) -> Got<PostId> {
    let what = format!("save_post /{}/{}", p.lang, p.slug);
    ok(&what, db.save_post(p).await)
}

/// The same post as `p` with `id` set, for an update from the post's current version
/// (version 1 for a post that does not exist, so the save reaches `PostNotFound`).
pub async fn update<S: Store>(db: &S, id: PostId, mut p: SavePost) -> Got<SavePost> {
    let current = match db.get_post(id).await {
        Ok(got) => got.version,
        Err(e) if e.code == ErrorCode::PostNotFound => 1,
        Err(e) => return Err(format!("get_post {} for update: {e}", id.0)),
    };
    p.id = Some(id);
    p.expected_version = Some(current);
    Ok(p)
}

pub fn media(key: &str, created_at: i64) -> NewMedia {
    NewMedia {
        r2_key: key.to_owned(),
        mime: "image/png".to_owned(),
        bytes: 10,
        width: 4,
        height: 3,
        alt: String::new(),
        variants_json: "{}".to_owned(),
        uploaded_by: None,
        created_at,
    }
}

pub fn event(at_ms: u64, kind: EventKind, actor: Option<UserId>) -> Got<Event> {
    let request_id = RequestId::parse("contract-1").ok_or("request id rejected")?;
    Ok(Event::new(kind, request_id, at_ms)
        .actor(actor)
        .subject(&format!("at {at_ms}")))
}

/// `ids` sorted descending, the order of every "newest first" list with equal keys.
pub fn desc<T: Ord + Copy>(ids: &[T]) -> Vec<T> {
    let mut v = ids.to_vec();
    v.sort_unstable_by(|a, b| b.cmp(a));
    v
}

pub fn asc<T: Ord + Copy>(ids: &[T]) -> Vec<T> {
    let mut v = ids.to_vec();
    v.sort_unstable();
    v
}

/// Runs `a` and `b` at once (both polled until both finish), so two store calls can race; a
/// store without real concurrency (memory) simply runs them one after the other.
pub async fn both<A: Future, B: Future>(a: A, b: B) -> (A::Output, B::Output) {
    let (mut a, mut b) = (pin!(a), pin!(b));
    let (mut ra, mut rb) = (None, None);
    poll_fn(|cx| {
        if ra.is_none()
            && let Poll::Ready(v) = a.as_mut().poll(cx)
        {
            ra = Some(v);
        }
        if rb.is_none()
            && let Poll::Ready(v) = b.as_mut().poll(cx)
        {
            rb = Some(v);
        }
        if ra.is_some()
            && rb.is_some()
            && let (Some(x), Some(y)) = (ra.take(), rb.take())
        {
            return Poll::Ready((x, y));
        }
        Poll::Pending
    })
    .await
}
