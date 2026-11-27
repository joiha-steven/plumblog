-- Outgoing webhook deliveries, retried without a queue (issue #18). One row per webhook per
-- event; the cron run claims due rows, sends them and records the outcome. Rows are pruned
-- a while after they finish. 0002 is taken by another change; numbers may have gaps.
-- `webhooks.secret` (0001, "readable by design") now holds the sealed form
-- `v1:<nonce>:<ciphertext>` (AES-256-GCM under the Worker secret WEBHOOK_KEY); a row in any
-- other form is refused when sending. No data change: no webhook existed before this.
-- IF NOT EXISTS: re-running the file by hand is harmless; it only adds a new table.
CREATE TABLE IF NOT EXISTS webhook_deliveries (
  id            INTEGER PRIMARY KEY,
  webhook_id    INTEGER NOT NULL REFERENCES webhooks (id) ON DELETE CASCADE,
  msg_id        TEXT    NOT NULL,                         -- `webhook-id` header; the same on every retry
  event         TEXT    NOT NULL,                         -- webhook event code (post.published...)
  payload       TEXT    NOT NULL,                         -- exact JSON body, so retries sign identical bytes
  attempts      INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
  next_at_ms    INTEGER,                                  -- NULL = finished (delivered or given up)
  last_status   INTEGER,                                  -- HTTP status of the last attempt, NULL if none came back
  last_error    TEXT,                                     -- failure reason code of the last attempt, NULL on success
  created_at_ms INTEGER NOT NULL
);
-- Claiming due rows: WHERE next_at_ms <= now ORDER BY next_at_ms, id.
CREATE INDEX IF NOT EXISTS webhook_deliveries_due ON webhook_deliveries (next_at_ms);
-- Recent deliveries of one webhook (id DESC), and the ON DELETE CASCADE from webhooks.
CREATE INDEX IF NOT EXISTS webhook_deliveries_webhook ON webhook_deliveries (webhook_id, id);
-- Pruning finished rows: WHERE next_at_ms IS NULL AND created_at_ms < cutoff ORDER BY created_at_ms, id.
CREATE INDEX IF NOT EXISTS webhook_deliveries_finished ON webhook_deliveries (created_at_ms) WHERE next_at_ms IS NULL;
