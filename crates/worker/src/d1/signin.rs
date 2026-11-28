use std::collections::HashMap;

use plumblog_model::{Error, ErrorCode, Result, Role, UserId};
use plumblog_store::{
    ChallengePurpose, ChallengeStore, LoginLimitStore, MAX_FAILURE_KEYS, NewSession, NewUser,
    OWNER_RESET_AT_KEY, OWNER_RESET_EXPIRES_KEY, OWNER_RESET_HASH_KEY, OWNER_RESET_KEEP_TOKENS_KEY,
    OWNER_RESET_KEYS, OWNER_RESET_SINCE_MARGIN_MS, OWNER_RESET_USER_KEY, ProfileUpdate, Proof,
    Recorded, SETUP_CODE_EXPIRES_KEY, SETUP_CODE_HASH_KEY, SITE_ID_KEY, SetupStore,
    setup_hash_json,
};
use worker::wasm_bindgen::JsValue;

use super::{
    D1Store, Failure, Stmt, bound, bump, guard, int, invalid, opt_int, opt_text, proof_guard,
    storage, text,
};

/// `window_start + window_ms <= now` in SQL, with the two values bound in that order.
const WINDOW_ENDED: &str = "window_start + ? <= ?";

impl LoginLimitStore for D1Store {
    async fn record_failure(
        &self,
        key_hash: &str,
        now: i64,
        window_ms: i64,
        cap: i64,
    ) -> Result<Recorded> {
        // One statement, so two attempts racing for one key both count. SET expressions
        // read the old row, so both CASEs see the same old window_start. The WHERE leaves a
        // key that reached `cap` in its open window untouched (no row written, none
        // returned); the read in the same transaction then gives the row as it is.
        let upsert = self.stmt(
            "record_failure",
            &format!(
                "INSERT INTO login_failures (key_hash, count, window_start) VALUES (?, 1, ?) \
                 ON CONFLICT (key_hash) DO UPDATE SET \
                 count = CASE WHEN {WINDOW_ENDED} THEN 1 ELSE count + 1 END, \
                 window_start = CASE WHEN {WINDOW_ENDED} THEN excluded.window_start \
                 ELSE window_start END \
                 WHERE count < ? OR {WINDOW_ENDED} \
                 RETURNING count"
            ),
            &[
                text(key_hash),
                int(now)?,
                bound(window_ms),
                bound(now),
                bound(window_ms),
                bound(now),
                bound(cap),
                bound(window_ms),
                bound(now),
            ],
        )?;
        let read = self.stmt(
            "record_failure_read",
            "SELECT count AS c, window_start AS w FROM login_failures WHERE key_hash = ?",
            &[text(key_hash)],
        )?;
        let results = self.batch("record_failure", vec![upsert, read]).await?;
        let rows = |i: usize| -> Result<Vec<HashMap<String, i64>>> {
            results
                .get(i)
                .ok_or_else(|| storage("record_failure", "batch result missing"))?
                .results()
                .map_err(|e| storage("record_failure", &format!("{e}")))
        };
        let counted = !rows(0)?.is_empty();
        let row = rows(1)?
            .into_iter()
            .next()
            .ok_or_else(|| storage("record_failure", "row missing after upsert"))?;
        match (row.get("c"), row.get("w")) {
            (Some(&count), Some(&window_start)) => Ok(Recorded {
                count,
                window_start,
                counted,
            }),
            _ => Err(storage(
                "record_failure",
                "row without count or window_start",
            )),
        }
    }

    async fn count_capped(
        &self,
        key_hash: &str,
        now: i64,
        window_ms: i64,
        cap: i64,
    ) -> Result<Option<i64>> {
        // The upsert of record_failure without its read-back: at the cap the WHERE leaves the
        // row untouched, so the statement writes nothing and returns no row.
        let s = self.stmt(
            "count_capped",
            &format!(
                "INSERT INTO login_failures (key_hash, count, window_start) VALUES (?, 1, ?) \
                 ON CONFLICT (key_hash) DO UPDATE SET \
                 count = CASE WHEN {WINDOW_ENDED} THEN 1 ELSE count + 1 END, \
                 window_start = CASE WHEN {WINDOW_ENDED} THEN excluded.window_start \
                 ELSE window_start END \
                 WHERE count < ? OR {WINDOW_ENDED} \
                 RETURNING count"
            ),
            &[
                text(key_hash),
                int(now)?,
                bound(window_ms),
                bound(now),
                bound(window_ms),
                bound(now),
                bound(cap),
                bound(window_ms),
                bound(now),
            ],
        )?;
        match self.row(s).await? {
            Some(mut r) => r.int().map(Some),
            None => Ok(None),
        }
    }

    async fn delete_ended_failures(&self, now: i64, window_ms: i64, limit: u32) -> Result<u64> {
        // D1's SQLite has no DELETE ... LIMIT; the subquery bounds it the same way.
        let s = self.stmt(
            "delete_ended_failures",
            &format!(
                "DELETE FROM login_failures WHERE key_hash IN \
                 (SELECT key_hash FROM login_failures WHERE {WINDOW_ENDED} LIMIT ?)"
            ),
            &[bound(window_ms), bound(now), bound(i64::from(limit))],
        )?;
        let r = self.exec(s).await?;
        Self::changes("delete_ended_failures", &r)
    }

    // 1 query (0 for no keys).
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
        if key_hashes.is_empty() {
            return Ok(Vec::new());
        }
        let marks = vec!["?"; key_hashes.len()].join(", ");
        let mut params: Vec<_> = key_hashes.iter().map(|k| text(k)).collect();
        params.push(bound(window_ms));
        params.push(bound(now));
        let s = self.stmt(
            "failures",
            &format!(
                "SELECT key_hash, count, window_start FROM login_failures \
                 WHERE key_hash IN ({marks}) AND NOT ({WINDOW_ENDED})"
            ),
            &params,
        )?;
        let mut found = std::collections::HashMap::new();
        for mut r in self.rows(s).await? {
            found.insert(r.text()?, (r.int()?, r.int()?));
        }
        Ok(key_hashes
            .iter()
            .map(|k| found.get(*k).copied().unwrap_or((0, 0)))
            .collect())
    }

    async fn forgive_failure(&self, key_hash: &str) -> Result<()> {
        // Rows hold at least one (CHECK): the last one goes with its row. Delete first, so a
        // count the update just lowered to 1 is not deleted too.
        let stmts = vec![
            self.stmt(
                "forgive_failure_last",
                "DELETE FROM login_failures WHERE key_hash = ? AND count <= 1",
                &[text(key_hash)],
            )?,
            self.stmt(
                "forgive_failure",
                "UPDATE login_failures SET count = count - 1 WHERE key_hash = ? AND count > 1",
                &[text(key_hash)],
            )?,
        ];
        self.batch("forgive_failure", stmts).await.map(drop)
    }

    async fn clear_failures(&self, key_hash: &str) -> Result<()> {
        let s = self.stmt(
            "clear_failures",
            "DELETE FROM login_failures WHERE key_hash = ?",
            &[text(key_hash)],
        )?;
        self.exec(s).await.map(drop)
    }
}

impl ChallengeStore for D1Store {
    async fn put_challenge(
        &self,
        challenge_hash: &str,
        purpose: ChallengePurpose,
        user: Option<UserId>,
        expires_at: i64,
    ) -> Result<()> {
        // A duplicate hash is a UNIQUE error (Conflict); a missing user an FK error.
        let s = self.stmt(
            "put_challenge",
            "INSERT INTO auth_challenges (challenge_hash, purpose, user_id, expires_at) \
             VALUES (?, ?, ?, ?)",
            &[
                text(challenge_hash),
                text(purpose.as_str()),
                opt_int(user.map(|u| u.0))?,
                int(expires_at)?,
            ],
        )?;
        self.exec(s).await.map(drop)
    }

    async fn take_challenge(
        &self,
        challenge_hash: &str,
        purpose: ChallengePurpose,
        now: i64,
    ) -> Result<Option<Option<UserId>>> {
        // DELETE ... RETURNING is one atomic statement: of two concurrent takes only one
        // gets the row. Expiry is checked on what came back, so an expired one is deleted.
        let s = self.stmt(
            "take_challenge",
            "DELETE FROM auth_challenges WHERE challenge_hash = ? AND purpose = ? \
             RETURNING user_id, expires_at",
            &[text(challenge_hash), text(purpose.as_str())],
        )?;
        let Some(mut row) = self.row(s).await? else {
            return Ok(None);
        };
        let user = row.opt_int()?.map(UserId);
        let expires_at = row.int()?;
        Ok((expires_at > now).then_some(user))
    }

    async fn delete_expired_challenges(&self, now: i64) -> Result<u64> {
        let s = self.stmt(
            "delete_expired_challenges",
            "DELETE FROM auth_challenges WHERE expires_at <= ?",
            &[bound(now)],
        )?;
        let r = self.exec(s).await?;
        Self::changes("delete_expired_challenges", &r)
    }
}

impl SetupStore for D1Store {
    async fn ensure_site_id(&self, value_json: &str, now: i64) -> Result<String> {
        // Insert-or-ignore then read, in one transaction, so a racing first visitor's id wins
        // for both.
        let insert = self.stmt(
            "ensure_site_id",
            "INSERT INTO settings (key, value_json, updated_at) VALUES (?, ?, ?) \
             ON CONFLICT (key) DO NOTHING",
            &[text(SITE_ID_KEY), text(value_json), int(now)?],
        )?;
        let read = self.stmt(
            "ensure_site_id_read",
            "SELECT value_json AS v FROM settings WHERE key = ?",
            &[text(SITE_ID_KEY)],
        )?;
        let results = self.batch("ensure_site_id", vec![insert, read]).await?;
        let rows: Vec<HashMap<String, String>> = results
            .get(1)
            .ok_or_else(|| storage("ensure_site_id", "batch returned no read result"))?
            .results()
            .map_err(|e| storage("ensure_site_id", &format!("{e}")))?;
        rows.into_iter()
            .next()
            .and_then(|mut r| r.remove("v"))
            .ok_or_else(|| storage("ensure_site_id", "site_id missing after insert"))
    }

    async fn put_setup_code(&self, code_hash: &str, expires_at: i64, now: i64) -> Result<()> {
        let upsert = "INSERT INTO settings (key, value_json, updated_at) VALUES (?, ?, ?) \
                      ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json, \
                      updated_at = excluded.updated_at";
        let hash = self.stmt(
            "put_setup_code_hash",
            upsert,
            &[
                text(SETUP_CODE_HASH_KEY),
                text(&setup_hash_json(code_hash)),
                int(now)?,
            ],
        )?;
        let expiry = self.stmt(
            "put_setup_code_expiry",
            upsert,
            &[
                text(SETUP_CODE_EXPIRES_KEY),
                text(&expires_at.to_string()),
                int(now)?,
            ],
        )?;
        self.batch("put_setup_code", vec![hash, expiry])
            .await
            .map(drop)
    }

    async fn create_first_owner(&self, code_hash: &str, user: NewUser, now: i64) -> Result<UserId> {
        // One batch: the guard checks every condition inside the transaction, so of two
        // setups racing only the first commits; the second finds a user and aborts. The
        // expiry must be exactly a JSON integer, as the memory store requires.
        let valid = guard(
            self,
            "create_first_owner_guard",
            "NOT EXISTS (SELECT 1 FROM users) \
             AND EXISTS (SELECT 1 FROM settings WHERE key = ? AND value_json = ?) \
             AND EXISTS (SELECT 1 FROM settings WHERE key = ? \
               AND CAST(CAST(value_json AS INTEGER) AS TEXT) = value_json \
               AND CAST(value_json AS INTEGER) > ?)",
            &[
                text(SETUP_CODE_HASH_KEY),
                text(&setup_hash_json(code_hash)),
                text(SETUP_CODE_EXPIRES_KEY),
                bound(now),
            ],
        )?;
        let insert = self.stmt(
            "create_first_owner_user",
            "INSERT INTO users (login, slug, display_name, bio, role, password_hash, \
             disabled_at, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, NULL, ?, ?)",
            &[
                text(&user.login),
                text(&user.slug),
                text(&user.display_name),
                text(&user.bio),
                text(Role::Owner.as_str()),
                opt_text(user.password_hash.as_deref()),
                int(user.created_at)?,
                int(user.created_at)?,
            ],
        )?;
        let consume = self.stmt(
            "create_first_owner_consume",
            "DELETE FROM settings WHERE key IN (?, ?)",
            &[text(SETUP_CODE_HASH_KEY), text(SETUP_CODE_EXPIRES_KEY)],
        )?;
        match self
            .batch_guarded("create_first_owner", vec![valid, insert, consume])
            .await
        {
            Ok(results) => Self::last_row_id("create_first_owner_user", &results[1]).map(UserId),
            Err(Failure::Guard) => Err(Error::new(
                ErrorCode::SetupInvalid,
                "setup code is wrong, expired or used, or a user already exists",
            )),
            Err(Failure::Error(e)) => Err(e),
        }
    }
    async fn put_owner_reset(
        &self,
        code_hash: &str,
        user: UserId,
        expires_at: i64,
        keep_tokens: bool,
        now: i64,
    ) -> Result<()> {
        // Hash first: a code half written by `blog admin reset` (REST, not atomic) is one
        // nobody was shown, and the previous code stops working at once.
        let mut stmts = Vec::new();
        for (name, key, value) in [
            (
                "put_owner_reset_hash",
                OWNER_RESET_HASH_KEY,
                setup_hash_json(code_hash),
            ),
            (
                "put_owner_reset_user",
                OWNER_RESET_USER_KEY,
                user.0.to_string(),
            ),
            (
                "put_owner_reset_expiry",
                OWNER_RESET_EXPIRES_KEY,
                expires_at.to_string(),
            ),
            ("put_owner_reset_at", OWNER_RESET_AT_KEY, now.to_string()),
            (
                "put_owner_reset_keep",
                OWNER_RESET_KEEP_TOKENS_KEY,
                keep_tokens.to_string(),
            ),
        ] {
            stmts.push(self.stmt(name, PUT_OWNER_RESET, &[text(key), text(&value), int(now)?])?);
        }
        self.batch("put_owner_reset", stmts).await.map(drop)
    }

    async fn take_owner_reset(&self, code_hash: &str, now: i64) -> Result<Option<UserId>> {
        // One batch (a transaction): the read and the deletes see the same rows, so of two
        // takes racing only the first reads a row. Every delete is conditioned on the hash,
        // and the hash row goes last, so a wrong code deletes nothing.
        let hash = setup_hash_json(code_hash);
        let read = self.stmt(
            "take_owner_reset_read",
            "SELECT u.value_json AS user, e.value_json AS expires FROM settings h \
             LEFT JOIN settings u ON u.key = ? LEFT JOIN settings e ON e.key = ? \
             WHERE h.key = ? AND h.value_json = ?",
            &[
                text(OWNER_RESET_USER_KEY),
                text(OWNER_RESET_EXPIRES_KEY),
                text(OWNER_RESET_HASH_KEY),
                text(&hash),
            ],
        )?;
        let rest = self.stmt(
            "take_owner_reset_rest",
            "DELETE FROM settings WHERE key IN (?, ?, ?, ?) \
             AND EXISTS (SELECT 1 FROM settings WHERE key = ? AND value_json = ?)",
            &[
                text(OWNER_RESET_USER_KEY),
                text(OWNER_RESET_EXPIRES_KEY),
                text(OWNER_RESET_AT_KEY),
                text(OWNER_RESET_KEEP_TOKENS_KEY),
                text(OWNER_RESET_HASH_KEY),
                text(&hash),
            ],
        )?;
        let consume = self.stmt(
            "take_owner_reset_hash",
            "DELETE FROM settings WHERE key = ? AND value_json = ?",
            &[text(OWNER_RESET_HASH_KEY), text(&hash)],
        )?;
        let results = self
            .batch("take_owner_reset", vec![read, rest, consume])
            .await?;
        let rows: Vec<HashMap<String, Option<String>>> = results
            .first()
            .ok_or_else(|| storage("take_owner_reset", "batch returned no read result"))?
            .results()
            .map_err(|e| storage("take_owner_reset", &format!("{e}")))?;
        let Some(row) = rows.into_iter().next() else {
            return Ok(None);
        };
        // Exactly a JSON integer, as the memory store and the setup expiry require.
        let int_of = |k: &str| {
            row.get(k)
                .cloned()
                .flatten()
                .and_then(|v| v.parse::<i64>().ok().filter(|n| n.to_string() == v))
        };
        let fresh = int_of("expires").is_some_and(|at| at > now);
        Ok(int_of("user").map(UserId).filter(|_| fresh))
    }

    async fn redeem_owner_reset(
        &self,
        id: UserId,
        hash: String,
        profile: Option<ProfileUpdate>,
        session: NewSession,
        now: i64,
    ) -> Result<i64> {
        if session.user_id != id || !matches!(session.proof, Proof::ResetCode(_)) {
            return Err(invalid(
                "redeem needs a reset-code session of the same user",
            ));
        }
        // One batch: the code is checked, spent and acted on together. A login another user
        // holds fails the update and rolls everything back, so the code still works.
        let mut stmts = vec![
            proof_guard(self, id, &session.proof, now)?,
            guard(
                self,
                "redeem_owner_reset_owner",
                "EXISTS (SELECT 1 FROM users WHERE id = ? AND role = 'owner' \
                 AND disabled_at IS NULL)",
                &[bound(id.0)],
            )?,
        ];
        if let Some(p) = &profile {
            stmts.push(bump(self)?);
            stmts.push(self.stmt(
                "redeem_owner_reset_user",
                "UPDATE users SET login = ?, slug = ?, display_name = ?, bio = ?, \
                 password_hash = ?, updated_at = ? WHERE id = ?",
                &[
                    text(&p.login),
                    text(&p.slug),
                    text(&p.display_name),
                    text(&p.bio),
                    text(&hash),
                    int(now)?,
                    bound(id.0),
                ],
            )?);
        } else {
            stmts.push(self.stmt(
                "redeem_owner_reset_user",
                "UPDATE users SET password_hash = ?, updated_at = ? WHERE id = ?",
                &[text(&hash), int(now)?, bound(id.0)],
            )?);
        }
        stmts.extend(self.redeem_cleanup(id, now)?);
        stmts.push(self.stmt(
            "redeem_owner_reset_session",
            "INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES (?, ?, ?, ?)",
            &[
                text(&session.token_hash),
                bound(id.0),
                int(session.created_at)?,
                int(session.expires_at)?,
            ],
        )?);
        match self.batch_guarded("redeem_owner_reset", stmts).await {
            Ok(r) => {
                let last = r
                    .last()
                    .ok_or_else(|| storage("redeem_owner_reset", "batch returned no results"))?;
                Self::last_row_id("redeem_owner_reset_session", last)
            }
            Err(Failure::Guard) => Err(Error::new(
                ErrorCode::SetupInvalid,
                "reset code wrong, expired or used, or its user is not an active owner",
            )),
            Err(Failure::Error(e)) => Err(e),
        }
    }
}

impl D1Store {
    /// The redeem's removals: every session and passkey of `id`, its tokens and unused owner
    /// invites since the reset, and the reset keys.
    fn redeem_cleanup(&self, id: UserId, now: i64) -> Result<Vec<Stmt>> {
        let mut out = Vec::new();
        let user = [bound(id.0)];
        out.push(self.stmt(
            "redeem_owner_reset_sessions",
            "DELETE FROM sessions WHERE user_id = ?",
            &user,
        )?);
        out.push(self.stmt(
            "redeem_owner_reset_passkeys",
            "DELETE FROM passkeys WHERE user_id = ?",
            &user,
        )?);
        // Absent keys (a code from an older `blog`): every token and owner invite goes.
        let since = format!(
            "(COALESCE((SELECT CAST(value_json AS INTEGER) FROM settings WHERE key = ?), 0) - {OWNER_RESET_SINCE_MARGIN_MS})"
        );
        out.push(self.stmt(
            "redeem_owner_reset_tokens",
            &format!(
                "UPDATE api_tokens SET revoked_at = ? WHERE user_id = ? AND revoked_at IS NULL \
                 AND (COALESCE((SELECT value_json FROM settings WHERE key = ?), 'false') \
                 <> 'true' OR created_at >= {since})"
            ),
            &[
                int(now)?,
                bound(id.0),
                text(OWNER_RESET_KEEP_TOKENS_KEY),
                text(OWNER_RESET_AT_KEY),
            ],
        )?);
        out.push(self.stmt(
            "redeem_owner_reset_invites",
            &format!(
                "DELETE FROM invites WHERE created_by = ? AND role = 'owner' \
                 AND used_at IS NULL AND created_at >= {since}"
            ),
            &[bound(id.0), text(OWNER_RESET_AT_KEY)],
        )?);
        let keys: Vec<JsValue> = OWNER_RESET_KEYS.iter().map(|k| text(k)).collect();
        let marks = vec!["?"; keys.len()].join(", ");
        out.push(self.stmt(
            "redeem_owner_reset_keys",
            &format!("DELETE FROM settings WHERE key IN ({marks})"),
            &keys,
        )?);
        Ok(out)
    }
}

/// The reset code upsert, one statement per key. `blog admin reset` sends the same SQL
/// (crates/cli/src/d1.rs `PUT_OWNER_RESET`).
const PUT_OWNER_RESET: &str = "INSERT INTO settings (key, value_json, updated_at) VALUES (?, ?, ?) \
     ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json, \
     updated_at = excluded.updated_at";
