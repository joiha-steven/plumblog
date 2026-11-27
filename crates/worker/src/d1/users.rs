use plumblog_model::{Error, ErrorCode, Result, Role, UserId};
use plumblog_store::{NewUser, ProfileUpdate, SiteStore, User, UserStore};

use super::{
    D1Store, Failure, Row, Stmt, bound, bump_if, guard, int, opt_int, opt_text, storage, text,
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
        ErrorCode::Conflict,
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

impl D1Store {
    fn not_last_owner(&self, name: &'static str, id: UserId) -> Result<Stmt> {
        guard(self, name, NOT_LAST_OWNER, &[bound(id.0), bound(id.0)])
    }

    /// The `set_role` batch for a non-owner `role`: the guard, then the write at index 1.
    /// Separate so `selftest` can run it after the owners changed under it.
    pub(super) fn demote_stmts(&self, id: UserId, role: Role, now: i64) -> Result<Vec<Stmt>> {
        Ok(vec![
            self.not_last_owner("set_role_guard", id)?,
            self.stmt(
                "set_role",
                "UPDATE users SET role = ?, updated_at = ? WHERE id = ?",
                &[text(role.as_str()), int(now)?, bound(id.0)],
            )?,
        ])
    }

    /// Runs `stmts` (a [`Self::not_last_owner`] guard first) and checks that statement
    /// `write_at` changed the user's row.
    async fn guarded_user_write(
        &self,
        what: &'static str,
        id: UserId,
        stmts: Vec<Stmt>,
        write_at: usize,
    ) -> Result<()> {
        let results = match self.batch_guarded(what, stmts).await {
            Ok(r) => r,
            Err(Failure::Guard) => return Err(last_owner(id)),
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

    async fn set_role(&self, id: UserId, role: Role, now: i64) -> Result<()> {
        if role == Role::Owner {
            return self
                .set_user_column("set_role", "role", text(role.as_str()), id, now)
                .await;
        }
        let stmts = self.demote_stmts(id, role, now)?;
        self.guarded_user_write("set_role", id, stmts, 1).await
    }

    async fn set_disabled(&self, id: UserId, disabled_at: Option<i64>, now: i64) -> Result<()> {
        let Some(at) = disabled_at else {
            return self
                .set_user_column("set_disabled", "disabled_at", opt_int(None)?, id, now)
                .await;
        };
        let stmts = vec![
            self.not_last_owner("set_disabled_guard", id)?,
            self.stmt(
                "set_disabled",
                "UPDATE users SET disabled_at = ?, updated_at = ? WHERE id = ?",
                &[int(at)?, int(now)?, bound(id.0)],
            )?,
            // Same transaction: a disabled account must not stay signed in.
            self.stmt(
                "set_disabled_sessions",
                "DELETE FROM sessions WHERE user_id = ?",
                &[bound(id.0)],
            )?,
        ];
        self.guarded_user_write("set_disabled", id, stmts, 1).await
    }

    async fn delete_user(&self, id: UserId) -> Result<()> {
        // The schema's ON DELETE rules do the cascades and SET NULLs, and posts.author_id
        // RESTRICT refuses the delete while they author posts.
        let stmts = vec![
            self.not_last_owner("delete_user_guard", id)?,
            self.stmt(
                "delete_user",
                "DELETE FROM users WHERE id = ?",
                &[bound(id.0)],
            )?,
        ];
        self.guarded_user_write("delete_user", id, stmts, 1)
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
