use plumblog_model::{Error, ErrorCode, Result, Role, UserId};

use super::{MemoryStore, conflict, count, invalid};
use crate::{
    ChallengePurpose, ChallengeStore, LoginLimitStore, MAX_FAILURE_KEYS, NewUser,
    SETUP_CODE_EXPIRES_KEY, SETUP_CODE_HASH_KEY, SITE_ID_KEY, SetupStore, setup_hash_json,
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
    async fn record_failure(&self, key_hash: &str, now: i64, window_ms: i64) -> Result<(i64, i64)> {
        self.write(|s| {
            let row = s
                .login_failures
                .entry(key_hash.to_owned())
                .or_insert((0, now));
            *row = if row.0 == 0 || window_ended(row.1, now, window_ms) {
                (1, now)
            } else {
                (row.0 + 1, row.1)
            };
            Ok(*row)
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

/// The setup expiry if `value_json` is exactly a JSON integer (D1 checks the same text).
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
}
