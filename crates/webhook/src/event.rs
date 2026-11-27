//! What a webhook can subscribe to.

use plumblog_model::stable_codes;
use plumblog_store::Webhook;

stable_codes! {
    /// Webhook events. Codes are sent to receivers (`type` in the payload) and stored in
    /// `webhooks.events` and `webhook_deliveries.event`, so a code is never renamed once
    /// shipped.
    pub enum WebhookEvent {
        PostPublished => "post.published",
        PostUpdated => "post.updated",
        PostDeleted => "post.deleted",
    }
}

/// `webhooks.events` for a set of events: codes joined by commas, in [`WebhookEvent::ALL`]
/// order, without duplicates.
#[must_use]
pub fn events_column(events: &[WebhookEvent]) -> String {
    WebhookEvent::ALL
        .iter()
        .filter(|e| events.contains(e))
        .map(|e| e.as_str())
        .collect::<Vec<_>>()
        .join(",")
}

/// Whether `webhook` is enabled and subscribed to `event`. Unknown codes in the column (a
/// row written by a newer version) are ignored.
#[must_use]
pub fn wants(webhook: &Webhook, event: WebhookEvent) -> bool {
    webhook.enabled
        && webhook
            .events
            .split(',')
            .any(|code| code.trim() == event.as_str())
}

#[cfg(test)]
mod tests {
    use super::{WebhookEvent, events_column, wants};
    use plumblog_store::Webhook;

    /// Receivers switch on these strings.
    #[test]
    fn event_codes_are_pinned() {
        let table: Vec<_> = WebhookEvent::ALL.iter().map(|e| (*e, e.as_str())).collect();
        assert_eq!(
            table,
            [
                (WebhookEvent::PostPublished, "post.published"),
                (WebhookEvent::PostUpdated, "post.updated"),
                (WebhookEvent::PostDeleted, "post.deleted"),
            ]
        );
    }

    #[test]
    fn events_column_is_canonical_and_wants_reads_it() {
        let col = events_column(&[
            WebhookEvent::PostDeleted,
            WebhookEvent::PostPublished,
            WebhookEvent::PostDeleted,
        ]);
        assert_eq!(col, "post.published,post.deleted");
        let mut w = Webhook {
            id: 1,
            url: "https://example.com/".into(),
            events: format!("{col}, future.event"),
            secret: String::new(),
            enabled: true,
            created_at: 0,
        };
        assert!(wants(&w, WebhookEvent::PostPublished));
        assert!(wants(&w, WebhookEvent::PostDeleted));
        assert!(!wants(&w, WebhookEvent::PostUpdated));
        w.enabled = false;
        assert!(!wants(&w, WebhookEvent::PostPublished));
    }
}
