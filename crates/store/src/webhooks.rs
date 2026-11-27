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
