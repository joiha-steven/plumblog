//! System log: what the system did, for the owner's timeline (docs/ARCHITECTURE.md, System
//! log). Event shape, redaction, retention policy and request scoping live here, all pure;
//! storage is a `store` concern.

mod event;
mod redact;
mod retention;
mod scope;

pub use event::{Event, EventKind, Severity};
pub use redact::REDACTED;
pub use retention::{Prune, Retention};
pub use scope::{Scope, request_id};

use plumblog_model::Result;

/// Where events go. One call per event, never per page view.
// Workers are single-threaded, so futures need not be `Send` (see auth::Crypto).
#[allow(async_fn_in_trait)]
pub trait Logger {
    async fn record(&self, event: Event) -> Result<()>;
}
