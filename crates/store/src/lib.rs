//! Storage behind traits. The Worker implements them over D1/R2; `memory` implements them
//! in plain Rust so logic is tested without Cloudflare. The files in `migrations/` are the
//! schema both follow; their constraints and ON DELETE rules are part of the contract.
//!
//! Shared rules for every trait:
//! - Times are `i64` ms since the Unix epoch (UTC). The store never reads a clock.
//! - Every write is all-or-nothing: on error nothing changed.
//! - Read paths never write.
//! - `site.content_version` is bumped inside the same write whenever the write can change
//!   what a reader sees (rules on each method).
//! - Page sizes (`limit`) are capped at [`MAX_PAGE`], one more for post and media lists (the
//!   probe row, see [`probe_len`]); a limit of 0 returns nothing.

mod auth;
mod comments;
#[cfg(any(test, feature = "contract"))]
pub mod contract;
mod log;
mod media;
pub mod memory;
mod posts;
mod reader;
mod redirects;
pub mod search;
mod settings;
mod signin;
mod taxonomy;
mod users;
mod webhooks;

pub use auth::{
    ApiToken, Invite, InviteStore, NewInvite, NewPasskey, NewSession, NewToken, Passkey,
    PasskeyStore, Proof, Session, SessionStore, TokenStore,
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
    NewRevision, Post, PostCounts, PostCursor, PostKind, PostOrder, PostQuery, PostStatus,
    PostStore, PostSummary, Revision, SavePost, Saved, valid_client_id,
};
pub use reader::{
    READER_MAX_OFFSET, READER_SITEMAP_MAX, ReaderFilter, ReaderLink, ReaderNeighbours, ReaderPage,
    ReaderPost, ReaderRow, ReaderSitemapRow, ReaderStore, ReaderTag, year_bounds, year_of,
};
pub use redirects::{Redirect, RedirectStore};
pub use settings::{OptionOwner, PluginState, SettingsStore};
pub use signin::{
    ChallengePurpose, ChallengeStore, INTERNAL_SETTING_KEYS, LAST_BACKUP_KEY, LoginLimitStore,
    MAX_FAILURE_KEYS, ORIGIN_KEY, OWNER_RESET_AT_KEY, OWNER_RESET_EXPIRES_KEY,
    OWNER_RESET_HASH_KEY, OWNER_RESET_KEEP_TOKENS_KEY, OWNER_RESET_KEYS,
    OWNER_RESET_SINCE_MARGIN_MS, OWNER_RESET_USER_KEY, Recorded, SETUP_CODE_EXPIRES_KEY,
    SETUP_CODE_HASH_KEY, SITE_ID_KEY, SetupStore, TURNSTILE_SITE_KEY, is_internal_setting,
    public_settings, setup_hash_json,
};
pub use taxonomy::{NewSeries, Series, Tag, TaxonomyStore};
pub use users::{NewUser, ProfileUpdate, SiteStore, User, UserStore};
pub use webhooks::{
    AttemptResult, Delivery, DeliveryStore, NewDelivery, NewWebhook, Webhook, WebhookStore,
};

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
    + LoginLimitStore
    + ChallengeStore
    + SetupStore
    + MediaStore
    + TaxonomyStore
    + PostStore
    + ReaderStore
    + RedirectStore
    + CommentStore
    + SettingsStore
    + WebhookStore
    + DeliveryStore
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
        + LoginLimitStore
        + ChallengeStore
        + SetupStore
        + MediaStore
        + TaxonomyStore
        + PostStore
        + ReaderStore
        + RedirectStore
        + CommentStore
        + SettingsStore
        + WebhookStore
        + DeliveryStore
        + LogStore
        + plumblog_syslog::LogSink
        + ?Sized
{
}

/// `limit` as a slice length for a list that is paged by a probe row: capped at
/// [`MAX_PAGE`] + 1. A client page of `MAX_PAGE` asks for one more row to learn whether
/// another page exists; clamping that request to `MAX_PAGE` would hide the row, so
/// `next_cursor` would never be set. Still bounded: one row over a page, never a table.
#[must_use]
pub fn probe_len(limit: u32) -> usize {
    usize::try_from(limit.min(MAX_PAGE + 1)).unwrap_or(0)
}

/// `limit` as a slice length, capped at [`MAX_PAGE`].
#[must_use]
pub fn page_len(limit: u32) -> usize {
    // u32 -> usize never truncates on the 32/64-bit targets we build for.
    usize::try_from(limit.min(MAX_PAGE)).unwrap_or(0)
}
