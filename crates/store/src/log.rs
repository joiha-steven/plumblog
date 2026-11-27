//! The owner's timeline (`system_log`).

use plumblog_model::{Result, UserId};
use plumblog_syslog::{Event, EventKind};

/// A stored event with its row id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LogEntry {
    pub id: i64,
    pub event: Event,
}

/// One page of the timeline, newest first (`at_ms` DESC, id DESC). Every filter set must hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LogQuery {
    pub kind: Option<EventKind>,
    pub actor: Option<UserId>,
    /// Inclusive.
    pub from_ms: Option<u64>,
    /// Exclusive.
    pub to_ms: Option<u64>,
    /// Keyset cursor: `(at_ms, id)` of the last entry of the previous page; the page starts
    /// strictly after it. A pair, not an id alone, because ids need not follow `at_ms`.
    pub before: Option<(u64, i64)>,
    pub limit: u32,
}

// Workers are single-threaded, so futures need not be `Send` (see plumblog_auth::Crypto).
#[allow(async_fn_in_trait)]
pub trait LogStore {
    /// Returns the row id. `InvalidInput` if the actor does not exist.
    async fn append(&self, event: Event) -> Result<i64>;
    /// A stored code this version does not know is `Storage`.
    async fn query_log(&self, query: LogQuery) -> Result<Vec<LogEntry>>;
    /// Deletes entries with `at_ms < older_than_ms`, then all but the newest `keep_max`;
    /// returns how many were deleted in total.
    async fn prune_log(&self, older_than_ms: u64, keep_max: u32) -> Result<u64>;
}
