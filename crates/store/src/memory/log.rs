use plumblog_model::Result;
use plumblog_syslog::{Event, Logger};

use super::{MemoryStore, count, invalid, next_rowid};
use crate::{LogEntry, LogQuery, LogStore, page_len};

#[expect(
    clippy::unused_async_trait_impl,
    reason = "in-memory stand-in for async D1 storage"
)]
impl LogStore for MemoryStore {
    async fn append(&self, event: Event) -> Result<i64> {
        self.write(|s| {
            if let Some(a) = event.actor_id()
                && !s.user_exists(a)
            {
                return Err(invalid(format!("log actor {} does not exist", a.0)));
            }
            let id = next_rowid(&s.log);
            s.log.insert(id, event);
            Ok(id)
        })
    }

    async fn query_log(&self, q: LogQuery) -> Result<Vec<LogEntry>> {
        Ok(self.read(|s| {
            let mut rows: Vec<(&i64, &Event)> = s
                .log
                .iter()
                .filter(|(_, e)| q.kind.is_none_or(|k| e.kind() == k))
                .filter(|(_, e)| q.actor.is_none_or(|a| e.actor_id() == Some(a)))
                .filter(|(_, e)| q.from_ms.is_none_or(|t| e.at_ms() >= t))
                .filter(|(_, e)| q.to_ms.is_none_or(|t| e.at_ms() < t))
                .filter(|(id, e)| q.before.is_none_or(|b| (e.at_ms(), **id) < b))
                .collect();
            rows.sort_by(|(ia, a), (ib, b)| (b.at_ms(), *ib).cmp(&(a.at_ms(), *ia)));
            rows.into_iter()
                .take(page_len(q.limit))
                .map(|(id, e)| LogEntry {
                    id: *id,
                    event: e.clone(),
                })
                .collect()
        }))
    }

    async fn prune_log(&self, older_than_ms: u64, keep_max: u32, limit: u32) -> Result<u64> {
        self.write(|s| {
            let mut newest: Vec<(u64, i64)> =
                s.log.iter().map(|(id, e)| (e.at_ms(), *id)).collect();
            newest.sort_unstable_by(|a, b| b.cmp(a));
            let keep = usize::try_from(keep_max).unwrap_or(usize::MAX);
            let mut doomed: Vec<(u64, i64)> = newest
                .iter()
                .enumerate()
                .filter(|(rank, (at, _))| *at < older_than_ms || *rank >= keep)
                .map(|(_, row)| *row)
                .collect();
            doomed.sort_unstable();
            doomed.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
            for (_, id) in &doomed {
                s.log.remove(id);
            }
            Ok(count(doomed.len()))
        })
    }
}

impl Logger for MemoryStore {
    async fn record(&self, event: Event) -> Result<()> {
        self.append(event).await.map(drop)
    }
}
