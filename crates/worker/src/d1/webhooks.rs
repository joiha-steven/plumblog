use plumblog_model::Result;
use plumblog_store::{NewWebhook, Webhook, WebhookStore};

use super::{D1Store, bound, flag, int, not_found, text};

impl WebhookStore for D1Store {
    async fn create_webhook(&self, w: NewWebhook) -> Result<i64> {
        let s = self.stmt(
            "create_webhook",
            "INSERT INTO webhooks (url, events, secret, enabled, created_at) VALUES (?, ?, ?, ?, ?)",
            &[
                text(&w.url),
                text(&w.events),
                text(&w.secret),
                flag(w.enabled),
                int(w.created_at)?,
            ],
        )?;
        let r = self.exec(s).await?;
        Self::last_row_id("create_webhook", &r)
    }

    // 1 query.
    async fn list_webhooks(&self) -> Result<Vec<Webhook>> {
        let s = self.stmt(
            "list_webhooks",
            "SELECT id, url, events, secret, enabled, created_at FROM webhooks ORDER BY id",
            &[],
        )?;
        self.rows(s)
            .await?
            .iter_mut()
            .map(|r| {
                Ok(Webhook {
                    id: r.int()?,
                    url: r.text()?,
                    events: r.text()?,
                    secret: r.text()?,
                    enabled: r.flag()?,
                    created_at: r.int()?,
                })
            })
            .collect()
    }

    async fn update_webhook(&self, w: &Webhook) -> Result<()> {
        let s = self.stmt(
            "update_webhook",
            "UPDATE webhooks SET url = ?, events = ?, secret = ?, enabled = ? WHERE id = ?",
            &[
                text(&w.url),
                text(&w.events),
                text(&w.secret),
                flag(w.enabled),
                bound(w.id),
            ],
        )?;
        let r = self.exec(s).await?;
        if Self::changes("update_webhook", &r)? == 0 {
            return Err(not_found("webhook", w.id));
        }
        Ok(())
    }

    async fn delete_webhook(&self, id: i64) -> Result<()> {
        let s = self.stmt(
            "delete_webhook",
            "DELETE FROM webhooks WHERE id = ?",
            &[bound(id)],
        )?;
        let r = self.exec(s).await?;
        if Self::changes("delete_webhook", &r)? == 0 {
            return Err(not_found("webhook", id));
        }
        Ok(())
    }
}
