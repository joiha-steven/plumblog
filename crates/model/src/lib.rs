//! Shared types every other crate speaks: ids, errors, roles and permissions. No I/O.

use std::fmt;

pub mod api;
pub mod ts;

/// Engine version, taken from the workspace so it never drifts.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Declares an enum whose variants each carry a stable string code. The enum, `ALL` and
/// `as_str` come from one list, so adding a variant cannot leave `ALL` or a code behind.
#[macro_export]
macro_rules! stable_codes {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident { $($variant:ident => $code:literal),+ $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        $vis enum $name { $($variant),+ }

        impl $name {
            pub const ALL: &[$name] = &[$($name::$variant),+];

            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $($name::$variant => $code),+ }
            }

            /// The variant whose code is `code`; `None` for unknown codes (e.g. a stored row
            /// written by a newer version).
            #[must_use]
            pub fn parse(code: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|v| v.as_str() == code)
            }
        }
    };
}

stable_codes! {
    /// Stable error codes. The string form is a public contract: it appears in API responses,
    /// the system log and locale keys (`error.<code>`), so a code is never renamed once shipped.
    pub enum ErrorCode {
        PostNotFound => "post.not_found",
        UserNotFound => "user.not_found",
        Forbidden => "auth.forbidden",
        Unauthenticated => "auth.unauthenticated",
        InvalidInput => "input.invalid",
        Storage => "store.failed",
        Internal => "internal",
        NotFound => "store.not_found",
        Conflict => "store.conflict",
        InviteInvalid => "invite.invalid",
        InvalidCredentials => "auth.invalid_credentials",
        Locked => "auth.locked",
        TurnstileRequired => "auth.turnstile_required",
        PasskeyInvalid => "auth.passkey_invalid",
        SetupInvalid => "setup.invalid",
        RouteNotFound => "api.not_found",
        OriginMissing => "settings.origin_missing",
        ReauthRequired => "auth.reauth_required",
        BodyTooLarge => "api.body_too_large",
        LengthRequired => "api.length_required",
    }
}

/// One error type across crates. `detail` is for logs and agents, never shown to readers:
/// user-facing text comes from the locale key derived from `code`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error {
    pub code: ErrorCode,
    pub detail: String,
    /// The request field at fault, for input errors: API answers name it so a client can
    /// point at it.
    pub field: Option<String>,
}

impl Error {
    pub fn new(code: ErrorCode, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
            field: None,
        }
    }

    /// The same error, naming the request field at fault (the first one named wins).
    #[must_use]
    pub fn at(mut self, field: &str) -> Self {
        if self.field.is_none() {
            self.field = Some(field.to_owned());
        }
        self
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

    /// For work with no id to inherit and no working RNG, so an event never lacks one.
    #[must_use]
    pub fn unknown() -> Self {
        Self("unknown".to_owned())
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MediaId(pub i64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TagId(pub i64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SeriesId(pub i64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CommentId(pub i64);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RevisionId(pub i64);

stable_codes! {
    /// Codes are stored in D1 (`users.role`, `invites.role`); token scopes are [`TokenScope`].
    pub enum Role {
        Owner => "owner",
        Writer => "writer",
    }
}

stable_codes! {
    /// What a role (or a token scope, see [`TokenScope::can`]) may do.
    pub enum Permission {
        WritePost => "post.write",
        PublishPost => "post.publish",
        EditAnyPost => "post.edit_any",
        DeleteAnyPost => "post.delete_any",
        UploadMedia => "media.upload",
        CreateOwnToken => "token.create_own",
        ManageSettings => "settings.manage",
        ManageThemes => "theme.manage",
        ManagePlugins => "plugin.manage",
        ManageUsers => "user.manage",
        ModerateComments => "comment.moderate",
        ViewSystemLog => "syslog.view",
    }
}

impl Permission {
    /// The whole role model (docs/ARCHITECTURE.md, Users and auth). An exhaustive match, so a
    /// new permission does not compile until someone decides who holds it.
    #[must_use]
    pub const fn roles(self) -> &'static [Role] {
        const BOTH: &[Role] = &[Role::Owner, Role::Writer];
        const OWNER: &[Role] = &[Role::Owner];
        match self {
            Self::WritePost
            | Self::PublishPost
            | Self::EditAnyPost
            | Self::DeleteAnyPost
            | Self::UploadMedia
            | Self::CreateOwnToken => BOTH,
            Self::ManageSettings
            | Self::ManageThemes
            | Self::ManagePlugins
            | Self::ManageUsers
            | Self::ModerateComments
            | Self::ViewSystemLog => OWNER,
        }
    }
}

impl Role {
    #[must_use]
    pub fn can(self, permission: Permission) -> bool {
        permission.roles().contains(&self)
    }
}

stable_codes! {
    /// What an API token may do, stored in `api_tokens.scope`. Ordered `Read < Writer <
    /// Owner` (declaration order), so the effective scope is a plain `min`.
    pub enum TokenScope {
        Read => "read",
        Writer => "writer",
        Owner => "owner",
    }
}

impl PartialOrd for TokenScope {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TokenScope {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (*self as u8).cmp(&(*other as u8))
    }
}

impl From<Role> for TokenScope {
    fn from(role: Role) -> Self {
        match role {
            Role::Owner => Self::Owner,
            Role::Writer => Self::Writer,
        }
    }
}

impl TokenScope {
    /// What a token acts as right now: never more than its user's current role, so a demoted
    /// owner's owner token acts as writer without touching the token row.
    #[must_use]
    pub fn effective(self, role: Role) -> Self {
        self.min(Self::from(role))
    }

    /// `Read` holds no permission at all (every [`Permission`] is a write or an owner
    /// action); `Writer` and `Owner` hold exactly what the role of the same name holds.
    #[must_use]
    pub fn can(self, permission: Permission) -> bool {
        match self {
            Self::Read => false,
            Self::Writer => Role::Writer.can(permission),
            Self::Owner => Role::Owner.can(permission),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ErrorCode, Permission, RequestId, Role};

    /// Pins every shipped code: renaming one breaks API clients and stored log rows.
    #[test]
    fn error_codes_are_pinned() {
        let table: Vec<_> = ErrorCode::ALL.iter().map(|c| (*c, c.as_str())).collect();
        assert_eq!(
            table,
            [
                (ErrorCode::PostNotFound, "post.not_found"),
                (ErrorCode::UserNotFound, "user.not_found"),
                (ErrorCode::Forbidden, "auth.forbidden"),
                (ErrorCode::Unauthenticated, "auth.unauthenticated"),
                (ErrorCode::InvalidInput, "input.invalid"),
                (ErrorCode::Storage, "store.failed"),
                (ErrorCode::Internal, "internal"),
                (ErrorCode::NotFound, "store.not_found"),
                (ErrorCode::Conflict, "store.conflict"),
                (ErrorCode::InviteInvalid, "invite.invalid"),
                (ErrorCode::InvalidCredentials, "auth.invalid_credentials"),
                (ErrorCode::Locked, "auth.locked"),
                (ErrorCode::TurnstileRequired, "auth.turnstile_required"),
                (ErrorCode::PasskeyInvalid, "auth.passkey_invalid"),
                (ErrorCode::SetupInvalid, "setup.invalid"),
                (ErrorCode::RouteNotFound, "api.not_found"),
                (ErrorCode::OriginMissing, "settings.origin_missing"),
                (ErrorCode::ReauthRequired, "auth.reauth_required"),
                (ErrorCode::BodyTooLarge, "api.body_too_large"),
                (ErrorCode::LengthRequired, "api.length_required"),
            ]
        );
    }

    /// Role codes are stored in D1 rows.
    #[test]
    fn role_codes_are_pinned_and_parse() {
        assert_eq!(Role::Owner.as_str(), "owner");
        assert_eq!(Role::Writer.as_str(), "writer");
        for r in Role::ALL {
            assert_eq!(Role::parse(r.as_str()), Some(*r));
        }
        assert_eq!(Role::parse("Owner"), None);
        assert_eq!(
            ErrorCode::parse("store.conflict"),
            Some(ErrorCode::Conflict)
        );
        assert_eq!(ErrorCode::parse("nope"), None);
    }

    /// The full owner/writer table, so any change to who holds what is a visible test edit.
    #[test]
    fn permission_table_is_pinned() {
        let writer_only_lacks = [
            Permission::ManageSettings,
            Permission::ManageThemes,
            Permission::ManagePlugins,
            Permission::ManageUsers,
            Permission::ModerateComments,
            Permission::ViewSystemLog,
        ];
        for p in Permission::ALL {
            assert!(Role::Owner.can(*p), "owner must have {p:?}");
            assert_eq!(
                Role::Writer.can(*p),
                !writer_only_lacks.contains(p),
                "writer and {p:?}"
            );
        }
        assert_eq!(Permission::ALL.len(), 12);
    }

    /// Scope codes are stored in D1 rows; the order and the effective scope are the token
    /// security model, so every pair is pinned.
    #[test]
    fn token_scopes_are_pinned_ordered_and_capped_by_role() {
        use super::TokenScope::{self, Owner, Read, Writer};
        let codes: Vec<_> = TokenScope::ALL.iter().map(|s| s.as_str()).collect();
        assert_eq!(codes, ["read", "writer", "owner"]);
        for s in TokenScope::ALL {
            assert_eq!(TokenScope::parse(s.as_str()), Some(*s));
        }
        assert_eq!(TokenScope::parse("Read"), None);
        assert!(Read < Writer && Writer < Owner && Read < Owner);
        assert_eq!(TokenScope::from(Role::Owner), Owner);
        assert_eq!(TokenScope::from(Role::Writer), Writer);
        for (scope, role, want) in [
            (Read, Role::Owner, Read),
            (Read, Role::Writer, Read),
            (Writer, Role::Owner, Writer),
            (Writer, Role::Writer, Writer),
            (Owner, Role::Owner, Owner),
            (Owner, Role::Writer, Writer),
        ] {
            assert_eq!(scope.effective(role), want, "{scope:?} of a {role:?}");
        }
        for p in Permission::ALL {
            assert!(!Read.can(*p), "read must not hold {p:?}");
            assert_eq!(Writer.can(*p), Role::Writer.can(*p), "writer and {p:?}");
            assert_eq!(Owner.can(*p), Role::Owner.can(*p), "owner and {p:?}");
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
