use plumblog_model::Result;

use super::{MemoryStore, conflict, count, invalid, next_rowid, not_found};
use crate::{
    AttemptResult, Delivery, DeliveryStore, NewDelivery, NewWebhook, Webhook, WebhookStore,
    page_len,
};

#[expect(
    clippy::unused_async_trait_impl,
    reason = "in-memory stand-in for async D1 storage"
)]
impl WebhookStore for MemoryStore {
    async fn create_webhook(&self, webhook: NewWebhook) -> Result<i64> {
        self.write(|s| {
            let id = next_rowid(&s.webhooks);
            s.webhooks.insert(
                id,
                Webhook {
                    id,
                    url: webhook.url,
                    events: webhook.events,
                    secret: webhook.secret,
                    enabled: webhook.enabled,
                    created_at: webhook.created_at,
                },
            );
            Ok(id)
        })
    }

    async fn list_webhooks(&self) -> Result<Vec<Webhook>> {
        Ok(self.read(|s| s.webhooks.values().cloned().collect()))
    }

    async fn update_webhook(&self, webhook: &Webhook) -> Result<()> {
        self.write(|s| {
            let w = s
                .webhooks
                .get_mut(&webhook.id)
                .ok_or_else(|| not_found("webhook", webhook.id))?;
            *w = Webhook {
                created_at: w.created_at,
                ..webhook.clone()
            };
            Ok(())
        })
    }

    async fn delete_webhook(&self, id: i64) -> Result<()> {
        self.write(|s| {
            s.webhooks
                .remove(&id)
                .ok_or_else(|| not_found("webhook", id))?;
            s.deliveries.retain(|_, d| d.webhook_id != id);
            Ok(())
        })
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "in-memory stand-in for async D1 storage"
)]
impl DeliveryStore for MemoryStore {
    async fn add_deliveries(&self, rows: &[NewDelivery]) -> Result<Vec<i64>> {
        self.write(|s| {
            let mut ids = Vec::with_capacity(rows.len());
            for r in rows {
                if !s.webhooks.contains_key(&r.webhook_id) {
                    return Err(invalid(format!(
                        "delivery for webhook {} that does not exist",
                        r.webhook_id
                    )));
                }
                let id = next_rowid(&s.deliveries);
                s.deliveries.insert(
                    id,
                    Delivery {
                        id,
                        webhook_id: r.webhook_id,
                        msg_id: r.msg_id.clone(),
                        event: r.event.clone(),
                        payload: r.payload.clone(),
                        attempts: 0,
                        next_at_ms: Some(r.created_at_ms),
                        last_status: None,
                        last_error: None,
                        created_at_ms: r.created_at_ms,
                    },
                );
                ids.push(id);
            }
            Ok(ids)
        })
    }

    async fn claim_deliveries(
        &self,
        now_ms: i64,
        lease_until_ms: i64,
        limit: u32,
    ) -> Result<Vec<Delivery>> {
        self.write(|s| {
            let mut due: Vec<(i64, i64)> = s
                .deliveries
                .values()
                .filter_map(|d| d.next_at_ms.filter(|t| *t <= now_ms).map(|t| (t, d.id)))
                .collect();
            due.sort_unstable();
            due.truncate(page_len(limit));
            let mut out = Vec::with_capacity(due.len());
            for (_, id) in due {
                if let Some(d) = s.deliveries.get_mut(&id) {
                    d.attempts += 1;
                    d.next_at_ms = Some(lease_until_ms);
                    out.push(d.clone());
                }
            }
            out.sort_unstable_by_key(|d| d.id);
            Ok(out)
        })
    }

    async fn finish_attempt(&self, id: i64, attempt: i64, result: &AttemptResult) -> Result<()> {
        self.write(|s| {
            let d = s
                .deliveries
                .get_mut(&id)
                .ok_or_else(|| not_found("delivery", id))?;
            if d.attempts != attempt {
                return Err(conflict(format!(
                    "delivery {id} is at attempt {}, not {attempt}",
                    d.attempts
                )));
            }
            d.next_at_ms = result.next_at_ms;
            d.last_status = result.last_status;
            d.last_error.clone_from(&result.last_error);
            Ok(())
        })
    }

    async fn list_deliveries(&self, webhook_id: i64, limit: u32) -> Result<Vec<Delivery>> {
        Ok(self.read(|s| {
            s.deliveries
                .values()
                .rev()
                .filter(|d| d.webhook_id == webhook_id)
                .take(page_len(limit))
                .cloned()
                .collect()
        }))
    }

    async fn prune_deliveries(&self, created_before_ms: i64, limit: u32) -> Result<u64> {
        self.write(|s| {
            let mut doomed: Vec<(i64, i64)> = s
                .deliveries
                .values()
                .filter(|d| d.next_at_ms.is_none() && d.created_at_ms < created_before_ms)
                .map(|d| (d.created_at_ms, d.id))
                .collect();
            doomed.sort_unstable();
            doomed.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
            for (_, id) in &doomed {
                s.deliveries.remove(id);
            }
            Ok(count(doomed.len()))
        })
    }
}
