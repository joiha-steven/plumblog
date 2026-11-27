//! Request scoping: one request id per request, on every event and every Worker log line,
//! so an AI can go from an admin entry to the full trace.

use crate::event::{Event, EventKind};
use plumblog_model::{RequestId, UserId};

/// The request id for an incoming request: Cloudflare's `cf-ray` header when valid, else a
/// generated id (cron and queue handlers have no `cf-ray`). `generate` comes from the
/// platform RNG; if it produces something invalid, a fixed id is used rather than none.
#[must_use]
pub fn request_id(cf_ray: Option<&str>, generate: impl FnOnce() -> String) -> RequestId {
    cf_ray
        .and_then(RequestId::parse)
        .or_else(|| RequestId::parse(&generate()))
        .unwrap_or_else(RequestId::unknown)
}

/// What one request knows about itself. Events made from it always carry its request id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Scope {
    request_id: RequestId,
    actor: Option<UserId>,
}

impl Scope {
    #[must_use]
    pub fn new(request_id: RequestId, actor: Option<UserId>) -> Self {
        Self { request_id, actor }
    }

    #[must_use]
    pub fn request_id(&self) -> &RequestId {
        &self.request_id
    }

    /// An event for this request, by this request's user.
    #[must_use]
    pub fn event(&self, kind: EventKind, at_ms: u64) -> Event {
        Event::new(kind, self.request_id.clone(), at_ms).actor(self.actor)
    }

    /// Prefix for Worker log lines, the same id the admin shows.
    #[must_use]
    pub fn log_prefix(&self) -> String {
        format!("[req={}]", self.request_id.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::{Scope, request_id};
    use crate::EventKind;
    use plumblog_model::UserId;

    #[test]
    fn request_id_prefers_cf_ray_then_generated_then_fixed() {
        assert_eq!(
            request_id(Some("8f2a1c3b9d-SIN"), || "x".into()).as_str(),
            "8f2a1c3b9d-SIN"
        );
        assert_eq!(
            request_id(Some("bad id!"), || "abc123".into()).as_str(),
            "abc123"
        );
        assert_eq!(request_id(None, || "abc123".into()).as_str(), "abc123");
        assert_eq!(request_id(None, || "no good".into()).as_str(), "unknown");
    }

    #[test]
    fn scope_events_and_logs_share_the_request_id() {
        let scope = Scope::new(request_id(Some("7a7a-HKG"), String::new), Some(UserId(3)));
        let e = scope
            .event(EventKind::ContentChanged, 10)
            .subject("hello-world");
        assert_eq!(e.request_id(), scope.request_id());
        assert_eq!(e.actor_id(), Some(UserId(3)));
        assert_eq!(scope.log_prefix(), "[req=7a7a-HKG]");
    }
}
