//! System log: what the system did, for the owner's timeline (docs/ARCHITECTURE.md, System
//! log). Only the event shape and the interface live here; storage is a `store` concern.

use plumblog_model::{ErrorCode, RequestId, Result, UserId, stable_codes};

stable_codes! {
    /// Kinds of timeline entries. Codes are stored in D1 and used by the admin filter, so a
    /// code is never renamed once shipped.
    pub enum EventKind {
        SignIn => "auth.sign_in",
        SignInFailed => "auth.sign_in_failed",
        SignInLocked => "auth.sign_in_locked",
        PasswordChanged => "auth.password_changed",
        SignOut => "auth.sign_out",
        PasskeyAdded => "auth.passkey_added",
        PasskeyRemoved => "auth.passkey_removed",
        UserCreated => "user.created",
        ContentChanged => "content.changed",
        SettingsChanged => "settings.changed",
        ThemeChanged => "theme.changed",
        PluginChanged => "plugin.changed",
        PluginDisabled => "plugin.disabled",
        Deploy => "system.deploy",
        Migration => "system.migration",
        ScheduledJob => "job.run",
        WebhookDelivery => "webhook.delivery",
        Error => "error",
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

    /// Pins every stored code: renaming one orphans existing D1 rows.
    #[test]
    fn event_kind_codes_are_pinned() {
        let table: Vec<_> = EventKind::ALL.iter().map(|k| (*k, k.as_str())).collect();
        assert_eq!(
            table,
            [
                (EventKind::SignIn, "auth.sign_in"),
                (EventKind::SignInFailed, "auth.sign_in_failed"),
                (EventKind::SignInLocked, "auth.sign_in_locked"),
                (EventKind::PasswordChanged, "auth.password_changed"),
                (EventKind::SignOut, "auth.sign_out"),
                (EventKind::PasskeyAdded, "auth.passkey_added"),
                (EventKind::PasskeyRemoved, "auth.passkey_removed"),
                (EventKind::UserCreated, "user.created"),
                (EventKind::ContentChanged, "content.changed"),
                (EventKind::SettingsChanged, "settings.changed"),
                (EventKind::ThemeChanged, "theme.changed"),
                (EventKind::PluginChanged, "plugin.changed"),
                (EventKind::PluginDisabled, "plugin.disabled"),
                (EventKind::Deploy, "system.deploy"),
                (EventKind::Migration, "system.migration"),
                (EventKind::ScheduledJob, "job.run"),
                (EventKind::WebhookDelivery, "webhook.delivery"),
                (EventKind::Error, "error"),
            ]
        );
    }
}
