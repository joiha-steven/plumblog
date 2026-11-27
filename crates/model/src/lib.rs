//! Shared types every other crate speaks: ids, errors, roles and permissions. No I/O.

use std::fmt;

/// Engine version, taken from the workspace so it never drifts.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Stable error codes. The string form is a public contract: it appears in API responses,
/// the system log and locale keys (`error.<code>`), so a code is never renamed once shipped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    PostNotFound,
    UserNotFound,
    Forbidden,
    Unauthenticated,
    InvalidInput,
    Storage,
    Internal,
}

impl ErrorCode {
    pub const ALL: &[ErrorCode] = &[
        Self::PostNotFound,
        Self::UserNotFound,
        Self::Forbidden,
        Self::Unauthenticated,
        Self::InvalidInput,
        Self::Storage,
        Self::Internal,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PostNotFound => "post.not_found",
            Self::UserNotFound => "user.not_found",
            Self::Forbidden => "auth.forbidden",
            Self::Unauthenticated => "auth.unauthenticated",
            Self::InvalidInput => "input.invalid",
            Self::Storage => "store.failed",
            Self::Internal => "internal",
        }
    }
}

/// One error type across crates. `detail` is for logs and agents, never shown to readers:
/// user-facing text comes from the locale key derived from `code`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub code: ErrorCode,
    pub detail: String,
}

impl Error {
    pub fn new(code: ErrorCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.detail)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// Ties a system log entry to the Worker log line of the same request.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RequestId(String);

impl RequestId {
    /// Accepts 1-64 chars of `[A-Za-z0-9-]` (fits Cloudflare's `cf-ray`), so ids are safe to
    /// print in logs and URLs without escaping.
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        let ok = (1..=64).contains(&raw.len())
            && raw.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
        ok.then(|| Self(raw.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct UserId(pub i64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PostId(pub i64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    Owner,
    Writer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Permission {
    WritePost,
    PublishPost,
    EditAnyPost,
    DeleteAnyPost,
    UploadMedia,
    CreateOwnToken,
    ManageSettings,
    ManageThemes,
    ManagePlugins,
    ManageUsers,
    ModerateComments,
    ViewSystemLog,
}

/// The whole role model as data (docs/ARCHITECTURE.md, Users and auth). Every permission is
/// listed exactly once, so a missing row is a test failure, not a silent deny.
pub const PERMISSIONS: &[(Permission, &[Role])] = &[
    (Permission::WritePost, &[Role::Owner, Role::Writer]),
    (Permission::PublishPost, &[Role::Owner, Role::Writer]),
    (Permission::EditAnyPost, &[Role::Owner, Role::Writer]),
    (Permission::DeleteAnyPost, &[Role::Owner, Role::Writer]),
    (Permission::UploadMedia, &[Role::Owner, Role::Writer]),
    (Permission::CreateOwnToken, &[Role::Owner, Role::Writer]),
    (Permission::ManageSettings, &[Role::Owner]),
    (Permission::ManageThemes, &[Role::Owner]),
    (Permission::ManagePlugins, &[Role::Owner]),
    (Permission::ManageUsers, &[Role::Owner]),
    (Permission::ModerateComments, &[Role::Owner]),
    (Permission::ViewSystemLog, &[Role::Owner]),
];

impl Role {
    #[must_use]
    pub fn can(self, permission: Permission) -> bool {
        PERMISSIONS
            .iter()
            .any(|(p, roles)| *p == permission && roles.contains(&self))
    }
}

#[cfg(test)]
mod tests {
    use super::{ErrorCode, PERMISSIONS, Permission, RequestId, Role};
    use std::collections::HashSet;

    #[test]
    fn error_codes_are_unique_and_dotted_lowercase() {
        let codes: HashSet<_> = ErrorCode::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(codes.len(), ErrorCode::ALL.len());
        for c in codes {
            assert!(
                c.bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'.' || b == b'_')
            );
        }
        assert_eq!(ErrorCode::PostNotFound.as_str(), "post.not_found");
    }

    #[test]
    fn owner_can_everything_writer_cannot_touch_site_settings() {
        let perms: HashSet<_> = PERMISSIONS.iter().map(|(p, _)| *p).collect();
        assert_eq!(perms.len(), PERMISSIONS.len(), "permission listed twice");
        assert!(PERMISSIONS.iter().all(|(p, _)| Role::Owner.can(*p)));
        assert!(Role::Writer.can(Permission::PublishPost));
        for p in [
            Permission::ManageSettings,
            Permission::ManageThemes,
            Permission::ManageUsers,
            Permission::ModerateComments,
            Permission::ViewSystemLog,
        ] {
            assert!(!Role::Writer.can(p), "writer must not have {p:?}");
        }
    }

    #[test]
    fn request_id_rejects_unsafe_input() {
        assert!(RequestId::parse("8f2a1c3b9d-SIN").is_some());
        assert!(RequestId::parse("").is_none());
        assert!(RequestId::parse("a b").is_none());
        assert!(RequestId::parse(&"a".repeat(65)).is_none());
    }
}
