use plumblog_model::{Error, ErrorCode, Result, Role, TokenScope, UserId};
use plumblog_store::{
    Admin, ApiToken, Invite, InviteStore, NewInvite, NewPasskey, NewSession, NewToken, NewUser,
    Passkey, PasskeyStore, Session, SessionStore, TokenStore,
};

use super::{D1Store, Failure, Row, Stmt, bound, guard, int, not_found, opt_int, opt_text, text};

const SESSION_COLS: &str = "id, token_hash, user_id, created_at, expires_at";
const PASSKEY_COLS: &str =
    "id, user_id, credential_id, public_key, sign_count, name, created_at, last_used_at";
/// The row's `user_id` is not disabled; part of the one lookup query, not a second read.
const USER_ACTIVE: &str =
    "EXISTS (SELECT 1 FROM users u WHERE u.id = user_id AND u.disabled_at IS NULL)";
const TOKEN_COLS: &str = "id, user_id, name, token_hash, scope, created_at, expires_at, revoked_at";
const INVITE_COLS: &str =
    "id, code_hash, role, created_by, created_at, expires_at, used_at, used_by";

fn read_session(r: &mut Row) -> Result<Session> {
    Ok(Session {
        id: r.int()?,
        token_hash: r.text()?,
        user_id: UserId(r.int()?),
        created_at: r.int()?,
        expires_at: r.int()?,
    })
}

fn read_passkey(r: &mut Row) -> Result<Passkey> {
    Ok(Passkey {
        id: r.int()?,
        user_id: UserId(r.int()?),
        credential_id: r.text()?,
        public_key: r.text()?,
        sign_count: r.int()?,
        name: r.text()?,
        created_at: r.int()?,
        last_used_at: r.opt_int()?,
    })
}

fn read_token(r: &mut Row) -> Result<ApiToken> {
    Ok(ApiToken {
        id: r.int()?,
        user_id: UserId(r.int()?),
        name: r.text()?,
        token_hash: r.text()?,
        scope: r.code("scope", TokenScope::parse)?,
        created_at: r.int()?,
        expires_at: r.opt_int()?,
        revoked_at: r.opt_int()?,
    })
}

fn read_invite(r: &mut Row) -> Result<Invite> {
    Ok(Invite {
        id: r.int()?,
        code_hash: r.text()?,
        role: r.code("role", Role::parse)?,
        created_by: UserId(r.int()?),
        created_at: r.int()?,
        expires_at: r.int()?,
        used_at: r.opt_int()?,
        used_by: r.opt_int()?.map(UserId),
    })
}

impl SessionStore for D1Store {
    async fn create_session(&self, session: NewSession) -> Result<i64> {
        let s = self.stmt(
            "create_session",
            "INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES (?, ?, ?, ?)",
            &[
                text(&session.token_hash),
                int(session.user_id.0)?,
                int(session.created_at)?,
                int(session.expires_at)?,
            ],
        )?;
        self.insert_proven(
            "create_session",
            session.user_id,
            &session.proof,
            session.created_at,
            s,
        )
        .await
    }

    // 1 query (every authenticated request).
    async fn find_session(&self, token_hash: &str, now: i64) -> Result<Option<Session>> {
        let s = self.stmt(
            "find_session",
            &format!(
                "SELECT {SESSION_COLS} FROM sessions WHERE token_hash = ? AND expires_at > ? \
                 AND {USER_ACTIVE}"
            ),
            &[text(token_hash), bound(now)],
        )?;
        self.row(s)
            .await?
            .map(|mut r| read_session(&mut r))
            .transpose()
    }

    async fn delete_session(&self, token_hash: &str) -> Result<()> {
        let s = self.stmt(
            "delete_session",
            "DELETE FROM sessions WHERE token_hash = ?",
            &[text(token_hash)],
        )?;
        self.exec(s).await.map(drop)
    }

    async fn renew_session(&self, token_hash: &str, expires_at: i64, now: i64) -> Result<bool> {
        let s = self.stmt(
            "renew_session",
            "UPDATE sessions SET expires_at = ? WHERE token_hash = ? AND expires_at > ?",
            &[int(expires_at)?, text(token_hash), bound(now)],
        )?;
        let r = self.exec(s).await?;
        Ok(Self::changes("renew_session", &r)? > 0)
    }

    async fn delete_user_sessions(&self, user: UserId) -> Result<u64> {
        let s = self.stmt(
            "delete_user_sessions",
            "DELETE FROM sessions WHERE user_id = ?",
            &[bound(user.0)],
        )?;
        let r = self.exec(s).await?;
        Self::changes("delete_user_sessions", &r)
    }

    async fn delete_expired_sessions(&self, now: i64) -> Result<u64> {
        let s = self.stmt(
            "delete_expired_sessions",
            "DELETE FROM sessions WHERE expires_at <= ?",
            &[bound(now)],
        )?;
        let r = self.exec(s).await?;
        Self::changes("delete_expired_sessions", &r)
    }
}

impl PasskeyStore for D1Store {
    async fn add_passkey(&self, passkey: NewPasskey) -> Result<i64> {
        let s = self.stmt(
            "add_passkey",
            "INSERT INTO passkeys (user_id, credential_id, public_key, sign_count, name, \
             created_at, last_used_at) VALUES (?, ?, ?, ?, ?, ?, NULL)",
            &[
                int(passkey.user_id.0)?,
                text(&passkey.credential_id),
                text(&passkey.public_key),
                int(passkey.sign_count)?,
                text(&passkey.name),
                int(passkey.created_at)?,
            ],
        )?;
        self.insert_proven(
            "add_passkey",
            passkey.user_id,
            &passkey.proof,
            passkey.created_at,
            s,
        )
        .await
    }

    // 1 query.
    async fn find_passkey(&self, credential_id: &str) -> Result<Option<Passkey>> {
        let s = self.stmt(
            "find_passkey",
            &format!("SELECT {PASSKEY_COLS} FROM passkeys WHERE credential_id = ?"),
            &[text(credential_id)],
        )?;
        self.row(s)
            .await?
            .map(|mut r| read_passkey(&mut r))
            .transpose()
    }

    // 1 query.
    async fn list_passkeys(&self, user: UserId) -> Result<Vec<Passkey>> {
        let s = self.stmt(
            "list_passkeys",
            &format!("SELECT {PASSKEY_COLS} FROM passkeys WHERE user_id = ? ORDER BY id"),
            &[bound(user.0)],
        )?;
        self.rows(s).await?.iter_mut().map(read_passkey).collect()
    }

    async fn touch_passkey(&self, id: i64, sign_count: i64, at: i64) -> Result<bool> {
        // The read and the write share one transaction, so the counter compared is the one
        // replaced even when two sign-ins with the same passkey race. A refused sign-in
        // writes nothing.
        let read = self.stmt(
            "touch_passkey_read",
            "SELECT sign_count FROM passkeys WHERE id = ?",
            &[bound(id)],
        )?;
        let write = self.stmt(
            "touch_passkey",
            "UPDATE passkeys SET sign_count = ?1, last_used_at = ?2 \
             WHERE id = ?3 AND (?1 > sign_count OR (?1 = 0 AND sign_count = 0))",
            &[int(sign_count)?, int(at)?, bound(id)],
        )?;
        let results = self.batch("touch_passkey", vec![read, write]).await?;
        let old = Self::batch_ints("touch_passkey_read", &results[0])?
            .first()
            .and_then(|r| r.get("sign_count").copied());
        let Some(old) = old else {
            return Err(not_found("passkey", id));
        };
        Ok(sign_count > old || (sign_count == 0 && old == 0))
    }

    async fn delete_own_passkey(&self, user: UserId, id: i64) -> Result<()> {
        let stmts = self.delete_own_passkey_stmts(user, id)?;
        self.run_delete_own_passkey(id, stmts).await
    }

    async fn delete_passkey(&self, id: i64) -> Result<()> {
        let s = self.stmt(
            "delete_passkey",
            "DELETE FROM passkeys WHERE id = ?",
            &[bound(id)],
        )?;
        let r = self.exec(s).await?;
        if Self::changes("delete_passkey", &r)? == 0 {
            return Err(not_found("passkey", id));
        }
        Ok(())
    }
}

impl D1Store {
    /// The batch of [`PasskeyStore::delete_own_passkey`]: a guard that the passkey is not the
    /// last one of a passwordless owner, then the delete. Separate so the D1 self-test can
    /// prepare it, change the data, and run it late.
    pub(super) fn delete_own_passkey_stmts(&self, user: UserId, id: i64) -> Result<Vec<Stmt>> {
        Ok(vec![
            guard(
                self,
                "delete_own_passkey_guard",
                "NOT EXISTS (SELECT 1 FROM users u WHERE u.id = ? AND u.password_hash IS NULL \
                 AND EXISTS (SELECT 1 FROM passkeys WHERE id = ? AND user_id = u.id) \
                 AND (SELECT COUNT(*) FROM passkeys WHERE user_id = u.id) = 1)",
                &[bound(user.0), bound(id)],
            )?,
            self.stmt(
                "delete_own_passkey",
                "DELETE FROM passkeys WHERE id = ? AND user_id = ?",
                &[bound(id), bound(user.0)],
            )?,
        ])
    }

    /// Runs a [`Self::delete_own_passkey_stmts`] batch and names its outcome.
    pub(super) async fn run_delete_own_passkey(&self, id: i64, stmts: Vec<Stmt>) -> Result<()> {
        let results = match self.batch_guarded("delete_own_passkey", stmts).await {
            Ok(r) => r,
            Err(Failure::Guard) => {
                return Err(Error::new(
                    ErrorCode::Conflict,
                    "last passkey of an account without a password",
                ));
            }
            Err(Failure::Error(e)) => return Err(e),
        };
        let deleted = results
            .get(1)
            .map(|r| Self::changes("delete_own_passkey", r))
            .transpose()?
            .unwrap_or(0);
        if deleted == 0 {
            return Err(not_found("passkey", id));
        }
        Ok(())
    }
}

impl TokenStore for D1Store {
    async fn create_token(&self, token: NewToken) -> Result<i64> {
        let s = self.stmt(
            "create_token",
            "INSERT INTO api_tokens (user_id, name, token_hash, scope, created_at, expires_at, \
             revoked_at) VALUES (?, ?, ?, ?, ?, ?, NULL)",
            &[
                int(token.user_id.0)?,
                text(&token.name),
                text(&token.token_hash),
                text(token.scope.as_str()),
                int(token.created_at)?,
                opt_int(token.expires_at)?,
            ],
        )?;
        self.insert_proven(
            "create_token",
            token.user_id,
            &token.proof,
            token.created_at,
            s,
        )
        .await
    }

    // 1 query (every API call).
    async fn find_token(&self, token_hash: &str, now: i64) -> Result<Option<ApiToken>> {
        let s = self.stmt(
            "find_token",
            &format!(
                "SELECT {TOKEN_COLS} FROM api_tokens WHERE token_hash = ? AND revoked_at IS NULL \
                 AND (expires_at IS NULL OR expires_at > ?) AND {USER_ACTIVE}"
            ),
            &[text(token_hash), bound(now)],
        )?;
        self.row(s)
            .await?
            .map(|mut r| read_token(&mut r))
            .transpose()
    }

    // 1 query.
    async fn list_tokens(&self, user: UserId) -> Result<Vec<ApiToken>> {
        let s = self.stmt(
            "list_tokens",
            &format!("SELECT {TOKEN_COLS} FROM api_tokens WHERE user_id = ? ORDER BY id"),
            &[bound(user.0)],
        )?;
        self.rows(s).await?.iter_mut().map(read_token).collect()
    }

    async fn revoke_token(&self, id: i64, at: i64) -> Result<()> {
        // COALESCE keeps the first revocation time.
        let s = self.stmt(
            "revoke_token",
            "UPDATE api_tokens SET revoked_at = COALESCE(revoked_at, ?) WHERE id = ?",
            &[int(at)?, bound(id)],
        )?;
        let r = self.exec(s).await?;
        if Self::changes("revoke_token", &r)? == 0 {
            return Err(not_found("api token", id));
        }
        Ok(())
    }
}

impl InviteStore for D1Store {
    async fn create_invite(&self, invite: NewInvite) -> Result<i64> {
        let s = self.stmt(
            "create_invite",
            "INSERT INTO invites (code_hash, role, created_by, created_at, expires_at, used_at, \
             used_by) VALUES (?, ?, ?, ?, ?, NULL, NULL)",
            &[
                text(&invite.code_hash),
                text(invite.role.as_str()),
                int(invite.created_by.0)?,
                int(invite.created_at)?,
                int(invite.expires_at)?,
            ],
        )?;
        // Like `insert_proven`, and the creator must still be an active owner when it runs.
        let by = Admin {
            id: invite.created_by,
            proof: invite.proof,
        };
        let g = self.admin_guard(&by, invite.created_at)?;
        match self.batch_guarded("create_invite", vec![g, s]).await {
            Ok(r) => Self::last_row_id("create_invite", &r[1]),
            Err(Failure::Guard) => Err(self.admin_refused(&by, invite.created_at).await?),
            Err(Failure::Error(e)) => Err(e),
        }
    }

    // 1 query.
    async fn list_invites(&self) -> Result<Vec<Invite>> {
        let s = self.stmt(
            "list_invites",
            &format!("SELECT {INVITE_COLS} FROM invites ORDER BY created_at DESC, id DESC"),
            &[],
        )?;
        self.rows(s).await?.iter_mut().map(read_invite).collect()
    }

    // 1 query.
    async fn list_unused_invites(&self, limit: u32) -> Result<Vec<Invite>> {
        let s = self.stmt(
            "list_unused_invites",
            &format!(
                "SELECT {INVITE_COLS} FROM invites WHERE used_at IS NULL \
                 ORDER BY created_at DESC, id DESC LIMIT ?"
            ),
            &[bound(i64::from(limit))],
        )?;
        self.rows(s).await?.iter_mut().map(read_invite).collect()
    }

    async fn delete_invite(&self, by: &Admin, id: i64, now: i64) -> Result<()> {
        let s = self.stmt(
            "delete_invite",
            // One statement: a used invite (the record of how its user joined) is never
            // deleted, even if it is accepted while this runs.
            "DELETE FROM invites WHERE id = ? AND used_at IS NULL",
            &[bound(id)],
        )?;
        let g = self.admin_guard(by, now)?;
        let r = match self.batch_guarded("delete_invite", vec![g, s]).await {
            Ok(r) => r,
            Err(Failure::Guard) => return Err(self.admin_refused(by, now).await?),
            Err(Failure::Error(e)) => return Err(e),
        };
        if Self::changes("delete_invite", &r[1])? == 0 {
            return Err(not_found("unused invite", id));
        }
        Ok(())
    }

    async fn accept_invite(&self, code_hash: &str, user: NewUser, now: i64) -> Result<UserId> {
        // One batch. The insert copies the role from the invite and only happens if the
        // invite is usable at that moment; the update repeats the same condition (the insert
        // does not change it), so both happen or neither does. Two requests racing for one
        // invite are serialised by D1: the second finds it used and inserts nothing. A login
        // or slug conflict fails the insert and rolls back the whole batch.
        // Its creator must still be an active owner: an invite outlives no owner's access,
        // whatever path demoted or disabled them.
        let usable = "code_hash = ? AND used_at IS NULL AND expires_at > ? AND EXISTS \
             (SELECT 1 FROM users c WHERE c.id = invites.created_by AND c.role = 'owner' \
             AND c.disabled_at IS NULL)";
        let insert = self.stmt(
            "accept_invite_user",
            &format!(
                "INSERT INTO users (login, slug, display_name, bio, role, password_hash, \
                 disabled_at, created_at, updated_at) \
                 SELECT ?, ?, ?, ?, role, ?, NULL, ?, ? FROM invites WHERE {usable}"
            ),
            &[
                text(&user.login),
                text(&user.slug),
                text(&user.display_name),
                text(&user.bio),
                opt_text(user.password_hash.as_deref()),
                int(user.created_at)?,
                int(user.created_at)?,
                text(code_hash),
                bound(now),
            ],
        )?;
        let mark = self.stmt(
            "accept_invite_mark",
            &format!(
                "UPDATE invites SET used_at = ?, \
                 used_by = (SELECT id FROM users WHERE login = ?) WHERE {usable}"
            ),
            // The new user by its unique login: D1 does not promise `sqlite_sequence`.
            &[int(now)?, text(&user.login), text(code_hash), bound(now)],
        )?;
        let results = self.batch("accept_invite", vec![insert, mark]).await?;
        if Self::changes("accept_invite_user", &results[0])? != 1 {
            return Err(Error::new(
                ErrorCode::InviteInvalid,
                "invite is unknown, used or expired",
            ));
        }
        Self::last_row_id("accept_invite_user", &results[0]).map(UserId)
    }
}
