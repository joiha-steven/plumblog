use plumblog_model::{Result, UserId};

use super::{MemoryStore, State, conflict, next_seq, user_not_found};
use crate::{NewUser, SiteStore, User, UserStore};

impl State {
    /// `users.login` is `NOCASE` (ASCII only) and `users.slug` is unique.
    fn check_user_unique(&self, login: &str, slug: &str, except: Option<UserId>) -> Result<()> {
        for u in self.users.values().filter(|u| Some(u.id) != except) {
            if u.login.eq_ignore_ascii_case(login) {
                return Err(conflict(format!("login {login:?} is taken")));
            }
            if u.slug == slug {
                return Err(conflict(format!("user slug {slug:?} is taken")));
            }
        }
        Ok(())
    }

    pub(super) fn insert_user(&mut self, user: NewUser) -> Result<UserId> {
        self.check_user_unique(&user.login, &user.slug, None)?;
        let id = UserId(next_seq(&mut self.seq.users));
        self.users.insert(
            id,
            User {
                id,
                login: user.login,
                slug: user.slug,
                display_name: user.display_name,
                bio: user.bio,
                role: user.role,
                password_hash: user.password_hash,
                disabled_at: None,
                created_at: user.created_at,
                updated_at: user.created_at,
            },
        );
        Ok(id)
    }
}

// `async fn` so nothing is read or written until the future is polled, as with D1.
// The bodies never await, which pedantic clippy flags; that is the point of an in-memory
// stand-in, so the lint is expected on these impls and only here.
#[expect(
    clippy::unused_async_trait_impl,
    reason = "in-memory stand-in for async D1 storage"
)]
impl SiteStore for MemoryStore {
    async fn content_version(&self) -> Result<i64> {
        Ok(self.read(|s| s.content_version))
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "in-memory stand-in for async D1 storage"
)]
impl UserStore for MemoryStore {
    async fn create_user(&self, user: NewUser) -> Result<UserId> {
        self.write(|s| s.insert_user(user))
    }

    async fn get_user(&self, id: UserId) -> Result<User> {
        self.read(|s| s.users.get(&id).cloned())
            .ok_or_else(|| user_not_found(id))
    }

    async fn find_user_by_login(&self, login: &str) -> Result<Option<User>> {
        Ok(self.read(|s| {
            s.users
                .values()
                .find(|u| u.login.eq_ignore_ascii_case(login))
                .cloned()
        }))
    }

    async fn find_user_by_slug(&self, slug: &str) -> Result<Option<User>> {
        Ok(self.read(|s| s.users.values().find(|u| u.slug == slug).cloned()))
    }

    async fn list_users(&self) -> Result<Vec<User>> {
        Ok(self.read(|s| s.users.values().cloned().collect()))
    }

    async fn update_user(&self, user: &User) -> Result<()> {
        self.write(|s| {
            let created_at = s
                .users
                .get(&user.id)
                .ok_or_else(|| user_not_found(user.id))?
                .created_at;
            s.check_user_unique(&user.login, &user.slug, Some(user.id))?;
            s.users.insert(
                user.id,
                User {
                    created_at,
                    ..user.clone()
                },
            );
            s.bump();
            Ok(())
        })
    }

    async fn delete_user(&self, id: UserId) -> Result<()> {
        self.write(|s| {
            if !s.user_exists(id) {
                return Err(user_not_found(id));
            }
            // posts.author_id is ON DELETE RESTRICT.
            if s.posts.values().any(|p| p.author_id == id) {
                return Err(conflict(format!(
                    "user {} still authors posts; reassign them first",
                    id.0
                )));
            }
            s.users.remove(&id);
            // ON DELETE CASCADE
            s.sessions.retain(|_, x| x.user_id != id);
            s.passkeys.retain(|_, x| x.user_id != id);
            s.tokens.retain(|_, x| x.user_id != id);
            s.invites.retain(|_, x| x.created_by != id);
            // ON DELETE SET NULL
            for i in s.invites.values_mut().filter(|i| i.used_by == Some(id)) {
                i.used_by = None;
            }
            for m in s.media.values_mut().filter(|m| m.uploaded_by == Some(id)) {
                m.uploaded_by = None;
            }
            for r in s.revisions.values_mut().filter(|r| r.author_id == Some(id)) {
                r.author_id = None;
            }
            for e in s.log.values_mut().filter(|e| e.actor == Some(id)) {
                e.actor = None;
            }
            Ok(())
        })
    }
}
