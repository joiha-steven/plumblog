use plumblog_model::{ErrorCode, RequestId, Result, UserId};
use plumblog_store::{LogEntry, LogQuery, LogStore};
use plumblog_syslog::{Event, EventKind, LogSink};
use worker::console_log;
use worker::wasm_bindgen::JsValue;

use super::log_sql::{Bind, select};
use super::{D1Store, Row, bound, opt_int, opt_text, probe_limit, storage, text, ubound, uint};

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
    let actor_name = r.opt_text()?;
    Ok(LogEntry {
        id,
        event: Event::restore(at_ms, request_id, kind, actor, error, &detail),
        actor_name,
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

    // 1 query (the actor's name is joined in).
    async fn query_log(&self, q: LogQuery<'_>) -> Result<Vec<LogEntry>> {
        let (sql, binds) = select(&q);
        let params: Vec<JsValue> = binds
            .iter()
            .map(|b| match b {
                Bind::Text(s) => text(s),
                Bind::Int(n) => bound(*n),
                Bind::Time(t) => ubound(*t),
                Bind::Limit(n) => probe_limit(*n),
            })
            .collect();
        let s = self.stmt("query_log", &sql, &params)?;
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
