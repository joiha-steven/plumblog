use std::ops::Bound;

use plumblog_model::{Result, Role, UserId};

use super::{MemoryStore, State, conflict, invalid, next_rowid, next_seq, user_not_found};
use crate::{NewSession, NewUser, ProfileUpdate, Session, SiteStore, User, UserStore, page_len};

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

    /// `Conflict` if `id` is an active owner and no other active owner exists, so a demote,
    /// disable or delete of `id` would leave the site without one.
    fn check_not_last_owner(&self, id: UserId) -> Result<()> {
        let active_owner = |u: &User| u.role == Role::Owner && u.disabled_at.is_none();
        let target = self.users.get(&id).is_some_and(active_owner);
        if target && !self.users.values().any(|u| u.id != id && active_owner(u)) {
            return Err(last_owner(id));
        }
        Ok(())
    }

    /// Exists and is not disabled.
    pub(super) fn user_active(&self, id: UserId) -> bool {
        self.users.get(&id).is_some_and(|u| u.disabled_at.is_none())
    }

    fn user_mut(&mut self, id: UserId) -> Result<&mut User> {
        self.users.get_mut(&id).ok_or_else(|| user_not_found(id))
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

fn last_owner(id: UserId) -> plumblog_model::Error {
    conflict(format!(
        "user {} is the last active owner; make another user owner first",
        id.0
    ))
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

    async fn users_by_id(&self, after: Option<UserId>, limit: u32) -> Result<(Vec<User>, bool)> {
        let keep = page_len(limit);
        let from = after.map_or(Bound::Unbounded, Bound::Excluded);
        let mut rows: Vec<User> = self.read(|s| {
            s.users
                .range((from, Bound::Unbounded))
                .take(keep + 1)
                .map(|(_, u)| u.clone())
                .collect()
        });
        let more = rows.len() > keep;
        rows.truncate(keep);
        Ok((rows, more))
    }

    async fn update_profile(&self, id: UserId, profile: ProfileUpdate, now: i64) -> Result<()> {
        self.write(|s| {
            s.user_mut(id)?;
            s.check_user_unique(&profile.login, &profile.slug, Some(id))?;
            let u = s.user_mut(id)?;
            u.login = profile.login;
            u.slug = profile.slug;
            u.display_name = profile.display_name;
            u.bio = profile.bio;
            u.updated_at = now;
            s.bump();
            Ok(())
        })
    }

    async fn set_password(&self, id: UserId, hash: Option<String>, now: i64) -> Result<()> {
        self.write(|s| {
            let u = s.user_mut(id)?;
            u.password_hash = hash;
            u.updated_at = now;
            Ok(())
        })
    }

    async fn change_password(
        &self,
        id: UserId,
        hash: Option<String>,
        profile: Option<ProfileUpdate>,
        session: NewSession,
        now: i64,
    ) -> Result<i64> {
        self.write(|s| {
            let has_password = s.user_mut(id)?.password_hash.is_some();
            if hash.is_none() && has_password {
                return Err(conflict(
                    "passkey-only change, but the account has a password",
                ));
            }
            if session.user_id != id {
                return Err(invalid(format!(
                    "session for user {} given for user {}",
                    session.user_id.0, id.0
                )));
            }
            if let Some(p) = &profile {
                s.check_user_unique(&p.login, &p.slug, Some(id))?;
            }
            // sessions.token_hash is UNIQUE; the user's own old sessions go first.
            s.sessions.retain(|_, x| x.user_id != id);
            if s.sessions
                .values()
                .any(|x| x.token_hash == session.token_hash)
            {
                return Err(conflict("session token hash already exists"));
            }
            let bump = profile.is_some();
            let u = s.user_mut(id)?;
            if let Some(p) = profile {
                u.login = p.login;
                u.slug = p.slug;
                u.display_name = p.display_name;
                u.bio = p.bio;
            }
            u.password_hash = hash;
            u.updated_at = now;
            if bump {
                s.bump();
            }
            let sid = next_rowid(&s.sessions);
            s.sessions.insert(
                sid,
                Session {
                    id: sid,
                    token_hash: session.token_hash,
                    user_id: id,
                    created_at: session.created_at,
                    expires_at: session.expires_at,
                },
            );
            Ok(sid)
        })
    }

    async fn set_role(&self, id: UserId, role: Role, now: i64) -> Result<()> {
        self.write(|s| {
            s.user_mut(id)?;
            if role != Role::Owner {
                s.check_not_last_owner(id)?;
            }
            let u = s.user_mut(id)?;
            u.role = role;
            u.updated_at = now;
            Ok(())
        })
    }

    async fn set_disabled(&self, id: UserId, disabled_at: Option<i64>, now: i64) -> Result<()> {
        self.write(|s| {
            s.user_mut(id)?;
            if disabled_at.is_some() {
                s.check_not_last_owner(id)?;
                // sessions.user_id: a disabled account must not stay signed in.
                s.sessions.retain(|_, x| x.user_id != id);
            }
            let u = s.user_mut(id)?;
            u.disabled_at = disabled_at;
            u.updated_at = now;
            Ok(())
        })
    }

    async fn delete_user(&self, id: UserId) -> Result<()> {
        self.write(|s| {
            if !s.user_exists(id) {
                return Err(user_not_found(id));
            }
            s.check_not_last_owner(id)?;
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
            s.challenges.retain(|_, x| x.1 != Some(id));
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
            for e in s.log.values_mut().filter(|e| e.actor_id() == Some(id)) {
                *e = e.clone().actor(None);
            }
            Ok(())
        })
    }
}
