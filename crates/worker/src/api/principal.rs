//! Who is asking. One place turns request credentials into a [`Principal`]; routes only ask
//! it for a user or a permission. An `Authorization` header means an API token and nothing
//! else: the cookie is then ignored entirely (not read, renewed or cleared).

use plumblog_auth::api_token::{self, ApiTokenSecret};
use plumblog_auth::session::{self, SessionToken};
use plumblog_model::{Error, ErrorCode, Permission, Result, TokenScope};
use plumblog_store::{Proof, SessionStore, TokenStore, User, UserStore};
use plumblog_syslog::{Event, EventKind};
use worker::Request;

use super::Ctx;

pub enum Principal {
    Anonymous {
        /// Hash of a session cookie that was sent but did not resolve (expired, ended,
        /// disabled user), so sign-in can delete the stale row.
        stale_hash: Option<String>,
    },
    Session {
        user: User,
        token_hash: String,
    },
    /// `Authorization: Bearer plb_...`. Acts with `scope` capped by the user's current role.
    Token {
        user: User,
        token_id: i64,
        name: String,
        scope: TokenScope,
    },
}

impl Principal {
    /// The signed-in user.
    ///
    /// # Errors
    ///
    /// `Unauthenticated` for anonymous requests.
    pub fn user(&self) -> Result<&User> {
        match self {
            Self::Session { user, .. } | Self::Token { user, .. } => Ok(user),
            Self::Anonymous { .. } => {
                Err(Error::new(ErrorCode::Unauthenticated, "no valid session"))
            }
        }
    }

    /// The signed-in user of a browser session. Account and token management (every
    /// `/api/auth/*` route) is session only, so a leaked token cannot mint tokens, change the
    /// password or add a passkey.
    ///
    /// # Errors
    ///
    /// `Unauthenticated` for anonymous requests, `Forbidden` for an API token.
    pub fn session_user(&self) -> Result<&User> {
        match self {
            Self::Session { user, .. } => Ok(user),
            Self::Token { name, .. } => Err(Error::new(
                ErrorCode::Forbidden,
                format!("API token {name:?}: session-only route"),
            )),
            Self::Anonymous { .. } => self.user(),
        }
    }

    /// Any signed-in user, a read-scope token included: for routes that only read.
    ///
    /// # Errors
    ///
    /// `Unauthenticated` for anonymous requests.
    pub fn require_read(&self) -> Result<&User> {
        self.user()
    }

    /// The signed-in user, if their role (or the token's effective scope) holds `permission`.
    ///
    /// # Errors
    ///
    /// `Unauthenticated` for anonymous requests, `Forbidden` when the role or scope lacks it.
    pub fn require(&self, permission: Permission) -> Result<&User> {
        let user = self.user()?;
        match self {
            Self::Token { scope, .. } => {
                plumblog_auth::require_token(*scope, user.role, permission)?;
            }
            _ => plumblog_auth::require(user.role, permission)?,
        }
        Ok(user)
    }

    /// A system log event by this principal, saying how it acted: `via` = `session` or
    /// `token`, and for a token `client` = its name and id (names need not be unique; its own
    /// field, as "token <name>" in one value would be redacted as a secret).
    pub fn event(&self, ctx: &Ctx, kind: EventKind) -> Event {
        let actor = self.user().ok().map(|u| u.id);
        let e = ctx.event(kind, actor);
        match self {
            Self::Token { name, token_id, .. } => e
                .field("via", "token")
                .field("client", &format!("{name} #{token_id}")),
            Self::Session { .. } => e.field("via", "session"),
            Self::Anonymous { .. } => e,
        }
    }

    /// What this session request writes a new credential with ([`Proof::Session`]).
    ///
    /// # Errors
    ///
    /// As [`Self::session_user`].
    pub fn session_proof(&self) -> Result<Proof> {
        self.session_user()?;
        self.session_hash()
            .map(|h| Proof::Session(h.to_owned()))
            .ok_or_else(|| Error::new(ErrorCode::Unauthenticated, "no valid session"))
    }

    /// Hash of the session this request came with (valid or stale), for rotation.
    #[must_use]
    pub fn session_hash(&self) -> Option<&str> {
        match self {
            Self::Session { token_hash, .. } => Some(token_hash),
            Self::Anonymous { stale_hash } => stale_hash.as_deref(),
            Self::Token { .. } => None,
        }
    }
}

/// True when the request carries an `Authorization` header: it is then an API token request
/// (no `Origin` check, cookie ignored), valid or not.
#[must_use]
pub fn has_authorization(req: &Request) -> bool {
    req.headers().has("authorization").unwrap_or(false)
}

/// Resolves the principal and the `Set-Cookie` the gate wants to send: a renewal when less
/// than half the session lifetime remains, or a clearing cookie for a session that no longer
/// resolves. Never a cookie for a token request.
///
/// # Errors
///
/// `Unauthenticated` for an `Authorization` header that is not a valid, live API token;
/// storage and crypto errors pass through.
pub async fn resolve(ctx: &Ctx, req: &Request) -> Result<(Principal, Option<String>)> {
    let header = |name: &str| req.headers().get(name).ok().flatten();
    if has_authorization(req) {
        let value = header("authorization").unwrap_or_default();
        return Ok((from_api_token(ctx, &value).await?, None));
    }
    let anonymous = Principal::Anonymous { stale_hash: None };
    let Some(cookies) = header("cookie") else {
        return Ok((anonymous, None));
    };
    if !cookies.contains(session::COOKIE_NAME) {
        return Ok((anonymous, None));
    }
    let Some(token) = session::token_from_cookie_header(&cookies) else {
        // Present but malformed or sent twice: never guess; tell the browser to drop it.
        worker::console_warn!(
            "[{}] session cookie malformed or duplicated",
            ctx.request_id.as_str()
        );
        return Ok((anonymous, Some(session::clear_cookie())));
    };
    from_token(ctx, &token).await
}

/// The token principal for an `Authorization` value: one indexed read for the token (which
/// already excludes revoked, expired and disabled-user tokens) and one for the user's
/// current role. No write: tokens record no last use.
async fn from_api_token(ctx: &Ctx, value: &str) -> Result<Principal> {
    let unauthenticated = |why: &str| Error::new(ErrorCode::Unauthenticated, why.to_owned());
    // Never echo the header: it may be a real token with a typo.
    let secret = ApiTokenSecret::from_authorization(value)
        .ok_or_else(|| unauthenticated("Authorization: not `Bearer plb_<43 base64url>`"))?;
    let hash = api_token::hash_token(&ctx.crypto, &secret).await?;
    let found =
        ctx.store.find_token(&hash, ctx.now).await?.ok_or_else(|| {
            unauthenticated("API token unknown, revoked, expired or user disabled")
        })?;
    let user = match ctx.store.get_user(found.user_id).await {
        Ok(u) => u,
        Err(e) if e.code == ErrorCode::UserNotFound => {
            return Err(unauthenticated("API token user deleted"));
        }
        Err(e) => return Err(e),
    };
    // Disabled between the two reads.
    if user.disabled_at.is_some() {
        return Err(unauthenticated("API token user disabled"));
    }
    Ok(Principal::Token {
        user,
        token_id: found.id,
        name: found.name,
        scope: found.scope,
    })
}

async fn from_token(ctx: &Ctx, token: &SessionToken) -> Result<(Principal, Option<String>)> {
    let hash = session::hash_token(&ctx.crypto, token).await?;
    let Some(found) = ctx.store.find_session(&hash, ctx.now).await? else {
        let p = Principal::Anonymous {
            stale_hash: Some(hash),
        };
        return Ok((p, Some(session::clear_cookie())));
    };
    let user = match ctx.store.get_user(found.user_id).await {
        Ok(u) => Some(u),
        // Deleted between the two reads: the session is gone with it.
        Err(e) if e.code == ErrorCode::UserNotFound => None,
        Err(e) => return Err(e),
    };
    let Some(user) = user.filter(|u| u.disabled_at.is_none()) else {
        // find_session already filters disabled users; a second check costs nothing.
        let p = Principal::Anonymous {
            stale_hash: Some(hash),
        };
        return Ok((p, Some(session::clear_cookie())));
    };
    let mut cookie = None;
    if session::needs_renewal(found.expires_at, ctx.now) {
        let expires = session::expires_at(ctx.now);
        match ctx.store.renew_session(&hash, expires, ctx.now).await {
            Ok(true) => cookie = Some(session::set_cookie(token, expires, ctx.now)),
            // Ended between the read and the write (sign-out elsewhere): it stays valid for
            // this request only, as the read said.
            Ok(false) => {}
            // The session is still valid for its remaining time; renewing can wait.
            Err(e) => {
                worker::console_error!("[{}] session renewal failed: {e}", ctx.request_id.as_str());
            }
        }
    }
    Ok((
        Principal::Session {
            user,
            token_hash: hash,
        },
        cookie,
    ))
}
