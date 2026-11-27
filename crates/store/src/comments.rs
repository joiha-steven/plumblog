//! Reader comments (`comments`): storage, and the rules every caller shares (limits,
//! when a post is open, who may moderate), so the Worker route and a future admin or API
//! cannot drift apart (docs/ARCHITECTURE.md, Reader site).

use crate::{Post, PostStatus, SettingsStore};
use plumblog_markdown::render_comment;
use plumblog_model::{
    CommentId, Error, ErrorCode, Permission, PostId, Result, Role, UserId, stable_codes,
};
use plumblog_syslog::{EventKind, LogSink, Scope, record};

stable_codes! {
    /// Stored in `comments.status`.
    pub enum CommentStatus {
        Pending => "pending",
        Approved => "approved",
        Spam => "spam",
        Trashed => "trashed",
    }
}

/// A row of `comments`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comment {
    pub id: CommentId,
    pub post_id: PostId,
    pub parent_id: Option<CommentId>,
    pub author_name: String,
    pub body: String,
    /// `render_comment(body)` as of the submit; `None` for rows from before
    /// `0004_comment_html.sql` (render them when the page is built).
    pub body_html: Option<String>,
    pub status: CommentStatus,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewComment {
    pub post_id: PostId,
    /// Must be a comment of the same post.
    pub parent_id: Option<CommentId>,
    pub author_name: String,
    pub body: String,
    pub body_html: Option<String>,
    pub status: CommentStatus,
    pub created_at: i64,
}

// Workers are single-threaded, so futures need not be `Send` (see plumblog_auth::Crypto).
#[allow(async_fn_in_trait)]
pub trait CommentStore {
    /// `InvalidInput` if the post is missing, or the parent is missing or on another post.
    /// Bumps the content version if the status is approved.
    async fn add_comment(&self, comment: NewComment) -> Result<CommentId>;
    /// `NotFound`.
    async fn get_comment(&self, id: CommentId) -> Result<Comment>;
    /// The newest `limit` (capped at [`crate::MAX_PAGE`]) comments of a post with `status`,
    /// returned oldest first (`created_at`, id), and whether older ones were left out.
    async fn latest_comments(
        &self,
        post: PostId,
        status: CommentStatus,
        limit: u32,
    ) -> Result<(Vec<Comment>, bool)>;
    /// Oldest first (`created_at`, id).
    async fn list_comments(&self, post: PostId, status: CommentStatus) -> Result<Vec<Comment>>;
    /// Moderation queue, oldest first (`created_at`, id). `after` is the `(created_at, id)`
    /// of the last comment of the previous page; the page starts strictly after it.
    async fn list_comments_by_status(
        &self,
        status: CommentStatus,
        after: Option<(i64, CommentId)>,
        limit: u32,
    ) -> Result<Vec<Comment>>;
    /// `NotFound`. Bumps if the old or new status is approved.
    async fn set_comment_status(&self, id: CommentId, status: CommentStatus) -> Result<()>;
    /// `NotFound`. Replies (at any depth) go too; bumps if any deleted comment was approved.
    async fn delete_comment(&self, id: CommentId) -> Result<()>;
}

/// Longest display name, in characters after trimming.
pub const NAME_MAX: usize = 60;
/// Longest comment, in characters after trimming.
pub const BODY_MAX: usize = 5000;
/// Most links in one comment, allowed or not: more is link spam.
pub const LINKS_MAX: usize = 30;
/// Site setting (JSON `true`/`false`) that turns comments on; absent means off.
pub const SETTING_ENABLED: &str = "comments.enabled";

/// A comment that passed the limits: what gets stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Checked {
    pub name: String,
    /// The reader's Markdown, line breaks normalised and trimmed. [`submit`] renders it once
    /// into `body_html`.
    pub body: String,
}

/// Invisible characters that could spoof or blank a display name: bidi controls, zero-width
/// characters, soft hyphen, BOM. The body keeps them (the theme isolates its direction).
fn invisible(c: char) -> bool {
    c.is_control()
        || matches!(c, '\u{ad}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{2069}' | '\u{feff}')
}

/// Checks the reader's form fields.
///
/// # Errors
/// `comments.name_invalid` (1-60 visible characters), `comments.body_invalid` (1-5000
/// characters) or `comments.too_many_links` (more than 30).
pub fn check(name: &str, body: &str) -> Result<Checked> {
    let name = name
        .chars()
        .filter(|c| !invisible(*c))
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if !(1..=NAME_MAX).contains(&name.chars().count()) {
        return Err(Error::new(ErrorCode::CommentName, "name length"));
    }
    // Browsers send textarea line breaks as CRLF; count and store one character per break.
    let body = body.replace("\r\n", "\n").replace('\r', "\n");
    let body = body.trim();
    if !(1..=BODY_MAX).contains(&body.chars().count()) {
        return Err(Error::new(ErrorCode::CommentBody, "body length"));
    }
    let links = render_comment(body).links;
    if links > LINKS_MAX {
        return Err(Error::new(
            ErrorCode::CommentLinks,
            format!("{links} links"),
        ));
    }
    Ok(Checked {
        name,
        body: body.to_owned(),
    })
}

/// Whether readers may comment on `post` now: the site switch is on, the post is published
/// and its own switch is on. No auto-close.
#[must_use]
pub fn is_open(site_enabled: bool, post: &Post) -> bool {
    site_enabled && post.status == PostStatus::Published && post.comments_enabled
}

/// The site switch. Absent or anything but JSON `true` is off (the default).
///
/// # Errors
/// The store read.
pub async fn site_enabled<S: SettingsStore>(store: &S) -> Result<bool> {
    Ok(store.get_setting(SETTING_ENABLED).await?.as_deref() == Some("true"))
}

/// Turns comments on or off for the whole site: a setting, so owner only.
///
/// # Errors
/// `auth.unauthenticated` without a role, `auth.forbidden` for a writer, or the store write.
pub async fn set_site_enabled<S: SettingsStore + LogSink>(
    store: &S,
    scope: &Scope,
    role: Option<Role>,
    on: bool,
    now_ms: u64,
    report: impl FnOnce(&str),
) -> Result<()> {
    allowed(role, Permission::ManageSettings)?;
    let now = i64::try_from(now_ms).unwrap_or(i64::MAX);
    store
        .put_setting(SETTING_ENABLED, if on { "true" } else { "false" }, now)
        .await?;
    let event = scope
        .event(EventKind::SettingsChanged, now_ms)
        .subject(SETTING_ENABLED)
        .field("value", if on { "true" } else { "false" });
    record(store, event, report).await;
    Ok(())
}

/// Stores a checked comment as pending and records one `comment.created` event with ids
/// only. The caller has already verified the reader (Turnstile) and the rate limit.
///
/// # Errors
/// The store write (`input.invalid` if the post vanished meanwhile).
pub async fn submit<S: CommentStore + LogSink>(
    store: &S,
    scope: &Scope,
    post: PostId,
    comment: Checked,
    now_ms: u64,
    report: impl FnOnce(&str),
) -> Result<CommentId> {
    let id = store
        .add_comment(NewComment {
            post_id: post,
            // Flat list in v1: readers cannot reply.
            parent_id: None,
            author_name: comment.name,
            // Rendered once here, so page builds never parse Markdown and a later renderer
            // change does not rewrite old comments (docs/ARCHITECTURE.md, Decision log).
            body_html: Some(render_comment(&comment.body).html),
            body: comment.body,
            status: CommentStatus::Pending,
            created_at: i64::try_from(now_ms).unwrap_or(i64::MAX),
        })
        .await?;
    let event = scope
        .event(EventKind::CommentCreated, now_ms)
        .field("post", &post.0.to_string())
        .field("comment", &id.0.to_string());
    record(store, event, report).await;
    Ok(id)
}

stable_codes! {
    /// What a moderator does to a comment; the code is the `status` field of the
    /// `comment.moderated` event.
    pub enum Moderation {
        Approve => "approved",
        Spam => "spam",
        Trash => "trashed",
        Delete => "deleted",
    }
}

/// Who acts, as the session or token resolved them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Actor {
    pub id: UserId,
    pub role: Role,
}

fn allowed(role: Option<Role>, permission: Permission) -> Result<()> {
    match role {
        None => Err(Error::new(ErrorCode::Unauthenticated, permission.as_str())),
        Some(r) if r.can(permission) => Ok(()),
        Some(r) => Err(Error::new(
            ErrorCode::Forbidden,
            format!("{} lacks {}", r.as_str(), permission.as_str()),
        )),
    }
}

/// Approves, spams, trashes or deletes a comment, then records one `comment.moderated`
/// event (comment id, post id, new status; the actor is the scope's user). Never the
/// comment's name or text. The store bumps the content version when readers see a change.
///
/// # Errors
/// `auth.unauthenticated` without an actor, `auth.forbidden` without `comment.moderate`,
/// `store.not_found` for an unknown comment, or the store write.
pub async fn moderate<S: CommentStore + LogSink>(
    store: &S,
    scope: &Scope,
    actor: Option<Actor>,
    id: CommentId,
    action: Moderation,
    now_ms: u64,
    report: impl FnOnce(&str),
) -> Result<()> {
    allowed(actor.map(|a| a.role), Permission::ModerateComments)?;
    let comment = store.get_comment(id).await?;
    let status = match action {
        Moderation::Approve => Some(CommentStatus::Approved),
        Moderation::Spam => Some(CommentStatus::Spam),
        Moderation::Trash => Some(CommentStatus::Trashed),
        Moderation::Delete => None,
    };
    match status {
        Some(status) => store.set_comment_status(id, status).await?,
        None => store.delete_comment(id).await?,
    }
    log_moderation(store, scope, actor, &comment, action, now_ms, report).await;
    Ok(())
}

async fn log_moderation<S: LogSink>(
    store: &S,
    scope: &Scope,
    actor: Option<Actor>,
    comment: &Comment,
    action: Moderation,
    now_ms: u64,
    report: impl FnOnce(&str),
) {
    let event = scope
        .event(EventKind::CommentModerated, now_ms)
        .actor(actor.map(|a| a.id))
        .field("comment", &comment.id.0.to_string())
        .field("post", &comment.post_id.0.to_string())
        .field("status", action.as_str());
    record(store, event, report).await;
}

#[cfg(test)]
mod tests {
    use super::{Actor, BODY_MAX, LINKS_MAX, Moderation, NAME_MAX, check, moderate};
    use super::{set_site_enabled, site_enabled};
    use crate::memory::MemoryStore;
    use plumblog_model::{CommentId, ErrorCode};
    use plumblog_model::{RequestId, Role, UserId};
    use plumblog_syslog::Scope;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    fn run<F: Future>(f: F) -> F::Output {
        match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("memory store future was pending"),
        }
    }

    fn code<T: std::fmt::Debug>(r: plumblog_model::Result<T>) -> ErrorCode {
        r.expect_err("must be refused").code
    }

    #[test]
    fn limits() {
        let ok = check("  Ann  ", " hi ").unwrap();
        assert_eq!((ok.name.as_str(), ok.body.as_str()), ("Ann", "hi"));
        assert_eq!(
            check(&"n".repeat(NAME_MAX), "b").unwrap().name.len(),
            NAME_MAX
        );
        assert_eq!(
            code(check(&"n".repeat(NAME_MAX + 1), "b")),
            ErrorCode::CommentName
        );
        assert_eq!(code(check("   ", "b")), ErrorCode::CommentName);
        // Invisible characters alone are no name, and never reach a stored name.
        assert_eq!(
            code(check("\u{200b}\u{202e}\u{2066}", "b")),
            ErrorCode::CommentName
        );
        assert_eq!(check("A\u{202e}nn\u{200b}", "b").unwrap().name, "Ann");
        assert_eq!(code(check("Ann", " \n ")), ErrorCode::CommentBody);
        assert!(check("Ann", &"\u{1ea1}".repeat(BODY_MAX)).is_ok());
        assert_eq!(
            code(check("Ann", &"b".repeat(BODY_MAX + 1))),
            ErrorCode::CommentBody
        );
        let links = |n: usize| "https://a.example/ ".repeat(n);
        assert!(check("Ann", &links(LINKS_MAX)).is_ok());
        assert_eq!(
            code(check("Ann", &links(LINKS_MAX + 1))),
            ErrorCode::CommentLinks
        );
        // Refused links count too: they are spam all the same.
        let bad = "[x](javascript:a) ".repeat(LINKS_MAX + 1);
        assert_eq!(code(check("Ann", &bad)), ErrorCode::CommentLinks);
        // CRLF and lone CR count as one break and are stored as LF.
        let crlf = format!("{}\r\n{}\rx", "a".repeat(BODY_MAX - 4), "b");
        assert_eq!(
            crlf.chars().count(),
            BODY_MAX + 1,
            "over the limit before normalising"
        );
        let stored = check("Ann", &crlf).unwrap().body;
        assert!(!stored.contains('\r'));
        assert_eq!(stored.chars().count(), BODY_MAX);
        // The stored body is the reader's Markdown, not HTML.
        assert_eq!(
            check("Ann", "<script>x</script>").unwrap().body,
            "<script>x</script>"
        );
    }

    #[test]
    fn moderation_needs_the_permission() {
        let db = MemoryStore::default();
        let scope = Scope::new(RequestId::unknown(), None);
        let actor = |role| {
            Some(Actor {
                id: UserId(1),
                role,
            })
        };
        // Permission is checked before the store is read: unknown ids still say who may not.
        let none = run(moderate(
            &db,
            &scope,
            None,
            CommentId(1),
            Moderation::Approve,
            1,
            |_| {},
        ));
        assert_eq!(code(none), ErrorCode::Unauthenticated);
        for role in [Role::Owner, Role::Writer] {
            let r = run(moderate(
                &db,
                &scope,
                actor(role),
                CommentId(1),
                Moderation::Spam,
                1,
                |_| {},
            ));
            assert_eq!(
                code(r),
                ErrorCode::NotFound,
                "{role:?} passes the permission check"
            );
        }
    }

    #[test]
    fn site_switch_is_owner_only_and_off_by_default() {
        let db = MemoryStore::default();
        let scope = Scope::new(RequestId::unknown(), None);
        assert!(!run(site_enabled(&db)).unwrap());
        let set = |role, on| run(set_site_enabled(&db, &scope, role, on, 1, |_| {}));
        assert_eq!(code(set(Some(Role::Writer), true)), ErrorCode::Forbidden);
        assert_eq!(code(set(None, true)), ErrorCode::Unauthenticated);
        assert!(!run(site_enabled(&db)).unwrap());
        set(Some(Role::Owner), true).unwrap();
        assert!(run(site_enabled(&db)).unwrap());
        set(Some(Role::Owner), false).unwrap();
        assert!(!run(site_enabled(&db)).unwrap());
    }
}
