use plumblog_model::Result;

use super::{MemoryStore, next_rowid, not_found};
use crate::{NewWebhook, Webhook, WebhookStore};

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
                .map(drop)
                .ok_or_else(|| not_found("webhook", id))
        })
    }
}
