use plumblog_model::{Error, ErrorCode, Result};
use plumblog_store::{
    AttemptResult, Delivery, DeliveryStore, NewDelivery, NewWebhook, Webhook, WebhookStore,
};
use worker::wasm_bindgen::JsValue;

use super::{D1Store, Row, bound, flag, int, limit, not_found, opt_int, opt_text, text};

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

const DELIVERY_COLUMNS: &str = "id, webhook_id, msg_id, event, payload, attempts, next_at_ms, \
     last_status, last_error, created_at_ms";

fn read_delivery(r: &mut Row) -> Result<Delivery> {
    Ok(Delivery {
        id: r.int()?,
        webhook_id: r.int()?,
        msg_id: r.text()?,
        event: r.text()?,
        payload: r.text()?,
        attempts: r.int()?,
        next_at_ms: r.opt_int()?,
        last_status: r.opt_int()?,
        last_error: r.opt_text()?,
        created_at_ms: r.int()?,
    })
}

impl DeliveryStore for D1Store {
    // 1 batch; a missing webhook fails the FK: InvalidInput, nothing queued.
    async fn add_deliveries(&self, rows: &[NewDelivery]) -> Result<Vec<i64>> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        let mut stmts = Vec::with_capacity(rows.len());
        for d in rows {
            stmts.push(self.stmt(
                "add_delivery",
                "INSERT INTO webhook_deliveries \
                 (webhook_id, msg_id, event, payload, attempts, next_at_ms, created_at_ms) \
                 VALUES (?, ?, ?, ?, 0, ?, ?)",
                &[
                    int(d.webhook_id)?,
                    text(&d.msg_id),
                    text(&d.event),
                    text(&d.payload),
                    int(d.created_at_ms)?,
                    int(d.created_at_ms)?,
                ],
            )?);
        }
        let results = self.batch("add_deliveries", stmts).await?;
        results
            .iter()
            .map(|r| Self::last_row_id("add_delivery", r))
            .collect()
    }

    // 1 query. One UPDATE picks and leases the rows, so two runs never claim the same row.
    async fn claim_deliveries(
        &self,
        now_ms: i64,
        lease_until_ms: i64,
        n: u32,
    ) -> Result<Vec<Delivery>> {
        let s = self.stmt(
            "claim_deliveries",
            &format!(
                "UPDATE webhook_deliveries SET attempts = attempts + 1, next_at_ms = ?1 \
                 WHERE id IN (SELECT id FROM webhook_deliveries WHERE next_at_ms <= ?2 \
                 ORDER BY next_at_ms, id LIMIT ?3) RETURNING {DELIVERY_COLUMNS}"
            ),
            &[int(lease_until_ms)?, bound(now_ms), limit(n)],
        )?;
        let mut out: Vec<Delivery> = self
            .rows(s)
            .await?
            .iter_mut()
            .map(read_delivery)
            .collect::<Result<_>>()?;
        // RETURNING order is unspecified in SQLite.
        out.sort_unstable_by_key(|d| d.id);
        Ok(out)
    }

    // 1 query; 1 more only when nothing was written, to say why.
    async fn finish_attempt(&self, id: i64, attempt: i64, r: &AttemptResult) -> Result<()> {
        let s = self.stmt(
            "finish_attempt",
            "UPDATE webhook_deliveries SET next_at_ms = ?, last_status = ?, last_error = ? \
             WHERE id = ? AND attempts = ?",
            &[
                opt_int(r.next_at_ms)?,
                opt_int(r.last_status)?,
                opt_text(r.last_error.as_deref()),
                bound(id),
                bound(attempt),
            ],
        )?;
        let res = self.exec(s).await?;
        if Self::changes("finish_attempt", &res)? > 0 {
            return Ok(());
        }
        let s = self.stmt(
            "finish_attempt_why",
            "SELECT attempts FROM webhook_deliveries WHERE id = ?",
            &[bound(id)],
        )?;
        match self.row(s).await? {
            None => Err(not_found("delivery", id)),
            Some(mut row) => Err(Error::new(
                ErrorCode::Conflict,
                format!("delivery {id} is at attempt {}, not {attempt}", row.int()?),
            )),
        }
    }

    // 1 query.
    async fn list_deliveries(&self, webhook_id: i64, n: u32) -> Result<Vec<Delivery>> {
        let s = self.stmt(
            "list_deliveries",
            &format!(
                "SELECT {DELIVERY_COLUMNS} FROM webhook_deliveries WHERE webhook_id = ? \
                 ORDER BY id DESC LIMIT ?"
            ),
            &[bound(webhook_id), limit(n)],
        )?;
        self.rows(s).await?.iter_mut().map(read_delivery).collect()
    }

    // 1 query. Oldest first, so a run cut short by `limit` continues next time.
    async fn prune_deliveries(&self, created_before_ms: i64, n: u32) -> Result<u64> {
        let s = self.stmt(
            "prune_deliveries",
            "DELETE FROM webhook_deliveries WHERE id IN (SELECT id FROM webhook_deliveries \
             WHERE next_at_ms IS NULL AND created_at_ms < ? ORDER BY created_at_ms, id LIMIT ?)",
            &[bound(created_before_ms), JsValue::from(n)],
        )?;
        let r = self.exec(s).await?;
        Self::changes("prune_deliveries", &r)
    }
}
