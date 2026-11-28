//! System log: what the system did, for the owner's timeline (docs/ARCHITECTURE.md, System
//! log). Event shape, redaction, retention policy and request scoping live here, all pure;
//! storage is a `store` concern.

mod event;
mod redact;
mod retention;
mod scope;

pub use event::{Event, EventKind, Severity, escape};
pub use redact::{MAX_DEPTH, REDACTED, redacted_label, secret_name};
pub use retention::Retention;
pub use scope::{Scope, request_id};

use plumblog_model::Result;

/// Where events are stored; the stores implement it. Request and job code never call it:
/// they call [`record`], which cannot fail. One call per event, never per page view.
// Workers are single-threaded, so futures need not be `Send` (see auth::Crypto).
#[allow(async_fn_in_trait)]
pub trait LogSink {
    async fn store_event(&self, event: Event) -> Result<()>;
}

/// Logs one event. Never fails: a storage error goes to `report` (the Worker passes
/// `console_error!`) as one line naming the request id, event kind and error, and the
/// caller carries on, so logging can never break the request it describes.
pub async fn record<S: LogSink>(sink: &S, event: Event, report: impl FnOnce(&str)) {
    let (id, kind) = (event.request_id().clone(), event.kind());
    if let Err(e) = sink.store_event(event).await {
        report(&format!(
            "[req={}] syslog.record_failed kind={} error={e}",
            id.as_str(),
            kind.as_str()
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::{Event, EventKind, LogSink, record};
    use plumblog_model::{Error, ErrorCode, RequestId, Result};
    use std::cell::RefCell;
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    struct Broken;
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "stands in for an async store that fails"
    )]
    impl LogSink for Broken {
        async fn store_event(&self, _: Event) -> Result<()> {
            Err(Error::new(ErrorCode::Storage, "D1 unavailable"))
        }
    }

    /// The futures here never wait, so polling once runs them to completion.
    fn run<F: Future>(f: F) -> F::Output {
        match pin!(f)
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
        {
            Poll::Ready(v) => v,
            Poll::Pending => unreachable!("no future here waits"),
        }
    }

    #[test]
    fn a_failing_store_is_reported_never_returned() {
        let rid = RequestId::parse("8f2a1c3b9d-SIN").unwrap();
        let reports = RefCell::new(Vec::new());
        let event = Event::new(EventKind::SettingsChanged, rid, 1);
        // `record` returns `()`: there is no error for a route to propagate.
        let () = run(record(&Broken, event, |m| {
            reports.borrow_mut().push(m.to_owned());
        }));
        let reports = reports.into_inner();
        assert_eq!(reports.len(), 1);
        assert!(
            reports[0]
                .starts_with("[req=8f2a1c3b9d-SIN] syslog.record_failed kind=settings.changed"),
            "{}",
            reports[0]
        );
        assert!(reports[0].contains("D1 unavailable"), "{}", reports[0]);
    }
}
