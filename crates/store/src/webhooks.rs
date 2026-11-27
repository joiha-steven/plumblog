//! Outgoing webhooks (`webhooks`).

use plumblog_model::Result;

/// A row of `webhooks`. `secret` is readable by design: deliveries are signed with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Webhook {
    pub id: i64,
    pub url: String,
    /// Comma-separated event codes.
    pub events: String,
    pub secret: String,
    pub enabled: bool,
    pub created_at: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewWebhook {
    pub url: String,
    pub events: String,
    pub secret: String,
    pub enabled: bool,
    pub created_at: i64,
}

// Workers are single-threaded, so futures need not be `Send` (see plumblog_auth::Crypto).
#[allow(async_fn_in_trait)]
pub trait WebhookStore {
    async fn create_webhook(&self, webhook: NewWebhook) -> Result<i64>;
    /// Ordered by id.
    async fn list_webhooks(&self) -> Result<Vec<Webhook>>;
    /// Writes every field except `id` and `created_at`. `NotFound`.
    async fn update_webhook(&self, webhook: &Webhook) -> Result<()>;
    /// `NotFound`.
    async fn delete_webhook(&self, id: i64) -> Result<()>;
}

/// A delivery to queue (`webhook_deliveries`): due at once (`next_at_ms = created_at_ms`),
/// no attempts yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewDelivery {
    pub webhook_id: i64,
    /// The `webhook-id` header, the same on every retry so receivers can drop duplicates.
    pub msg_id: String,
    /// Webhook event code.
    pub event: String,
    /// The exact JSON body; retries sign these same bytes.
    pub payload: String,
    pub created_at_ms: i64,
}

/// A row of `webhook_deliveries`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delivery {
    pub id: i64,
    pub webhook_id: i64,
    pub msg_id: String,
    pub event: String,
    pub payload: String,
    /// Attempts started, counting one that is in flight.
    pub attempts: i64,
    /// `None` once finished (delivered or given up).
    pub next_at_ms: Option<i64>,
    pub last_status: Option<i64>,
    pub last_error: Option<String>,
    pub created_at_ms: i64,
}

/// What one attempt left behind.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttemptResult {
    /// When to try again; `None` finishes the delivery.
    pub next_at_ms: Option<i64>,
    pub last_status: Option<i64>,
    pub last_error: Option<String>,
}

// Workers are single-threaded, so futures need not be `Send` (see plumblog_auth::Crypto).
#[allow(async_fn_in_trait)]
pub trait DeliveryStore {
    /// Queues all of `rows` in one write and returns their ids in order. `InvalidInput` if a
    /// webhook does not exist (then nothing is queued).
    async fn add_deliveries(&self, rows: &[NewDelivery]) -> Result<Vec<i64>>;
    /// Claims up to `limit` due rows (`next_at_ms <= now_ms`), earliest due first
    /// (`next_at_ms`, id), in one atomic write: each gets `attempts + 1` and
    /// `next_at_ms = lease_until_ms`, so a concurrent claim cannot take it again, and a run
    /// that dies mid-send leaves it to be retried once the lease ends. Returns the claimed
    /// rows as written, ordered by id.
    async fn claim_deliveries(
        &self,
        now_ms: i64,
        lease_until_ms: i64,
        limit: u32,
    ) -> Result<Vec<Delivery>>;
    /// Records the outcome of attempt number `attempt`. `NotFound` if the row is gone (its
    /// webhook was deleted); `Conflict` if `attempts` is no longer `attempt` (a later claim
    /// took it after the lease ran out), and then nothing is written.
    async fn finish_attempt(&self, id: i64, attempt: i64, result: &AttemptResult) -> Result<()>;
    /// One webhook's deliveries, newest first (id DESC).
    async fn list_deliveries(&self, webhook_id: i64, limit: u32) -> Result<Vec<Delivery>>;
    /// Deletes at most `limit` finished rows created before `created_before_ms`, oldest
    /// first (`created_at_ms`, id); returns how many went. Pending rows are never pruned.
    async fn prune_deliveries(&self, created_before_ms: i64, limit: u32) -> Result<u64>;
}
