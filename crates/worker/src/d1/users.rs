use plumblog_model::{Error, ErrorCode, Result, Role, UserId};
use plumblog_store::{Admin, NewSession, NewUser, ProfileUpdate, SiteStore, User, UserStore};

use super::{
    D1Store, Failure, Row, Stmt, bound, bump, bump_if, guard, int, invalid, opt_text,
    proof_condition, proof_gone, proof_guard, storage, text,
};

pub(super) const USER_COLS: &str =
    "id, login, slug, display_name, bio, role, password_hash, disabled_at, created_at, updated_at";

pub(super) fn read_user(r: &mut Row) -> Result<User> {
    Ok(User {
        id: UserId(r.int()?),
        login: r.text()?,
        slug: r.text()?,
        display_name: r.text()?,
        bio: r.text()?,
        role: r.code("role", Role::parse)?,
        password_hash: r.opt_text()?,
        disabled_at: r.opt_int()?,
        created_at: r.int()?,
        updated_at: r.int()?,
    })
}

fn user_not_found(id: UserId) -> Error {
    Error::new(ErrorCode::UserNotFound, format!("user {} not found", id.0))
}

fn last_owner(id: UserId) -> Error {
    Error::new(
        ErrorCode::LastOwner,
        format!(
            "user {} is the last active owner; make another user owner first",
            id.0
        ),
    )
}

/// True unless the user is an active owner and no other active owner exists. Inside the
/// batch, so two requests demoting the only two owners at once cannot both pass.
const NOT_LAST_OWNER: &str = "NOT EXISTS (SELECT 1 FROM users WHERE id = ? AND role = 'owner' \
     AND disabled_at IS NULL) \
     OR EXISTS (SELECT 1 FROM users WHERE id <> ? AND role = 'owner' AND disabled_at IS NULL)";

/// The acting user is an active owner (with its proof: [`Admin`]).
const ACTIVE_OWNER: &str =
    "EXISTS (SELECT 1 FROM users WHERE id = ? AND role = 'owner' AND disabled_at IS NULL)";

impl D1Store {
    /// A guard that aborts its batch unless `by`'s proof holds and `by` is an active owner,
    /// so a request authorized before its owner was demoted or disabled writes nothing.
    pub(super) fn admin_guard(&self, by: &Admin, now: i64) -> Result<Stmt> {
        #[cfg(feature = "contract-tests")]
        if by.proof == plumblog_store::Proof::Unchecked {
            return guard(self, "admin_guard", "1", &[]);
        }
        let (proof, mut params) = proof_condition(by.id, &by.proof, now)?;
        params.push(bound(by.id.0));
        guard(
            self,
            "admin_guard",
            &format!("({proof}) AND {ACTIVE_OWNER}"),
            &params,
        )
    }

    /// After a guard fired: why [`Self::admin_guard`] refused `by`, or `None` if it holds
    /// (another guard of the batch fired). A read after the failed write, so only for the
    /// answer and the log; nothing was written.
    pub(super) async fn admin_refusal(&self, by: &Admin, now: i64) -> Result<Option<Error>> {
        #[cfg(feature = "contract-tests")]
        if by.proof == plumblog_store::Proof::Unchecked {
            return Ok(None);
        }
        if !self.proof_holds(by.id, &by.proof, now).await? {
            return Ok(Some(proof_gone()));
        }
        let user = match self.get_user(by.id).await {
            Ok(u) => Some(u),
            Err(e) if e.code == ErrorCode::UserNotFound => None,
            Err(e) => return Err(e),
        };
        Ok(by.refusal(user.as_ref()))
    }

    /// For a batch whose only guard is [`Self::admin_guard`]: the refusal, always an error.
    pub(super) async fn admin_refused(&self, by: &Admin, now: i64) -> Result<Error> {
        Ok(self.admin_refusal(by, now).await?.unwrap_or_else(|| {
            Error::new(
                ErrorCode::Forbidden,
                format!(
                    "acting user {} failed the check in the write, passed it after",
                    by.id.0
                ),
            )
        }))
    }

    fn not_last_owner(&self, name: &'static str, id: UserId) -> Result<Stmt> {
        guard(self, name, NOT_LAST_OWNER, &[bound(id.0), bound(id.0)])
    }

    /// The `set_role` batch for a non-owner `role`: the guards, then the write at index 2.
    /// Separate so `selftest` can run it after the owners changed under it.
    pub(super) fn demote_stmts(
        &self,
        by: &Admin,
        id: UserId,
        role: Role,
        now: i64,
    ) -> Result<Vec<Stmt>> {
        Ok(vec![
            self.admin_guard(by, now)?,
            self.not_last_owner("set_role_guard", id)?,
            self.role_stmt(id, role, now)?,
            self.drop_unused_invites("set_role_invites", id)?,
        ])
    }

    /// A demoted or disabled owner's unused invites go in the same batch: a link minted with
    /// a stolen session must not outlive that owner's access.
    fn drop_unused_invites(&self, name: &'static str, id: UserId) -> Result<Stmt> {
        self.stmt(
            name,
            "DELETE FROM invites WHERE created_by = ? AND used_at IS NULL",
            &[bound(id.0)],
        )
    }

    fn role_stmt(&self, id: UserId, role: Role, now: i64) -> Result<Stmt> {
        self.stmt(
            "set_role",
            "UPDATE users SET role = ?, updated_at = ? WHERE id = ?",
            &[text(role.as_str()), int(now)?, bound(id.0)],
        )
    }

    /// Runs `stmts` (an [`Self::admin_guard`] for `by` first, then maybe a
    /// [`Self::not_last_owner`] guard) and checks that statement `write_at` changed the
    /// user's row.
    async fn guarded_user_write(
        &self,
        what: &'static str,
        by: &Admin,
        id: UserId,
        stmts: Vec<Stmt>,
        write_at: usize,
        now: i64,
    ) -> Result<()> {
        let results = match self.batch_guarded(what, stmts).await {
            Ok(r) => r,
            // D1 does not say which guard fired; the actor's is told apart by a read.
            Err(Failure::Guard) => {
                return Err(self
                    .admin_refusal(by, now)
                    .await?
                    .unwrap_or_else(|| last_owner(id)));
            }
            Err(Failure::Error(e)) => return Err(e),
        };
        if Self::changes(what, &results[write_at])? == 0 {
            return Err(user_not_found(id));
        }
        Ok(())
    }

    /// One `UPDATE users SET {set}, updated_at = ? WHERE id = ?`; `UserNotFound` if absent.
    async fn set_user_column(
        &self,
        name: &'static str,
        set: &str,
        value: worker::wasm_bindgen::JsValue,
        id: UserId,
        now: i64,
    ) -> Result<()> {
        let s = self.stmt(
            name,
            &format!("UPDATE users SET {set} = ?, updated_at = ? WHERE id = ?"),
            &[value, int(now)?, bound(id.0)],
        )?;
        let r = self.exec(s).await?;
        if Self::changes(name, &r)? == 0 {
            return Err(user_not_found(id));
        }
        Ok(())
    }
}

impl SiteStore for D1Store {
    // 1 query.
    async fn content_version(&self) -> Result<i64> {
        let s = self.stmt(
            "content_version",
            "SELECT content_version FROM site WHERE id = 1",
            &[],
        )?;
        self.row(s)
            .await?
            .ok_or_else(|| storage("content_version", "site row is missing"))?
            .int()
    }
}

impl UserStore for D1Store {
    async fn create_user(&self, user: NewUser) -> Result<UserId> {
        let s = self.stmt(
            "create_user",
            "INSERT INTO users (login, slug, display_name, bio, role, password_hash, \
             disabled_at, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, NULL, ?, ?)",
            &[
                text(&user.login),
                text(&user.slug),
                text(&user.display_name),
                text(&user.bio),
                text(user.role.as_str()),
                opt_text(user.password_hash.as_deref()),
                int(user.created_at)?,
                int(user.created_at)?,
            ],
        )?;
        let r = self.exec(s).await?;
        Self::last_row_id("create_user", &r).map(UserId)
    }

    // 1 query.
    async fn get_user(&self, id: UserId) -> Result<User> {
        let s = self.stmt(
            "get_user",
            &format!("SELECT {USER_COLS} FROM users WHERE id = ?"),
            &[bound(id.0)],
        )?;
        match self.row(s).await? {
            Some(mut r) => read_user(&mut r),
            None => Err(user_not_found(id)),
        }
    }

    // 1 query. `login` is COLLATE NOCASE, so `=` ignores ASCII case like the memory store.
    async fn find_user_by_login(&self, login: &str) -> Result<Option<User>> {
        let s = self.stmt(
            "find_user_by_login",
            &format!("SELECT {USER_COLS} FROM users WHERE login = ?"),
            &[text(login)],
        )?;
        self.row(s)
            .await?
            .map(|mut r| read_user(&mut r))
            .transpose()
    }

    // 1 query.
    async fn find_user_by_slug(&self, slug: &str) -> Result<Option<User>> {
        let s = self.stmt(
            "find_user_by_slug",
            &format!("SELECT {USER_COLS} FROM users WHERE slug = ?"),
            &[text(slug)],
        )?;
        self.row(s)
            .await?
            .map(|mut r| read_user(&mut r))
            .transpose()
    }

    // 1 query.
    async fn list_users(&self) -> Result<Vec<User>> {
        let s = self.stmt(
            "list_users",
            &format!("SELECT {USER_COLS} FROM users ORDER BY id"),
            &[],
        )?;
        self.rows(s).await?.iter_mut().map(read_user).collect()
    }

    // 1 query: a primary-key range (`reader_sql::users_by_id`), one row past the page.
    async fn users_by_id(&self, after: Option<UserId>, limit_n: u32) -> Result<(Vec<User>, bool)> {
        let keep = plumblog_store::page_len(limit_n);
        let s = self.stmt(
            "users_by_id",
            &super::reader_sql::users_by_id(),
            &[
                bound(after.map_or(0, |u| u.0)),
                bound(i64::try_from(keep).unwrap_or(0) + 1),
            ],
        )?;
        let mut rows = self
            .rows(s)
            .await?
            .iter_mut()
            .map(read_user)
            .collect::<Result<Vec<_>>>()?;
        let more = rows.len() > keep;
        rows.truncate(keep);
        Ok((rows, more))
    }

    async fn update_profile(&self, id: UserId, profile: ProfileUpdate, now: i64) -> Result<()> {
        let s = self.stmt(
            "update_profile",
            "UPDATE users SET login = ?, slug = ?, display_name = ?, bio = ?, updated_at = ? \
             WHERE id = ?",
            &[
                text(&profile.login),
                text(&profile.slug),
                text(&profile.display_name),
                text(&profile.bio),
                int(now)?,
                bound(id.0),
            ],
        )?;
        // Bump only if the user exists; a conflict rolls back both.
        let bump = bump_if(
            self,
            "EXISTS (SELECT 1 FROM users WHERE id = ?)",
            &[bound(id.0)],
        )?;
        let results = self.batch("update_profile", vec![bump, s]).await?;
        if Self::changes("update_profile", &results[1])? == 0 {
            return Err(user_not_found(id));
        }
        Ok(())
    }

    async fn set_password(&self, id: UserId, hash: Option<String>, now: i64) -> Result<()> {
        self.set_user_column(
            "set_password",
            "password_hash",
            opt_text(hash.as_deref()),
            id,
            now,
        )
        .await
    }

    async fn change_password(
        &self,
        id: UserId,
        hash: Option<String>,
        profile: Option<ProfileUpdate>,
        session: NewSession,
        now: i64,
    ) -> Result<i64> {
        if session.user_id != id {
            return Err(invalid(format!(
                "session for user {} given for user {}",
                session.user_id.0, id.0
            )));
        }
        // Passkey-only (`hash = None`): the account must still have no password when the
        // batch runs, or an owner reset racing this change would be left unusable.
        let condition = if hash.is_some() {
            "EXISTS (SELECT 1 FROM users WHERE id = ?)"
        } else {
            "EXISTS (SELECT 1 FROM users WHERE id = ? AND password_hash IS NULL)"
        };
        let mut stmts = vec![
            proof_guard(self, id, &session.proof, session.created_at)?,
            guard(self, "change_password_guard", condition, &[bound(id.0)])?,
        ];
        let write = if let Some(p) = &profile {
            stmts.push(bump(self)?);
            self.stmt(
                "change_password",
                "UPDATE users SET login = ?, slug = ?, display_name = ?, bio = ?, \
                 password_hash = ?, updated_at = ? WHERE id = ?",
                &[
                    text(&p.login),
                    text(&p.slug),
                    text(&p.display_name),
                    text(&p.bio),
                    opt_text(hash.as_deref()),
                    int(now)?,
                    bound(id.0),
                ],
            )?
        } else {
            self.stmt(
                "change_password",
                "UPDATE users SET password_hash = ?, updated_at = ? WHERE id = ?",
                &[opt_text(hash.as_deref()), int(now)?, bound(id.0)],
            )?
        };
        stmts.push(write);
        stmts.push(self.stmt(
            "change_password_sessions",
            "DELETE FROM sessions WHERE user_id = ?",
            &[bound(id.0)],
        )?);
        stmts.push(self.stmt(
            "change_password_session",
            "INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES (?, ?, ?, ?)",
            &[
                text(&session.token_hash),
                bound(id.0),
                int(session.created_at)?,
                int(session.expires_at)?,
            ],
        )?);
        let results = match self.batch_guarded("change_password", stmts).await {
            Ok(r) => r,
            Err(Failure::Guard)
                if !self
                    .proof_holds(id, &session.proof, session.created_at)
                    .await? =>
            {
                self.get_user(id).await?;
                return Err(proof_gone());
            }
            Err(Failure::Guard) if hash.is_none() => {
                // Which half of the guard failed: a missing user, or one with a password.
                self.get_user(id).await?;
                return Err(Error::new(
                    ErrorCode::Conflict,
                    "passkey-only change, but the account has a password",
                ));
            }
            Err(Failure::Guard) => return Err(user_not_found(id)),
            Err(Failure::Error(e)) => return Err(e),
        };
        let last = results
            .last()
            .ok_or_else(|| storage("change_password", "batch returned no results"))?;
        Self::last_row_id("change_password_session", last)
    }

    async fn set_role(&self, by: &Admin, id: UserId, role: Role, now: i64) -> Result<()> {
        if role == Role::Owner {
            let stmts = vec![self.admin_guard(by, now)?, self.role_stmt(id, role, now)?];
            return self
                .guarded_user_write("set_role", by, id, stmts, 1, now)
                .await;
        }
        let stmts = self.demote_stmts(by, id, role, now)?;
        self.guarded_user_write("set_role", by, id, stmts, 2, now)
            .await
    }

    async fn set_disabled(
        &self,
        by: &Admin,
        id: UserId,
        disabled_at: Option<i64>,
        now: i64,
    ) -> Result<()> {
        let Some(at) = disabled_at else {
            let stmts = vec![
                self.admin_guard(by, now)?,
                self.stmt(
                    "set_disabled",
                    "UPDATE users SET disabled_at = NULL, updated_at = ? WHERE id = ?",
                    &[int(now)?, bound(id.0)],
                )?,
            ];
            return self
                .guarded_user_write("set_disabled", by, id, stmts, 1, now)
                .await;
        };
        let stmts = vec![
            self.admin_guard(by, now)?,
            self.not_last_owner("set_disabled_guard", id)?,
            self.stmt(
                "set_disabled",
                "UPDATE users SET disabled_at = ?, updated_at = ? WHERE id = ?",
                &[int(at)?, int(now)?, bound(id.0)],
            )?,
            // Same transaction: a disabled account must not stay signed in, nor keep tokens
            // that come back when it is re-enabled.
            self.stmt(
                "set_disabled_sessions",
                "DELETE FROM sessions WHERE user_id = ?",
                &[bound(id.0)],
            )?,
            self.drop_unused_invites("set_disabled_invites", id)?,
            self.stmt(
                "set_disabled_tokens",
                "UPDATE api_tokens SET revoked_at = ? WHERE user_id = ? AND revoked_at IS NULL",
                &[int(now)?, bound(id.0)],
            )?,
        ];
        self.guarded_user_write("set_disabled", by, id, stmts, 2, now)
            .await
    }

    async fn delete_user(&self, by: &Admin, id: UserId, now: i64) -> Result<()> {
        // The schema's ON DELETE rules do the cascades and SET NULLs, and posts.author_id
        // RESTRICT refuses the delete while they author posts.
        let stmts = vec![
            self.admin_guard(by, now)?,
            self.not_last_owner("delete_user_guard", id)?,
            self.stmt(
                "delete_user",
                "DELETE FROM users WHERE id = ?",
                &[bound(id.0)],
            )?,
        ];
        self.guarded_user_write("delete_user", by, id, stmts, 2, now)
            .await
            .map_err(|e| {
                if e.detail.contains("FOREIGN KEY constraint failed") {
                    Error::new(
                        ErrorCode::Conflict,
                        format!("user {} still authors posts; reassign them first", id.0),
                    )
                } else {
                    e
                }
            })
    }
}
