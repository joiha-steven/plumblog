//! Who is asking. One place turns request credentials into a [`Principal`]; routes only ask
//! it for a user or a permission, so API tokens (issue #10) add a variant here and no route
//! changes.

use plumblog_auth::session::{self, SessionToken};
use plumblog_model::{Error, ErrorCode, Permission, Result, Role};
use plumblog_store::{SessionStore, User, UserStore};
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
    // Issue #10: `Token { user, scope }` from `Authorization: Bearer`, role = the token scope.
}

impl Principal {
    /// The signed-in user.
    ///
    /// # Errors
    ///
    /// `Unauthenticated` for anonymous requests.
    pub fn user(&self) -> Result<&User> {
        match self {
            Self::Session { user, .. } => Ok(user),
            Self::Anonymous { .. } => {
                Err(Error::new(ErrorCode::Unauthenticated, "no valid session"))
            }
        }
    }

    fn role(&self) -> Result<Role> {
        self.user().map(|u| u.role)
    }

    /// The signed-in user, if their role holds `permission`.
    ///
    /// # Errors
    ///
    /// `Unauthenticated` for anonymous requests, `Forbidden` when the role lacks it.
    pub fn require(&self, permission: Permission) -> Result<&User> {
        plumblog_auth::require(self.role()?, permission)?;
        self.user()
    }

    /// Hash of the session this request came with (valid or stale), for rotation.
    #[must_use]
    pub fn session_hash(&self) -> Option<&str> {
        match self {
            Self::Session { token_hash, .. } => Some(token_hash),
            Self::Anonymous { stale_hash } => stale_hash.as_deref(),
        }
    }
}

/// Resolves the principal and the `Set-Cookie` the gate wants to send: a renewal when less
/// than half the session lifetime remains, or a clearing cookie for a session that no longer
/// resolves.
///
/// # Errors
///
/// `Unauthenticated` for credentials that are not supported yet (`Authorization`) or a
/// malformed or duplicated session cookie; storage and crypto errors pass through.
pub async fn resolve(ctx: &Ctx, req: &Request) -> Result<(Principal, Option<String>)> {
    let header = |name: &str| req.headers().get(name).ok().flatten();
    if header("authorization").is_some() {
        return Err(Error::new(
            ErrorCode::Unauthenticated,
            "Authorization header: API tokens are not supported yet (issue #10)",
        ));
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
