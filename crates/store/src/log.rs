//! The owner's timeline (`system_log`).

use plumblog_model::{Result, UserId};
use plumblog_syslog::{Event, EventKind, LogSink, Retention, Scope};

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
    /// Deletes at most `limit` entries, oldest first (`at_ms`, id), among those with
    /// `at_ms < older_than_ms` or outside the newest `keep_max`; returns how many went.
    async fn prune_log(&self, older_than_ms: u64, keep_max: u32, limit: u32) -> Result<u64>;
}

/// Subject of the retention job's own `job.run` event, so the timeline shows when it ran.
pub const RETENTION_JOB: &str = "system_log.retention";

/// The daily retention job (docs/ARCHITECTURE.md, System log): deletes up to
/// `retention.per_run` rows past the age or row cap, then records one `job.run` event with
/// how many went, or the error code if pruning failed. The event is written after the
/// prune, so the log may hold `max_rows + 1` rows until the next run. Recording goes
/// through [`plumblog_syslog::record`]: its failure reaches `report`, never the result.
///
/// # Errors
/// The prune's error.
pub async fn retention_job<S: LogStore + LogSink>(
    store: &S,
    scope: &Scope,
    retention: Retention,
    now_ms: u64,
    report: impl FnOnce(&str),
) -> Result<u64> {
    let pruned = store
        .prune_log(
            retention.age_cut_ms(now_ms),
            retention.max_rows,
            retention.per_run,
        )
        .await;
    let event = scope
        .event(EventKind::ScheduledJob, now_ms)
        .subject(RETENTION_JOB);
    let event = match &pruned {
        Ok(n) => event.field("deleted", &n.to_string()),
        Err(e) => event.error(e.code),
    };
    plumblog_syslog::record(store, event, report).await;
    pruned
}
