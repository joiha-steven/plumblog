use plumblog_model::{Error, ErrorCode, Result, Role, UserId};
use plumblog_store::{NewUser, SiteStore, User, UserStore};

use super::{D1Store, Row, bound, int, map_error, message, opt_int, opt_text, storage, text};

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

    async fn update_user(&self, user: &User) -> Result<()> {
        let s = self.stmt(
            "update_user",
            "UPDATE users SET login = ?, slug = ?, display_name = ?, bio = ?, role = ?, \
             password_hash = ?, disabled_at = ?, updated_at = ? WHERE id = ?",
            &[
                text(&user.login),
                text(&user.slug),
                text(&user.display_name),
                text(&user.bio),
                text(user.role.as_str()),
                opt_text(user.password_hash.as_deref()),
                opt_int(user.disabled_at)?,
                int(user.updated_at)?,
                bound(user.id.0),
            ],
        )?;
        let r = self.exec(s).await?;
        if Self::changes("update_user", &r)? == 0 {
            return Err(user_not_found(user.id));
        }
        Ok(())
    }

    async fn delete_user(&self, id: UserId) -> Result<()> {
        // One statement: the schema's ON DELETE rules do the cascades and SET NULLs, and
        // posts.author_id RESTRICT refuses the delete while they author posts.
        let s = self.stmt(
            "delete_user",
            "DELETE FROM users WHERE id = ?",
            &[bound(id.0)],
        )?;
        let r = s.inner.run().await.map_err(|e| {
            let msg = message(&e);
            if msg.contains("FOREIGN KEY constraint failed") {
                Error::new(
                    ErrorCode::Conflict,
                    format!("user {} still authors posts; reassign them first", id.0),
                )
            } else {
                map_error("delete_user", &e)
            }
        })?;
        if Self::changes("delete_user", &r)? == 0 {
            return Err(user_not_found(id));
        }
        Ok(())
    }
}
