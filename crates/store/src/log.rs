//! The owner's timeline (`system_log`).

use plumblog_model::{Result, UserId};
use plumblog_syslog::{Event, EventKind, Retention, Scope};

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

/// Subject of the retention job's own `job.run` event, so the timeline shows when it ran.
pub const RETENTION_JOB: &str = "system_log.retention";

/// The daily retention job (docs/ARCHITECTURE.md, System log): prunes by age, then by row
/// count, then records one `job.run` event with how many rows went, or the error code if
/// pruning failed. The event is written after the prune, so the log may hold
/// `max_rows + 1` rows until the next run. Returns the prune result; a failure to record
/// the event is returned only if the prune itself succeeded.
///
/// # Errors
/// The prune's error, else the append's.
pub async fn retention_job<S: LogStore>(
    store: &S,
    scope: &Scope,
    retention: Retention,
    now_ms: u64,
) -> Result<u64> {
    let keep = u32::try_from(retention.max_rows).unwrap_or(u32::MAX);
    let pruned = store.prune_log(retention.age_cut_ms(now_ms), keep).await;
    let event = scope
        .event(EventKind::ScheduledJob, now_ms)
        .subject(RETENTION_JOB);
    let event = match &pruned {
        Ok(n) => event.field("deleted", &n.to_string()),
        Err(e) => event.error(e.code),
    };
    let recorded = store.append(event).await;
    let deleted = pruned?;
    recorded.map(|_| deleted)
}
