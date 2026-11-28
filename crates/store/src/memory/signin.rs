use plumblog_model::{Error, ErrorCode, Result, Role, UserId};

use super::{MemoryStore, conflict, count, invalid};
use crate::{
    ChallengePurpose, ChallengeStore, LoginLimitStore, MAX_FAILURE_KEYS, NewSession, NewUser,
    OWNER_RESET_AT_KEY, OWNER_RESET_EXPIRES_KEY, OWNER_RESET_HASH_KEY, OWNER_RESET_KEEP_TOKENS_KEY,
    OWNER_RESET_KEYS, OWNER_RESET_USER_KEY, ProfileUpdate, Proof, Recorded, SETUP_CODE_EXPIRES_KEY,
    SETUP_CODE_HASH_KEY, SITE_ID_KEY, SetupStore, setup_hash_json,
};

/// `window_start + window_ms <= now`, overflow-safe.
fn window_ended(window_start: i64, now: i64, window_ms: i64) -> bool {
    window_start.saturating_add(window_ms) <= now
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "in-memory stand-in for async D1 storage"
)]
impl LoginLimitStore for MemoryStore {
    async fn record_failure(
        &self,
        key_hash: &str,
        now: i64,
        window_ms: i64,
        cap: i64,
    ) -> Result<Recorded> {
        self.write(|s| {
            if let Some(&(n, start)) = s.login_failures.get(key_hash)
                && n >= cap
                && !window_ended(start, now, window_ms)
            {
                return Ok(Recorded {
                    count: n,
                    window_start: start,
                    counted: false,
                });
            }
            let row = s
                .login_failures
                .entry(key_hash.to_owned())
                .or_insert((0, now));
            *row = if row.0 == 0 || window_ended(row.1, now, window_ms) {
                (1, now)
            } else {
                (row.0 + 1, row.1)
            };
            Ok(Recorded {
                count: row.0,
                window_start: row.1,
                counted: true,
            })
        })
    }

    async fn count_capped(
        &self,
        key_hash: &str,
        now: i64,
        window_ms: i64,
        cap: i64,
    ) -> Result<Option<i64>> {
        self.write(|s| {
            let row = s.login_failures.get(key_hash).copied();
            let next = match row {
                Some((n, start)) if !window_ended(start, now, window_ms) => {
                    if n >= cap {
                        return Ok(None);
                    }
                    (n + 1, start)
                }
                _ => (1, now),
            };
            s.login_failures.insert(key_hash.to_owned(), next);
            Ok(Some(next.0))
        })
    }

    async fn delete_ended_failures(&self, now: i64, window_ms: i64, limit: u32) -> Result<u64> {
        self.write(|s| {
            let ended: Vec<String> = s
                .login_failures
                .iter()
                .filter(|(_, (_, start))| window_ended(*start, now, window_ms))
                .map(|(k, _)| k.clone())
                .take(usize::try_from(limit).unwrap_or(usize::MAX))
                .collect();
            for k in &ended {
                s.login_failures.remove(k);
            }
            Ok(ended.len() as u64)
        })
    }

    async fn failures(
        &self,
        key_hashes: &[&str],
        now: i64,
        window_ms: i64,
    ) -> Result<Vec<(i64, i64)>> {
        if key_hashes.len() > MAX_FAILURE_KEYS {
            return Err(invalid(format!(
                "{} failure keys > {MAX_FAILURE_KEYS}",
                key_hashes.len()
            )));
        }
        Ok(self.read(|s| {
            key_hashes
                .iter()
                .map(|k| match s.login_failures.get(*k) {
                    Some(&(n, start)) if !window_ended(start, now, window_ms) => (n, start),
                    _ => (0, 0),
                })
                .collect()
        }))
    }

    async fn forgive_failure(&self, key_hash: &str) -> Result<()> {
        self.write(|s| {
            match s.login_failures.get_mut(key_hash) {
                Some((n, _)) if *n > 1 => *n -= 1,
                Some(_) => {
                    s.login_failures.remove(key_hash);
                }
                None => {}
            }
            Ok(())
        })
    }

    async fn clear_failures(&self, key_hash: &str) -> Result<()> {
        self.write(|s| {
            s.login_failures.remove(key_hash);
            Ok(())
        })
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "in-memory stand-in for async D1 storage"
)]
impl ChallengeStore for MemoryStore {
    async fn put_challenge(
        &self,
        challenge_hash: &str,
        purpose: ChallengePurpose,
        user: Option<UserId>,
        expires_at: i64,
    ) -> Result<()> {
        self.write(|s| {
            if s.challenges.contains_key(challenge_hash) {
                return Err(conflict("challenge hash already exists"));
            }
            if let Some(u) = user
                && !s.user_exists(u)
            {
                return Err(invalid(format!("user {} does not exist", u.0)));
            }
            s.challenges
                .insert(challenge_hash.to_owned(), (purpose, user, expires_at));
            Ok(())
        })
    }

    async fn take_challenge(
        &self,
        challenge_hash: &str,
        purpose: ChallengePurpose,
        now: i64,
    ) -> Result<Option<Option<UserId>>> {
        self.write(|s| {
            let Some(&(p, user, expires_at)) = s.challenges.get(challenge_hash) else {
                return Ok(None);
            };
            if p != purpose {
                return Ok(None);
            }
            s.challenges.remove(challenge_hash);
            Ok((expires_at > now).then_some(user))
        })
    }

    async fn delete_expired_challenges(&self, now: i64) -> Result<u64> {
        self.write(|s| {
            let before = s.challenges.len();
            s.challenges.retain(|_, c| c.2 > now);
            Ok(count(before - s.challenges.len()))
        })
    }
}

/// A setup or reset value (expiry, user id) if `value_json` is exactly a JSON integer (D1
/// checks the same text).
impl super::State {
    /// Whether `proof` still holds for `user` at `now` (see [`crate::Proof`]).
    pub(super) fn proof_holds(&self, user: UserId, proof: &Proof, now: i64) -> bool {
        match proof {
            Proof::Session(h) => self
                .sessions
                .values()
                .any(|x| x.token_hash == *h && x.user_id == user),
            Proof::Password(h) => self
                .users
                .get(&user)
                .is_some_and(|u| u.password_hash.as_deref() == Some(h.as_str())),
            Proof::Passkey(id) => self.passkeys.get(id).is_some_and(|x| x.user_id == user),
            Proof::ResetCode(h) => {
                let get = |k: &str| self.settings.get(k).map(|(v, _)| v.as_str());
                get(OWNER_RESET_HASH_KEY) == Some(setup_hash_json(h).as_str())
                    && get(OWNER_RESET_USER_KEY).and_then(parse_expiry) == Some(user.0)
                    && get(OWNER_RESET_EXPIRES_KEY)
                        .and_then(parse_expiry)
                        .is_some_and(|at| at > now)
            }
            #[cfg(any(test, feature = "contract"))]
            Proof::Unchecked => true,
        }
    }

    /// `Unauthenticated` unless `proof` holds (checked before anything else, as D1 does).
    pub(super) fn require_proof(&self, user: UserId, proof: &Proof, now: i64) -> Result<()> {
        if self.proof_holds(user, proof, now) {
            Ok(())
        } else {
            Err(Error::new(
                ErrorCode::Unauthenticated,
                "the credential this request authenticated with is gone",
            ))
        }
    }
}

fn parse_expiry(value_json: &str) -> Option<i64> {
    value_json
        .parse::<i64>()
        .ok()
        .filter(|n| n.to_string() == value_json)
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "in-memory stand-in for async D1 storage"
)]
impl SetupStore for MemoryStore {
    async fn ensure_site_id(&self, value_json: &str, now: i64) -> Result<String> {
        self.write(|s| {
            Ok(s.settings
                .entry(SITE_ID_KEY.to_owned())
                .or_insert_with(|| (value_json.to_owned(), now))
                .0
                .clone())
        })
    }

    async fn put_setup_code(&self, code_hash: &str, expires_at: i64, now: i64) -> Result<()> {
        self.write(|s| {
            s.settings.insert(
                SETUP_CODE_HASH_KEY.to_owned(),
                (setup_hash_json(code_hash), now),
            );
            s.settings.insert(
                SETUP_CODE_EXPIRES_KEY.to_owned(),
                (expires_at.to_string(), now),
            );
            Ok(())
        })
    }

    async fn create_first_owner(&self, code_hash: &str, user: NewUser, now: i64) -> Result<UserId> {
        self.write(|s| {
            let code_ok = s
                .settings
                .get(SETUP_CODE_HASH_KEY)
                .is_some_and(|(v, _)| *v == setup_hash_json(code_hash));
            let fresh = s
                .settings
                .get(SETUP_CODE_EXPIRES_KEY)
                .and_then(|(v, _)| parse_expiry(v))
                .is_some_and(|at| at > now);
            if !(code_ok && fresh && s.users.is_empty()) {
                return Err(Error::new(
                    ErrorCode::SetupInvalid,
                    "setup code is wrong, expired or used, or a user already exists",
                ));
            }
            let id = s.insert_user(NewUser {
                role: Role::Owner,
                ..user
            })?;
            s.settings.remove(SETUP_CODE_HASH_KEY);
            s.settings.remove(SETUP_CODE_EXPIRES_KEY);
            Ok(id)
        })
    }

    async fn put_owner_reset(
        &self,
        code_hash: &str,
        user: UserId,
        expires_at: i64,
        keep_tokens: bool,
        now: i64,
    ) -> Result<()> {
        self.write(|s| {
            for (k, v) in [
                (OWNER_RESET_HASH_KEY, setup_hash_json(code_hash)),
                (OWNER_RESET_USER_KEY, user.0.to_string()),
                (OWNER_RESET_EXPIRES_KEY, expires_at.to_string()),
                (OWNER_RESET_AT_KEY, now.to_string()),
                (OWNER_RESET_KEEP_TOKENS_KEY, keep_tokens.to_string()),
            ] {
                s.settings.insert(k.to_owned(), (v, now));
            }
            Ok(())
        })
    }

    async fn take_owner_reset(&self, code_hash: &str, now: i64) -> Result<Option<UserId>> {
        self.write(|s| {
            let code_ok = s
                .settings
                .get(OWNER_RESET_HASH_KEY)
                .is_some_and(|(v, _)| *v == setup_hash_json(code_hash));
            if !code_ok {
                return Ok(None);
            }
            let read = |k: &str| s.settings.get(k).and_then(|(v, _)| parse_expiry(v));
            let fresh = read(OWNER_RESET_EXPIRES_KEY).is_some_and(|at| at > now);
            let user = read(OWNER_RESET_USER_KEY).map(UserId);
            for &k in OWNER_RESET_KEYS {
                s.settings.remove(k);
            }
            Ok(user.filter(|_| fresh))
        })
    }

    async fn redeem_owner_reset(
        &self,
        id: UserId,
        hash: String,
        profile: Option<ProfileUpdate>,
        session: NewSession,
        now: i64,
    ) -> Result<i64> {
        let invalid_code = || {
            Error::new(
                ErrorCode::SetupInvalid,
                "reset code wrong, expired or used, or its user is not an active owner",
            )
        };
        if session.user_id != id || !matches!(session.proof, Proof::ResetCode(_)) {
            return Err(invalid(
                "redeem needs a reset-code session of the same user",
            ));
        }
        self.write(|s| {
            let active_owner = s
                .users
                .get(&id)
                .is_some_and(|u| u.role == Role::Owner && u.disabled_at.is_none());
            if !active_owner || !s.proof_holds(id, &session.proof, now) {
                return Err(invalid_code());
            }
            let setting = |k: &str| s.settings.get(k).map(|(v, _)| v.clone());
            // Absent (a code from an older `blog`): everything of the user goes.
            let since = setting(OWNER_RESET_AT_KEY)
                .as_deref()
                .and_then(parse_expiry)
                .unwrap_or(0);
            let keep = setting(OWNER_RESET_KEEP_TOKENS_KEY).as_deref() == Some("true");
            if let Some(p) = &profile {
                s.check_user_unique(&p.login, &p.slug, Some(id))?;
            }
            s.sessions.retain(|_, x| x.user_id != id);
            if s.sessions
                .values()
                .any(|x| x.token_hash == session.token_hash)
            {
                return Err(conflict("session token hash already exists"));
            }
            s.passkeys.retain(|_, x| x.user_id != id);
            for t in s.tokens.values_mut() {
                if t.user_id == id && t.revoked_at.is_none() && (!keep || t.created_at >= since) {
                    t.revoked_at = Some(now);
                }
            }
            s.invites.retain(|_, x| {
                !(x.created_by == id
                    && x.role == Role::Owner
                    && x.used_at.is_none()
                    && x.created_at >= since)
            });
            for &k in OWNER_RESET_KEYS {
                s.settings.remove(k);
            }
            let bump = profile.is_some();
            let u = s.user_mut(id)?;
            if let Some(p) = profile {
                u.login = p.login;
                u.slug = p.slug;
                u.display_name = p.display_name;
                u.bio = p.bio;
            }
            u.password_hash = Some(hash);
            u.updated_at = now;
            if bump {
                s.bump();
            }
            let sid = super::next_rowid(&s.sessions);
            s.sessions.insert(
                sid,
                crate::Session {
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
}
