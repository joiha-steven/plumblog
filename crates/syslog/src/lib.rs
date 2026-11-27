//! System log: what the system did, for the owner's timeline (docs/ARCHITECTURE.md, System
//! log). Only the event shape and the interface live here; storage is a `store` concern.

use plumblog_model::{ErrorCode, RequestId, Result, UserId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EventKind {
    SignIn,
    SignInFailed,
    ContentChanged,
    SettingsChanged,
    ThemeChanged,
    PluginChanged,
    PluginDisabled,
    Deploy,
    Migration,
    ScheduledJob,
    WebhookDelivery,
    Error,
}

impl EventKind {
    pub const ALL: &[EventKind] = &[
        Self::SignIn,
        Self::SignInFailed,
        Self::ContentChanged,
        Self::SettingsChanged,
        Self::ThemeChanged,
        Self::PluginChanged,
        Self::PluginDisabled,
        Self::Deploy,
        Self::Migration,
        Self::ScheduledJob,
        Self::WebhookDelivery,
        Self::Error,
    ];

    /// Stored in D1 and used by the admin filter, so never renamed once shipped.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SignIn => "auth.sign_in",
            Self::SignInFailed => "auth.sign_in_failed",
            Self::ContentChanged => "content.changed",
            Self::SettingsChanged => "settings.changed",
            Self::ThemeChanged => "theme.changed",
            Self::PluginChanged => "plugin.changed",
            Self::PluginDisabled => "plugin.disabled",
            Self::Deploy => "system.deploy",
            Self::Migration => "system.migration",
            Self::ScheduledJob => "job.run",
            Self::WebhookDelivery => "webhook.delivery",
            Self::Error => "error",
        }
    }
}

/// One timeline entry. No field holds passwords, tokens or reader data by construction:
/// the only person recorded is the acting user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    pub at_ms: u64,
    pub request_id: RequestId,
    pub kind: EventKind,
    pub actor: Option<UserId>,
    pub error: Option<ErrorCode>,
    /// Plain-language summary for the owner, e.g. a post slug or setting key.
    pub detail: String,
}

/// Where events go. One call per event, never per page view.
// Workers are single-threaded, so futures need not be `Send` (see auth::Crypto).
#[allow(async_fn_in_trait)]
pub trait Logger {
    async fn record(&self, event: Event) -> Result<()>;
}

#[cfg(test)]
mod tests {
    use super::EventKind;
    use std::collections::HashSet;

    #[test]
    fn event_kind_codes_are_unique() {
        let codes: HashSet<_> = EventKind::ALL.iter().map(|k| k.as_str()).collect();
        assert_eq!(codes.len(), EventKind::ALL.len());
    }
}
