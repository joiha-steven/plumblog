use plumblog_model::{ErrorCode, RequestId, Result, UserId};
use plumblog_store::{LogEntry, LogQuery, LogStore};
use plumblog_syslog::{Event, EventKind, LogSink};
use worker::console_log;
use worker::wasm_bindgen::JsValue;

use super::{D1Store, Row, bound, limit, opt_int, opt_text, storage, text, ubound, uint};

fn read_entry(r: &mut Row) -> Result<LogEntry> {
    let id = r.int()?;
    let at_ms = r.uint()?;
    let raw_request = r.text()?;
    let request_id = RequestId::parse(&raw_request)
        .ok_or_else(|| storage("query_log", &format!("bad request id {raw_request:?}")))?;
    let kind = r.code("event kind", EventKind::parse)?;
    let actor = r.opt_int()?.map(UserId);
    let error = r.opt_code("error", ErrorCode::parse)?;
    let detail = r.text()?;
    Ok(LogEntry {
        id,
        event: Event::restore(at_ms, request_id, kind, actor, error, &detail),
    })
}

impl LogStore for D1Store {
    async fn append(&self, e: Event) -> Result<i64> {
        // A missing actor fails the FK: InvalidInput.
        let s = self.stmt(
            "log_append",
            "INSERT INTO system_log (at_ms, request_id, kind, actor_id, error_code, detail) \
             VALUES (?, ?, ?, ?, ?, ?)",
            &[
                uint(e.at_ms())?,
                text(e.request_id().as_str()),
                text(e.kind().as_str()),
                opt_int(e.actor_id().map(|a| a.0))?,
                opt_text(e.error_code().map(ErrorCode::as_str)),
                text(&e.detail()),
            ],
        )?;
        let r = self.exec(s).await?;
        Self::last_row_id("log_append", &r)
    }

    // 1 query.
    async fn query_log(&self, q: LogQuery) -> Result<Vec<LogEntry>> {
        let (before_at, before_id) = match q.before {
            Some((at, id)) => (ubound(at), bound(id)),
            None => (JsValue::NULL, JsValue::NULL),
        };
        let s = self.stmt(
            "query_log",
            "SELECT id, at_ms, request_id, kind, actor_id, error_code, detail FROM system_log \
             WHERE (?1 IS NULL OR kind = ?1) AND (?2 IS NULL OR actor_id = ?2) \
             AND (?3 IS NULL OR at_ms >= ?3) AND (?4 IS NULL OR at_ms < ?4) \
             AND (?5 IS NULL OR (at_ms, id) < (?5, ?6)) \
             ORDER BY at_ms DESC, id DESC LIMIT ?7",
            &[
                opt_text(q.kind.map(EventKind::as_str)),
                opt_int(q.actor.map(|a| a.0))?,
                q.from_ms.map_or(JsValue::NULL, ubound),
                q.to_ms.map_or(JsValue::NULL, ubound),
                before_at,
                before_id,
                limit(q.limit),
            ],
        )?;
        self.rows(s).await?.iter_mut().map(read_entry).collect()
    }

    // 1 query. Oldest first, so a run cut short by `limit` leaves the newest rows and the
    // next run continues where this one stopped.
    async fn prune_log(&self, older_than_ms: u64, keep_max: u32, limit: u32) -> Result<u64> {
        let s = self.stmt(
            "prune_log",
            "DELETE FROM system_log WHERE id IN (SELECT id FROM system_log \
             WHERE at_ms < ?1 OR id NOT IN \
             (SELECT id FROM system_log ORDER BY at_ms DESC, id DESC LIMIT ?2) \
             ORDER BY at_ms, id LIMIT ?3)",
            &[
                ubound(older_than_ms),
                JsValue::from(keep_max),
                JsValue::from(limit),
            ],
        )?;
        let r = self.exec(s).await?;
        let deleted = Self::changes("prune_log", &r)?;
        // What the free plan's daily write budget sees: each row once per table and index.
        if let Ok(Some(meta)) = r.meta() {
            console_log!(
                "prune_log deleted={deleted} rows_written={}",
                meta.rows_written.unwrap_or_default()
            );
        }
        Ok(deleted)
    }
}

impl LogSink for D1Store {
    async fn store_event(&self, event: Event) -> Result<()> {
        self.append(event).await.map(drop)
    }
}
