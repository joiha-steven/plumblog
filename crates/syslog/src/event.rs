//! The event catalogue and the event record.

use crate::redact;
use plumblog_model::{ErrorCode, RequestId, UserId, stable_codes};

stable_codes! {
    /// Kinds of timeline entries (docs/ARCHITECTURE.md, System log: "Recorded"). Codes are
    /// stored in D1, used by the admin filter and name the locale key `syslog.<code>` the
    /// admin shows, so a code is never renamed once shipped.
    pub enum EventKind {
        SignIn => "auth.sign_in",
        SignInFailed => "auth.sign_in_failed",
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

/// How the timeline marks an entry. Derived from the kind, plus `Warning` for a failed job
/// or webhook (an event with an error code), so the owner sees problems first.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Severity {
    Info,
    Warning,
    Error,
}

impl EventKind {
    /// Severity when the event carries no error code. An exhaustive match: a new kind must
    /// decide how loud it is.
    #[must_use]
    pub const fn base_severity(self) -> Severity {
        match self {
            Self::SignIn
            | Self::ContentChanged
            | Self::SettingsChanged
            | Self::ThemeChanged
            | Self::PluginChanged
            | Self::Deploy
            | Self::Migration
            | Self::ScheduledJob
            | Self::WebhookDelivery => Severity::Info,
            Self::SignInFailed | Self::PluginDisabled => Severity::Warning,
            Self::Error => Severity::Error,
        }
    }

    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.as_str() == code)
    }
}

/// Longest stored subject or field value, in characters: keeps one row small and stops a
/// pasted document from turning the log into a copy of the content.
pub const MAX_TEXT: usize = 200;
/// Most context fields per event.
pub const MAX_FIELDS: usize = 8;

/// One timeline entry. Fields are private and set through methods that redact, so no code
/// path can store a password, token or reader data (docs/ARCHITECTURE.md, "Never recorded").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    at_ms: u64,
    request_id: RequestId,
    kind: EventKind,
    actor: Option<UserId>,
    error: Option<ErrorCode>,
    subject: String,
    fields: Vec<(String, String)>,
}

impl Event {
    /// Use [`crate::Scope::event`] in request code so the request id cannot be forgotten.
    #[must_use]
    pub fn new(kind: EventKind, request_id: RequestId, at_ms: u64) -> Self {
        Self {
            at_ms,
            request_id,
            kind,
            actor: None,
            error: None,
            subject: String::new(),
            fields: Vec::new(),
        }
    }

    /// Who did it; `None` for the system (cron, deploy).
    #[must_use]
    pub fn actor(mut self, user: Option<UserId>) -> Self {
        self.actor = user;
        self
    }

    /// The stable error code of a failure; the admin shows its plain message from the locale.
    #[must_use]
    pub fn error(mut self, code: ErrorCode) -> Self {
        self.error = Some(code);
        self
    }

    /// What it was about: a post slug, setting key, job or plugin name. Redacted and capped.
    #[must_use]
    pub fn subject(mut self, subject: &str) -> Self {
        self.subject = redact::value("subject", subject);
        self
    }

    /// One piece of context (`from` / `to` of a setting, webhook status...). Secret-named
    /// keys and personal-looking values are redacted; extra fields beyond [`MAX_FIELDS`] are
    /// dropped.
    #[must_use]
    pub fn field(mut self, key: &str, value: &str) -> Self {
        if self.fields.len() < MAX_FIELDS {
            let key = redact::truncate(key.trim(), 40);
            let value = redact::value(&key, value);
            self.fields.push((key, value));
        }
        self
    }

    #[must_use]
    pub fn at_ms(&self) -> u64 {
        self.at_ms
    }

    #[must_use]
    pub fn request_id(&self) -> &RequestId {
        &self.request_id
    }

    #[must_use]
    pub fn kind(&self) -> EventKind {
        self.kind
    }

    #[must_use]
    pub fn actor_id(&self) -> Option<UserId> {
        self.actor
    }

    #[must_use]
    pub fn error_code(&self) -> Option<ErrorCode> {
        self.error
    }

    #[must_use]
    pub fn subject_text(&self) -> &str {
        &self.subject
    }

    #[must_use]
    pub fn fields(&self) -> &[(String, String)] {
        &self.fields
    }

    #[must_use]
    pub fn severity(&self) -> Severity {
        let base = self.kind.base_severity();
        if self.error.is_some() {
            base.max(Severity::Warning)
        } else {
            base
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Event, EventKind, MAX_FIELDS, Severity};
    use plumblog_model::{ErrorCode, RequestId, UserId};

    fn rid() -> RequestId {
        RequestId::parse("8f2a1c3b9d-SIN").unwrap()
    }

    /// Pins every stored code: renaming one orphans existing D1 rows.
    #[test]
    fn event_kind_codes_are_pinned() {
        let table: Vec<_> = EventKind::ALL.iter().map(|k| (*k, k.as_str())).collect();
        assert_eq!(
            table,
            [
                (EventKind::SignIn, "auth.sign_in"),
                (EventKind::SignInFailed, "auth.sign_in_failed"),
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
        for k in EventKind::ALL {
            assert_eq!(EventKind::from_code(k.as_str()), Some(*k));
        }
        assert_eq!(EventKind::from_code("page.view"), None);
    }

    #[test]
    fn severity_rises_with_an_error_code() {
        let ok = Event::new(EventKind::WebhookDelivery, rid(), 1);
        assert_eq!(ok.severity(), Severity::Info);
        let failed = ok.clone().error(ErrorCode::Internal);
        assert_eq!(failed.severity(), Severity::Warning);
        assert_eq!(
            Event::new(EventKind::Error, rid(), 1).severity(),
            Severity::Error
        );
        assert_eq!(
            Event::new(EventKind::SignInFailed, rid(), 1).severity(),
            Severity::Warning
        );
    }

    #[test]
    fn builder_records_context_and_caps_it() {
        let mut e = Event::new(EventKind::SettingsChanged, rid(), 42)
            .actor(Some(UserId(7)))
            .subject("site.title")
            .field("from", "Old")
            .field("to", "New");
        assert_eq!(e.actor_id(), Some(UserId(7)));
        assert_eq!(e.subject_text(), "site.title");
        assert_eq!(e.fields()[1], ("to".to_owned(), "New".to_owned()));
        for i in 0..20 {
            e = e.field(&format!("k{i}"), "v");
        }
        assert_eq!(e.fields().len(), MAX_FIELDS);
    }
}
