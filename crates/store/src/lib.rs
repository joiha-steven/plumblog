//! Storage behind traits. The Worker implements them over D1/R2; `memory` implements them
//! in plain Rust so logic is tested without Cloudflare. `migrations/0001_init.sql` is the
//! schema both follow; its constraints and ON DELETE rules are part of the contract.
//!
//! Shared rules for every trait:
//! - Times are `i64` ms since the Unix epoch (UTC). The store never reads a clock.
//! - Every write is all-or-nothing: on error nothing changed.
//! - Read paths never write.
//! - `site.content_version` is bumped inside the same write whenever the write can change
//!   what a reader sees (rules on each method).
//! - Page sizes (`limit`) are capped at [`MAX_PAGE`]; a limit of 0 returns nothing.

mod auth;
mod comments;
#[cfg(any(test, feature = "contract"))]
pub mod contract;
mod log;
mod media;
pub mod memory;
mod posts;
mod redirects;
pub mod search;
mod settings;
mod taxonomy;
mod users;
mod webhooks;

pub use auth::{
    ApiToken, Invite, InviteStore, NewInvite, NewPasskey, NewSession, NewToken, Passkey,
    PasskeyStore, Session, SessionStore, TokenStore,
};
pub use comments::{Actor, Checked, Comment, CommentStatus, CommentStore, Moderation, NewComment};
/// Comment rules shared by every caller (limits, open, moderation).
pub mod comment {
    pub use crate::comments::{
        BODY_MAX, LINKS_MAX, NAME_MAX, SETTING_ENABLED, check, is_open, moderate, set_site_enabled,
        site_enabled, submit,
    };
}
pub use log::{LogEntry, LogQuery, LogStore, RETENTION_JOB, retention_job};
pub use media::{Media, MediaQuery, MediaStore, NewMedia};
pub use posts::{
    NewRevision, Post, PostCursor, PostKind, PostOrder, PostQuery, PostStatus, PostStore,
    PostSummary, Revision, SavePost,
};
pub use redirects::{Redirect, RedirectStore};
pub use settings::{OptionOwner, PluginState, SettingsStore};
pub use taxonomy::{NewSeries, Series, Tag, TaxonomyStore};
pub use users::{NewUser, ProfileUpdate, SiteStore, User, UserStore};
pub use webhooks::{NewWebhook, Webhook, WebhookStore};

/// Largest page any list query returns, so one request cannot read a whole table.
pub const MAX_PAGE: u32 = 100;

/// Everything the Worker needs from storage, as one bound.
pub trait Store:
    SiteStore
    + UserStore
    + SessionStore
    + PasskeyStore
    + TokenStore
    + InviteStore
    + MediaStore
    + TaxonomyStore
    + PostStore
    + RedirectStore
    + CommentStore
    + SettingsStore
    + WebhookStore
    + LogStore
    + plumblog_syslog::LogSink
{
}

impl<T> Store for T where
    T: SiteStore
        + UserStore
        + SessionStore
        + PasskeyStore
        + TokenStore
        + InviteStore
        + MediaStore
        + TaxonomyStore
        + PostStore
        + RedirectStore
        + CommentStore
        + SettingsStore
        + WebhookStore
        + LogStore
        + plumblog_syslog::LogSink
        + ?Sized
{
}

/// `limit` as a slice length, capped at [`MAX_PAGE`].
#[must_use]
pub fn page_len(limit: u32) -> usize {
    // u32 -> usize never truncates on the 32/64-bit targets we build for.
    usize::try_from(limit.min(MAX_PAGE)).unwrap_or(0)
}
