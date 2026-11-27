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
        SignInLocked => "auth.sign_in_locked",
        GlobalCap => "auth.global_cap",
        PasswordChanged => "auth.password_changed",
        SignOut => "auth.sign_out",
        PasskeyAdded => "auth.passkey_added",
        PasskeyRemoved => "auth.passkey_removed",
        TokenCreated => "auth.token_created",
        TokenRevoked => "auth.token_revoked",
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
        // A reader sent a comment (stored as pending). Ids only: never the name or text.
        CommentCreated => "comment.created",
        // Someone approved, spammed, trashed or deleted a comment.
        CommentModerated => "comment.moderated",
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
            | Self::PasswordChanged
            | Self::SignOut
            | Self::PasskeyAdded
            | Self::PasskeyRemoved
            | Self::TokenCreated
            | Self::TokenRevoked
            | Self::UserCreated
            | Self::ContentChanged
            | Self::SettingsChanged
            | Self::ThemeChanged
            | Self::PluginChanged
            | Self::Deploy
            | Self::Migration
            | Self::ScheduledJob
            | Self::WebhookDelivery
            | Self::CommentCreated
            | Self::CommentModerated => Severity::Info,
            Self::SignInFailed | Self::SignInLocked | Self::GlobalCap | Self::PluginDisabled => {
                Severity::Warning
            }
            Self::Error => Severity::Error,
        }
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
        self.subject = redact::subject(subject);
        self
    }

    /// One piece of context (`from` / `to` of a setting, webhook status...). Secret-named
    /// keys and personal-looking values are redacted; extra fields beyond [`MAX_FIELDS`] are
    /// dropped.
    #[must_use]
    pub fn field(mut self, key: &str, value: &str) -> Self {
        if self.fields.len() < MAX_FIELDS {
            // The full name decides redaction: a long name must not hide a `_password`
            // suffix behind the cut.
            let value = redact::value(key.trim(), value);
            let key = redact::truncate(key.trim(), 40);
            self.fields.push((key, value));
        }
        self
    }

    /// Subject and fields as one text column (D1 `system_log.detail`): the subject on the
    /// first line, then one `key<TAB>value` line per field, with `\\`, newline and tab escaped
    /// so any text round-trips.
    #[must_use]
    pub fn detail(&self) -> String {
        let mut out = escape(&self.subject);
        for (k, v) in &self.fields {
            out.push('\n');
            out.push_str(&escape(k));
            out.push('\t');
            out.push_str(&escape(v));
        }
        out
    }

    /// Rebuilds a stored event from its columns. `detail` goes through the same redacting
    /// setters as a new event, so a row written by an older or buggy build cannot surface a
    /// secret in the admin.
    #[must_use]
    pub fn restore(
        at_ms: u64,
        request_id: RequestId,
        kind: EventKind,
        actor: Option<UserId>,
        error: Option<ErrorCode>,
        detail: &str,
    ) -> Self {
        let mut lines = detail.split('\n');
        let mut event = Self::new(kind, request_id, at_ms)
            .actor(actor)
            .subject(&unescape(lines.next().unwrap_or_default()));
        event.error = error;
        for line in lines {
            let (k, v) = line.split_once('\t').unwrap_or((line, ""));
            event = event.field(&unescape(k), &unescape(v));
        }
        event
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

fn escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{Event, EventKind, MAX_FIELDS, Severity};
    use crate::REDACTED;
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
                (EventKind::SignInLocked, "auth.sign_in_locked"),
                (EventKind::GlobalCap, "auth.global_cap"),
                (EventKind::PasswordChanged, "auth.password_changed"),
                (EventKind::SignOut, "auth.sign_out"),
                (EventKind::PasskeyAdded, "auth.passkey_added"),
                (EventKind::PasskeyRemoved, "auth.passkey_removed"),
                (EventKind::TokenCreated, "auth.token_created"),
                (EventKind::TokenRevoked, "auth.token_revoked"),
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
                (EventKind::CommentCreated, "comment.created"),
                (EventKind::CommentModerated, "comment.moderated"),
            ]
        );
        for k in EventKind::ALL {
            assert_eq!(EventKind::parse(k.as_str()), Some(*k));
        }
        assert_eq!(EventKind::parse("page.view"), None);
    }

    /// How API writes are attributed: `via` is `session` or `token`, and `client` names the
    /// token. "token <name>" in one value would read as a secret and be redacted whole, so the
    /// name gets its own field. The name stays readable; a pasted secret never does.
    #[test]
    fn via_fields_keep_token_names_and_drop_secrets() {
        let secret = "plb_BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc";
        let e = Event::new(EventKind::ContentChanged, rid(), 1)
            .field("via", "token")
            .field("client", "Claude agent")
            .field("via", "session")
            .field("client", secret);
        let values: Vec<_> = e.fields().iter().map(|(_, v)| v.as_str()).collect();
        assert_eq!(values[..3], ["token", "Claude agent", "session"]);
        assert!(!values[3].contains(secret), "{values:?}");
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
        assert_eq!(
            Event::new(EventKind::SignInLocked, rid(), 1).severity(),
            Severity::Warning
        );
        assert_eq!(
            Event::new(EventKind::GlobalCap, rid(), 1).severity(),
            Severity::Warning
        );
        assert_eq!(
            Event::new(EventKind::PasskeyRemoved, rid(), 1).severity(),
            Severity::Info
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

    #[test]
    fn detail_round_trips_any_text() {
        let e = Event::new(EventKind::SettingsChanged, rid(), 9)
            .actor(Some(UserId(3)))
            .error(ErrorCode::Storage)
            .subject("a\\b\nc\td")
            .field("from", "line1\nline2")
            .field("to", "tab\there \\n literal")
            .field("empty", "");
        let back = Event::restore(
            9,
            rid(),
            EventKind::SettingsChanged,
            Some(UserId(3)),
            Some(ErrorCode::Storage),
            &e.detail(),
        );
        assert_eq!(back, e);
        let bare = Event::new(EventKind::Deploy, rid(), 1);
        assert_eq!(bare.detail(), "");
        assert_eq!(
            Event::restore(1, rid(), EventKind::Deploy, None, None, ""),
            bare
        );
    }

    #[test]
    fn restore_redacts_again() {
        let raw = "x\npassword\thunter2\nnote\tmail me at a@b.co\ncolor\tblue";
        let e = Event::restore(1, rid(), EventKind::SettingsChanged, None, None, raw);
        let values: Vec<_> = e.fields().iter().map(|(_, v)| v.clone()).collect();
        assert_eq!(
            values,
            [
                REDACTED.to_owned(),
                format!("mail me at {REDACTED}"),
                "blue".to_owned()
            ]
        );
    }

    /// A long key is checked whole, then cut: the `_password` suffix past 40 chars counts.
    #[test]
    fn long_keys_are_checked_before_truncation() {
        let key = format!("{}_password", "a".repeat(60));
        let e = Event::new(EventKind::SettingsChanged, rid(), 1).field(&key, "hunter2");
        assert_eq!(e.fields()[0].1, REDACTED);
        assert_eq!(e.fields()[0].0.chars().count(), 41);
    }
}
