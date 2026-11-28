//! The store traits over Cloudflare D1. Behaves exactly like `plumblog_store::memory`
//! (same error codes, ordering, version bumps); the shared contract suite proves it
//! (`/__contract` in test builds, tools/worker-test/contract.test.mjs).
//!
//! Rules every method here follows:
//! - Values are always bound parameters; SQL text is fixed or built from fixed fragments.
//! - A write that touches more than one row is one `batch` (D1 runs it as a transaction).
//! - A precondition that only a read can check is re-asserted inside the batch by a
//!   [`guard`]: if a concurrent request changed it, the whole batch fails and nothing is
//!   written.
//! - Each read method says in a comment how many queries it makes.

mod auth;
mod comments;
mod log;
mod media;
mod post_save;
mod posts;
mod reader;
mod reader_sql;
mod redirects;
mod row;
#[cfg(feature = "contract-tests")]
pub mod selftest;
mod settings;
mod signin;
mod taxonomy;
mod users;
mod webhooks;

use plumblog_model::{Error, ErrorCode, Result, UserId};
use plumblog_store::{
    OWNER_RESET_EXPIRES_KEY, OWNER_RESET_HASH_KEY, OWNER_RESET_USER_KEY, Proof, setup_hash_json,
};
use worker::wasm_bindgen::JsValue;
use worker::{D1Database, D1PreparedStatement, D1Result};

pub(crate) use row::Row;

/// D1 allows at most this many bound parameters per statement.
const MAX_PARAMS: usize = 100;

/// Every store trait over one D1 binding.
pub struct D1Store {
    db: D1Database,
}

impl D1Store {
    #[must_use]
    pub fn new(db: D1Database) -> Self {
        Self { db }
    }
}

/// A statement with the name used in error details, so a log line says which one failed.
pub(crate) struct Stmt {
    name: &'static str,
    inner: D1PreparedStatement,
}

/// Why a batch did not commit.
pub(crate) enum Failure {
    /// A [`guard`] found its condition false: something read before the batch changed.
    Guard,
    Error(Error),
}

impl From<Error> for Failure {
    fn from(e: Error) -> Self {
        Self::Error(e)
    }
}

/// The CHECK message SQLite gives when a guard fires (`site` has `CHECK (id = 1)`).
const GUARD_MESSAGE: &str = "CHECK constraint failed: id = 1";

impl D1Store {
    /// Prepares `sql` with `params` bound in order.
    pub(crate) fn stmt(&self, name: &'static str, sql: &str, params: &[JsValue]) -> Result<Stmt> {
        if params.len() > MAX_PARAMS {
            return Err(storage(
                name,
                &format!("{} parameters > {MAX_PARAMS}", params.len()),
            ));
        }
        let inner = self
            .db
            .prepare(sql)
            .bind(params)
            .map_err(|e| storage(name, &message(&e)))?;
        Ok(Stmt { name, inner })
    }

    /// Runs a read and returns its rows.
    pub(crate) async fn rows(&self, s: Stmt) -> Result<Vec<Row>> {
        let raw = s
            .inner
            .raw_js_value()
            .await
            .map_err(|e| map_error(s.name, &e))?;
        raw.into_iter().map(|r| Row::new(s.name, r)).collect()
    }

    /// Runs a read expected to return at most one row.
    pub(crate) async fn row(&self, s: Stmt) -> Result<Option<Row>> {
        Ok(self.rows(s).await?.into_iter().next())
    }

    /// Runs one write statement.
    pub(crate) async fn exec(&self, s: Stmt) -> Result<D1Result> {
        s.inner.run().await.map_err(|e| map_error(s.name, &e))
    }

    /// Runs statements as one transaction: all of them or none.
    pub(crate) async fn batch(
        &self,
        what: &'static str,
        stmts: Vec<Stmt>,
    ) -> Result<Vec<D1Result>> {
        self.batch_guarded(what, stmts).await.map_err(|f| match f {
            Failure::Error(e) => e,
            Failure::Guard => storage(what, "guard fired in a batch without a guard"),
        })
    }

    /// Like [`Self::batch`], but tells a fired [`guard`] apart from other errors.
    pub(crate) async fn batch_guarded(
        &self,
        what: &'static str,
        stmts: Vec<Stmt>,
    ) -> std::result::Result<Vec<D1Result>, Failure> {
        let names: Vec<&str> = stmts.iter().map(|s| s.name).collect();
        let n = stmts.len();
        let results = self
            .db
            .batch(stmts.into_iter().map(|s| s.inner).collect())
            .await
            .map_err(|e| {
                let msg = message(&e);
                if msg.contains(GUARD_MESSAGE) {
                    Failure::Guard
                } else {
                    // D1 does not say which statement failed; name the batch and its parts.
                    Failure::Error(classify(&format!("{what} [{}]", names.join(", ")), &msg))
                }
            })?;
        if results.len() != n {
            return Err(Failure::Error(storage(
                what,
                &format!(
                    "batch returned {} results for {n} statements",
                    results.len()
                ),
            )));
        }
        Ok(results)
    }

    /// Rows changed by a write result.
    pub(crate) fn changes(name: &'static str, r: &D1Result) -> Result<u64> {
        let meta = Self::meta(name, r)?;
        meta.changes
            .and_then(|c| u64::try_from(c).ok())
            .ok_or_else(|| storage(name, "D1 result has no change count"))
    }

    /// Row id of the row an INSERT added.
    pub(crate) fn last_row_id(name: &'static str, r: &D1Result) -> Result<i64> {
        let meta = Self::meta(name, r)?;
        meta.last_row_id
            .ok_or_else(|| storage(name, "D1 result has no last row id"))
    }

    fn meta(name: &'static str, r: &D1Result) -> Result<worker::D1ResultMeta> {
        r.meta()
            .map_err(|e| storage(name, &message(&e)))?
            .ok_or_else(|| storage(name, "D1 result has no meta"))
    }

    /// Rows returned by one statement of a batch (e.g. `RETURNING`), as `(column, i64)` maps.
    pub(crate) fn batch_ints(
        name: &'static str,
        r: &D1Result,
    ) -> Result<Vec<std::collections::HashMap<String, i64>>> {
        r.results().map_err(|e| storage(name, &message(&e)))
    }
}

/// A batch statement that aborts the whole batch unless `condition` (an SQL boolean over
/// `params`) holds. It tries to set `site.id` to 0, which `CHECK (id = 1)` rejects.
pub(crate) fn guard(
    store: &D1Store,
    name: &'static str,
    condition: &str,
    params: &[JsValue],
) -> Result<Stmt> {
    store.stmt(
        name,
        &format!("UPDATE site SET id = 0 WHERE id = 1 AND NOT ({condition})"),
        params,
    )
}

/// The SQL condition that `proof` still holds for `user` at `now`, and its parameters.
pub(crate) fn proof_condition(
    user: UserId,
    proof: &Proof,
    now: i64,
) -> Result<(&'static str, Vec<JsValue>)> {
    Ok(match proof {
        Proof::Session(h) => (
            "EXISTS (SELECT 1 FROM sessions WHERE token_hash = ? AND user_id = ?)",
            vec![text(h), bound(user.0)],
        ),
        Proof::Password(h) => (
            "EXISTS (SELECT 1 FROM users WHERE id = ? AND password_hash = ?)",
            vec![bound(user.0), text(h)],
        ),
        Proof::Passkey(id) => (
            "EXISTS (SELECT 1 FROM passkeys WHERE id = ? AND user_id = ?)",
            vec![bound(*id), bound(user.0)],
        ),
        // Both values exactly JSON integers, as `take_owner_reset` reads them.
        Proof::ResetCode(h) => (
            "EXISTS (SELECT 1 FROM settings h, settings u, settings e \
             WHERE h.key = ? AND h.value_json = ? AND u.key = ? AND u.value_json = ? \
             AND e.key = ? AND CAST(CAST(e.value_json AS INTEGER) AS TEXT) = e.value_json \
             AND CAST(e.value_json AS INTEGER) > ?)",
            vec![
                text(OWNER_RESET_HASH_KEY),
                text(&setup_hash_json(h)),
                text(OWNER_RESET_USER_KEY),
                text(&user.0.to_string()),
                text(OWNER_RESET_EXPIRES_KEY),
                int(now)?,
            ],
        ),
        #[cfg(feature = "contract-tests")]
        Proof::Unchecked => ("1", Vec::new()),
    })
}

/// A [`guard`] that aborts its batch unless `proof` holds for `user` at `now`.
pub(crate) fn proof_guard(store: &D1Store, user: UserId, proof: &Proof, now: i64) -> Result<Stmt> {
    let (condition, params) = proof_condition(user, proof, now)?;
    guard(store, "proof_guard", condition, &params)
}

/// What a write gives when the credential its request authenticated with is gone.
pub(crate) fn proof_gone() -> Error {
    Error::new(
        ErrorCode::Unauthenticated,
        "the credential this request authenticated with is gone",
    )
}

impl D1Store {
    /// Whether `proof` holds for `user` at `now` (to name why a guarded batch was refused).
    pub(crate) async fn proof_holds(&self, user: UserId, proof: &Proof, now: i64) -> Result<bool> {
        let (condition, params) = proof_condition(user, proof, now)?;
        let s = self.stmt(
            "proof_holds",
            &format!("SELECT ({condition}) AS ok"),
            &params,
        )?;
        Ok(self.row(s).await?.map(|mut r| r.int()).transpose()? == Some(1))
    }

    /// Inserts with `insert` (one row) only if `proof` holds, in one batch; the new row id.
    pub(crate) async fn insert_proven(
        &self,
        what: &'static str,
        user: UserId,
        proof: &Proof,
        now: i64,
        insert: Stmt,
    ) -> Result<i64> {
        let g = proof_guard(self, user, proof, now)?;
        match self.batch_guarded(what, vec![g, insert]).await {
            Ok(r) => Self::last_row_id(what, &r[1]),
            Err(Failure::Guard) => Err(proof_gone()),
            Err(Failure::Error(e)) => Err(e),
        }
    }
}

/// The statement that bumps `site.content_version`, when `condition` holds.
pub(crate) fn bump_if(store: &D1Store, condition: &str, params: &[JsValue]) -> Result<Stmt> {
    store.stmt(
        "bump_version",
        &format!(
            "UPDATE site SET content_version = content_version + 1 WHERE id = 1 AND ({condition})"
        ),
        params,
    )
}

/// The statement that always bumps `site.content_version`.
pub(crate) fn bump(store: &D1Store) -> Result<Stmt> {
    bump_if(store, "1", &[])
}

/// Everything D1 says about an error: its message and, for D1 errors, the SQLite cause.
fn message(e: &worker::Error) -> String {
    match e {
        worker::Error::D1(d) => {
            let js: &worker::js_sys::Error = d.as_ref();
            format!("{} ({})", String::from(js.message()), d.cause())
        }
        other => other.to_string(),
    }
}

fn map_error(name: &str, e: &worker::Error) -> Error {
    classify(name, &message(e))
}

/// Constraint errors are the caller's fault; anything else is a storage failure.
fn classify(name: &str, msg: &str) -> Error {
    let code = if msg.contains("UNIQUE constraint failed") {
        ErrorCode::Conflict
    } else if msg.contains("FOREIGN KEY constraint failed")
        || msg.contains("CHECK constraint failed")
    {
        ErrorCode::InvalidInput
    } else {
        ErrorCode::Storage
    };
    Error::new(code, format!("{name}: {msg}"))
}

pub(crate) fn storage(name: &str, msg: &str) -> Error {
    Error::new(ErrorCode::Storage, format!("{name}: {msg}"))
}

pub(crate) fn not_found(what: &str, id: impl std::fmt::Display) -> Error {
    Error::new(ErrorCode::NotFound, format!("{what} {id} not found"))
}

pub(crate) fn invalid(detail: impl Into<String>) -> Error {
    Error::new(ErrorCode::InvalidInput, detail)
}

/// Binds an integer. D1 takes JS numbers, exact only up to 2^53; larger is refused rather
/// than silently rounded.
pub(crate) fn int(n: i64) -> Result<JsValue> {
    worker::js_sys::Number::try_from(n)
        .map(JsValue::from)
        .map_err(|_| invalid(format!("integer {n} is too large to store")))
}

/// Binds an unsigned integer (log times).
pub(crate) fn uint(n: u64) -> Result<JsValue> {
    worker::js_sys::Number::try_from(n)
        .map(JsValue::from)
        .map_err(|_| invalid(format!("integer {n} is too large to store")))
}

/// Binds an integer that is only compared, never stored. Stored integers are within
/// +-(2^53 - 1) (see [`int`]), so clamping to +-2^53 (exact as f64) compares the same.
pub(crate) fn bound(n: i64) -> JsValue {
    const EDGE: f64 = 9_007_199_254_740_992.0;
    match int(n) {
        Ok(v) => v,
        Err(_) if n > 0 => JsValue::from_f64(EDGE),
        Err(_) => JsValue::from_f64(-EDGE),
    }
}

/// [`bound`] for unsigned values (log times).
pub(crate) fn ubound(n: u64) -> JsValue {
    uint(n).unwrap_or_else(|_| JsValue::from_f64(9_007_199_254_740_992.0))
}

pub(crate) fn opt_int(n: Option<i64>) -> Result<JsValue> {
    n.map_or(Ok(JsValue::NULL), int)
}

pub(crate) fn text(s: &str) -> JsValue {
    JsValue::from_str(s)
}

pub(crate) fn opt_text(s: Option<&str>) -> JsValue {
    s.map_or(JsValue::NULL, JsValue::from_str)
}

pub(crate) fn flag(b: bool) -> JsValue {
    JsValue::from_f64(if b { 1.0 } else { 0.0 })
}

/// `LIMIT` value: the page length capped at `MAX_PAGE`.
pub(crate) fn limit(n: u32) -> JsValue {
    JsValue::from(n.min(plumblog_store::MAX_PAGE))
}

/// `LIMIT` value for a list paged by a probe row (see `plumblog_store::probe_len`).
pub(crate) fn probe_limit(n: u32) -> JsValue {
    JsValue::from(n.min(plumblog_store::MAX_PAGE + 1))
}

/// How many times a read-then-write method retries when a guard fires: each retry re-reads,
/// so it either succeeds on fresh data or returns the error that data calls for.
pub(crate) const GUARD_RETRIES: usize = 3;

pub(crate) fn changed_concurrently(what: &str) -> Error {
    Error::new(
        ErrorCode::Conflict,
        format!("{what}: the row kept changing during the write; try again"),
    )
}

#[cfg(feature = "contract-tests")]
impl D1Store {
    /// Empties every table and resets the site row, so a contract case starts from a fresh
    /// migration. Children before parents so no foreign key blocks a delete.
    ///
    /// # Errors
    ///
    /// `Storage` if D1 refuses any statement; then nothing was deleted.
    pub async fn wipe(&self) -> Result<()> {
        const TABLES: &[&str] = &[
            "post_tags",
            "post_media",
            "revisions",
            "comments",
            "posts_fts",
            "posts",
            "redirects",
            "sessions",
            "passkeys",
            "api_tokens",
            "invites",
            "login_failures",
            "auth_challenges",
            "media",
            "series",
            "tags",
            "settings",
            "extension_options",
            "plugins",
            "webhook_deliveries",
            "webhooks",
            "system_log",
            "users",
        ];
        let mut stmts = Vec::with_capacity(TABLES.len() + 1);
        for t in TABLES {
            stmts.push(self.stmt("wipe", &format!("DELETE FROM {t}"), &[])?);
        }
        stmts.push(self.stmt(
            "wipe_site",
            "UPDATE site SET content_version = 1 WHERE id = 1",
            &[],
        )?);
        self.batch("wipe", stmts).await.map(drop)
    }
}
