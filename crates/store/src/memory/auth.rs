use std::cmp::Reverse;

use plumblog_model::{Error, ErrorCode, Result, UserId};

use super::{MemoryStore, conflict, count, invalid, next_rowid, not_found};
use crate::{
    ApiToken, Invite, InviteStore, NewInvite, NewPasskey, NewSession, NewToken, NewUser, Passkey,
    PasskeyStore, Session, SessionStore, TokenStore,
};

#[expect(
    clippy::unused_async_trait_impl,
    reason = "in-memory stand-in for async D1 storage"
)]
impl SessionStore for MemoryStore {
    async fn create_session(&self, session: NewSession) -> Result<i64> {
        self.write(|s| {
            s.require_proof(session.user_id, &session.proof, session.created_at)?;
            if !s.user_exists(session.user_id) {
                return Err(invalid(format!(
                    "user {} does not exist",
                    session.user_id.0
                )));
            }
            if s.sessions
                .values()
                .any(|x| x.token_hash == session.token_hash)
            {
                return Err(conflict("session token hash already exists"));
            }
            let id = next_rowid(&s.sessions);
            s.sessions.insert(
                id,
                Session {
                    id,
                    token_hash: session.token_hash,
                    user_id: session.user_id,
                    created_at: session.created_at,
                    expires_at: session.expires_at,
                },
            );
            Ok(id)
        })
    }

    async fn find_session(&self, token_hash: &str, now: i64) -> Result<Option<Session>> {
        Ok(self.read(|s| {
            s.sessions
                .values()
                .find(|x| {
                    x.token_hash == token_hash && x.expires_at > now && s.user_active(x.user_id)
                })
                .cloned()
        }))
    }

    async fn renew_session(&self, token_hash: &str, expires_at: i64, now: i64) -> Result<bool> {
        self.write(|s| {
            let found = s
                .sessions
                .values_mut()
                .find(|x| x.token_hash == token_hash && x.expires_at > now);
            Ok(found.map(|x| x.expires_at = expires_at).is_some())
        })
    }

    async fn delete_session(&self, token_hash: &str) -> Result<()> {
        self.write(|s| {
            s.sessions.retain(|_, x| x.token_hash != token_hash);
            Ok(())
        })
    }

    async fn delete_user_sessions(&self, user: UserId) -> Result<u64> {
        self.write(|s| {
            let before = s.sessions.len();
            s.sessions.retain(|_, x| x.user_id != user);
            Ok(count(before - s.sessions.len()))
        })
    }

    async fn delete_expired_sessions(&self, now: i64) -> Result<u64> {
        self.write(|s| {
            let before = s.sessions.len();
            s.sessions.retain(|_, x| x.expires_at > now);
            Ok(count(before - s.sessions.len()))
        })
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "in-memory stand-in for async D1 storage"
)]
impl PasskeyStore for MemoryStore {
    async fn add_passkey(&self, passkey: NewPasskey) -> Result<i64> {
        self.write(|s| {
            s.require_proof(passkey.user_id, &passkey.proof, passkey.created_at)?;
            if !s.user_exists(passkey.user_id) {
                return Err(invalid(format!(
                    "user {} does not exist",
                    passkey.user_id.0
                )));
            }
            if s.passkeys
                .values()
                .any(|x| x.credential_id == passkey.credential_id)
            {
                return Err(conflict("passkey credential id already exists"));
            }
            let id = next_rowid(&s.passkeys);
            s.passkeys.insert(
                id,
                Passkey {
                    id,
                    user_id: passkey.user_id,
                    credential_id: passkey.credential_id,
                    public_key: passkey.public_key,
                    sign_count: passkey.sign_count,
                    name: passkey.name,
                    created_at: passkey.created_at,
                    last_used_at: None,
                },
            );
            Ok(id)
        })
    }

    async fn find_passkey(&self, credential_id: &str) -> Result<Option<Passkey>> {
        Ok(self.read(|s| {
            s.passkeys
                .values()
                .find(|x| x.credential_id == credential_id)
                .cloned()
        }))
    }

    async fn list_passkeys(&self, user: UserId) -> Result<Vec<Passkey>> {
        Ok(self.read(|s| {
            s.passkeys
                .values()
                .filter(|x| x.user_id == user)
                .cloned()
                .collect()
        }))
    }

    async fn touch_passkey(&self, id: i64, sign_count: i64, at: i64) -> Result<bool> {
        self.write(|s| {
            let p = s
                .passkeys
                .get_mut(&id)
                .ok_or_else(|| not_found("passkey", id))?;
            let accepted = sign_count > p.sign_count || (sign_count == 0 && p.sign_count == 0);
            // A refused sign-in is not a use, so nothing is recorded.
            if accepted {
                p.sign_count = sign_count;
                p.last_used_at = Some(at);
            }
            Ok(accepted)
        })
    }

    async fn delete_own_passkey(&self, user: UserId, id: i64) -> Result<()> {
        self.write(|s| {
            if s.passkeys.get(&id).is_none_or(|p| p.user_id != user) {
                return Err(not_found("passkey", id));
            }
            let passwordless = s
                .users
                .get(&user)
                .is_some_and(|u| u.password_hash.is_none());
            let theirs = s.passkeys.values().filter(|p| p.user_id == user).count();
            if passwordless && theirs == 1 {
                return Err(conflict("last passkey of an account without a password"));
            }
            s.passkeys.remove(&id);
            Ok(())
        })
    }

    async fn delete_passkey(&self, id: i64) -> Result<()> {
        self.write(|s| {
            s.passkeys
                .remove(&id)
                .map(drop)
                .ok_or_else(|| not_found("passkey", id))
        })
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "in-memory stand-in for async D1 storage"
)]
impl TokenStore for MemoryStore {
    async fn create_token(&self, token: NewToken) -> Result<i64> {
        self.write(|s| {
            s.require_proof(token.user_id, &token.proof, token.created_at)?;
            if !s.user_exists(token.user_id) {
                return Err(invalid(format!("user {} does not exist", token.user_id.0)));
            }
            if s.tokens.values().any(|x| x.token_hash == token.token_hash) {
                return Err(conflict("api token hash already exists"));
            }
            let id = next_rowid(&s.tokens);
            s.tokens.insert(
                id,
                ApiToken {
                    id,
                    user_id: token.user_id,
                    name: token.name,
                    token_hash: token.token_hash,
                    scope: token.scope,
                    created_at: token.created_at,
                    expires_at: token.expires_at,
                    revoked_at: None,
                },
            );
            Ok(id)
        })
    }

    async fn find_token(&self, token_hash: &str, now: i64) -> Result<Option<ApiToken>> {
        Ok(self.read(|s| {
            s.tokens
                .values()
                .find(|x| {
                    x.token_hash == token_hash
                        && x.revoked_at.is_none()
                        && x.expires_at.is_none_or(|e| e > now)
                        && s.user_active(x.user_id)
                })
                .cloned()
        }))
    }

    async fn list_tokens(&self, user: UserId) -> Result<Vec<ApiToken>> {
        Ok(self.read(|s| {
            s.tokens
                .values()
                .filter(|x| x.user_id == user)
                .cloned()
                .collect()
        }))
    }

    async fn revoke_token(&self, id: i64, at: i64) -> Result<()> {
        self.write(|s| {
            let t = s
                .tokens
                .get_mut(&id)
                .ok_or_else(|| not_found("api token", id))?;
            t.revoked_at.get_or_insert(at);
            Ok(())
        })
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "in-memory stand-in for async D1 storage"
)]
impl InviteStore for MemoryStore {
    async fn create_invite(&self, invite: NewInvite) -> Result<i64> {
        self.write(|s| {
            s.require_admin(
                &crate::Admin {
                    id: invite.created_by,
                    proof: invite.proof.clone(),
                },
                invite.created_at,
            )?;
            if !s.user_exists(invite.created_by) {
                return Err(invalid(format!(
                    "user {} does not exist",
                    invite.created_by.0
                )));
            }
            if s.invites.values().any(|x| x.code_hash == invite.code_hash) {
                return Err(conflict("invite code hash already exists"));
            }
            let id = next_rowid(&s.invites);
            s.invites.insert(
                id,
                Invite {
                    id,
                    code_hash: invite.code_hash,
                    role: invite.role,
                    created_by: invite.created_by,
                    created_at: invite.created_at,
                    expires_at: invite.expires_at,
                    used_at: None,
                    used_by: None,
                },
            );
            Ok(id)
        })
    }

    async fn list_invites(&self) -> Result<Vec<Invite>> {
        Ok(self.read(|s| {
            let mut all: Vec<Invite> = s.invites.values().cloned().collect();
            all.sort_by_key(|i| Reverse((i.created_at, i.id)));
            all
        }))
    }

    async fn delete_invite(&self, by: &crate::Admin, id: i64, now: i64) -> Result<()> {
        self.write(|s| {
            s.require_admin(by, now)?;
            if s.invites.get(&id).is_none_or(|i| i.used_at.is_some()) {
                return Err(not_found("unused invite", id));
            }
            s.invites.remove(&id);
            Ok(())
        })
    }

    async fn accept_invite(&self, code_hash: &str, user: NewUser, now: i64) -> Result<UserId> {
        self.write(|s| {
            let (invite_id, role) = s
                .invites
                .values()
                .find(|i| {
                    i.code_hash == code_hash
                        && i.used_at.is_none()
                        && i.expires_at > now
                        // Only while its creator is still an active owner.
                        && s.users.get(&i.created_by).is_some_and(|u| {
                            u.role == plumblog_model::Role::Owner && u.disabled_at.is_none()
                        })
                })
                .map(|i| (i.id, i.role))
                .ok_or_else(|| {
                    Error::new(
                        ErrorCode::InviteInvalid,
                        "invite is unknown, used or expired",
                    )
                })?;
            let id = s.insert_user(NewUser { role, ..user })?;
            if let Some(i) = s.invites.get_mut(&invite_id) {
                i.used_at = Some(now);
                i.used_by = Some(id);
            }
            Ok(id)
        })
    }
}
