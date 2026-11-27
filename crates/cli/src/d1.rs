//! D1 over the REST query endpoint, and the settings writes `blog` shares with the Worker.
//!
//! Query: <https://developers.cloudflare.com/api/resources/d1/subresources/database/methods/query/>
//! The REST docs do not say a multi-statement request is atomic (only the Worker binding's
//! `batch()` is documented as a transaction), so every write here is safe to repeat and
//! ordered so a half-done request leaves nothing harmful.

use crate::cloudflare::{ApiError, ApiErrorKind, Call, Client, Endpoint};
use plumblog_store::{SETUP_CODE_EXPIRES_KEY, SETUP_CODE_HASH_KEY, SITE_ID_KEY, setup_hash_json};
use serde::Deserialize;
use serde_json::{Map, Value, json};

/// One statement's outcome in a query reply.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct StatementResult {
    #[serde(default)]
    pub results: Vec<Map<String, Value>>,
    #[serde(default)]
    pub success: bool,
    #[serde(default)]
    pub meta: Value,
}

impl StatementResult {
    /// Rows changed, from `meta.changes`.
    #[must_use]
    pub fn changes(&self) -> u64 {
        self.meta
            .get("changes")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    }
}

/// A D1 database reached through the REST API.
pub struct Database<'a> {
    client: &'a Client,
    id: String,
}

/// Binds as D1's REST params: the docs type them as strings, and SQLite's column affinity
/// stores numeric text in INTEGER columns as integers, as the Worker's own binds do.
fn params(values: &[&str]) -> Value {
    Value::Array(values.iter().map(|v| json!(v)).collect())
}

fn malformed(detail: String) -> ApiError {
    ApiError {
        kind: ApiErrorKind::Malformed,
        endpoint: Some(Endpoint::D1Query),
        status: None,
        errors: Vec::new(),
        detail,
        tries: 1,
    }
}

impl<'a> Database<'a> {
    #[must_use]
    pub fn new(client: &'a Client, id: impl Into<String>) -> Self {
        Self {
            client,
            id: id.into(),
        }
    }

    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    fn send(&self, body: Value) -> Result<Vec<StatementResult>, ApiError> {
        let call = Call::new(Endpoint::D1Query)
            .param("database_id", self.id.clone())
            .json(body);
        let results: Vec<StatementResult> = self.client.call_as(&call)?;
        if let Some(i) = results.iter().position(|r| !r.success) {
            return Err(malformed(format!("statement {} did not succeed", i + 1)));
        }
        Ok(results)
    }

    /// `sql` as one request: several statements joined by semicolons run in order (wrangler
    /// sends a migration file the same way, as `{sql}`).
    ///
    /// # Errors
    /// The API error, or `Malformed` if a statement reports no success.
    pub fn query(&self, sql: &str, values: &[&str]) -> Result<Vec<StatementResult>, ApiError> {
        let mut body = json!({ "sql": sql });
        if !values.is_empty() {
            body["params"] = params(values);
        }
        self.send(body)
    }

    /// Several statements, each with its own params, in one request (`{batch: [...]}`).
    ///
    /// # Errors
    /// As [`Database::query`].
    pub fn batch(&self, stmts: &[(&str, &[&str])]) -> Result<Vec<StatementResult>, ApiError> {
        let batch: Vec<Value> = stmts
            .iter()
            .map(|(sql, values)| json!({ "sql": sql, "params": params(values) }))
            .collect();
        let results = self.send(json!({ "batch": batch }))?;
        if results.len() != stmts.len() {
            return Err(malformed(format!(
                "{} results for {} statements",
                results.len(),
                stmts.len()
            )));
        }
        Ok(results)
    }

    /// Rows of a single read.
    ///
    /// # Errors
    /// As [`Database::query`].
    pub fn rows(&self, sql: &str, values: &[&str]) -> Result<Vec<Map<String, Value>>, ApiError> {
        Ok(self
            .query(sql, values)?
            .into_iter()
            .next()
            .map(|r| r.results)
            .unwrap_or_default())
    }
}

// --- Settings, written exactly as crates/worker/src/d1 writes them ---

/// `put_setting`'s upsert (crates/worker/src/d1/settings.rs).
const PUT_SETTING: &str = "INSERT INTO settings (key, value_json, updated_at) VALUES (?, ?, ?) \
     ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json, \
     updated_at = excluded.updated_at";
/// `bump` (crates/worker/src/d1/mod.rs): new settings must not be served from old cache keys.
const BUMP: &str = "UPDATE site SET content_version = content_version + 1 WHERE id = 1 AND (1)";
/// `ensure_site_id`'s insert (crates/worker/src/d1/signin.rs): never overwrites.
const ENSURE_SITE_ID: &str = "INSERT INTO settings (key, value_json, updated_at) VALUES (?, ?, ?) \
     ON CONFLICT (key) DO NOTHING";
/// `put_setup_code`'s upsert (crates/worker/src/d1/signin.rs), written only while no user
/// exists: an owner created meanwhile (with an older code) makes it write nothing.
const PUT_SETUP_CODE: &str = "INSERT INTO settings (key, value_json, updated_at) \
     SELECT ?, ?, ? WHERE NOT EXISTS (SELECT 1 FROM users) \
     ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json, \
     updated_at = excluded.updated_at";

/// `value_json` of the given settings keys that exist.
///
/// # Errors
/// As [`Database::query`].
pub fn settings(db: &Database, keys: &[&str]) -> Result<Vec<(String, String)>, ApiError> {
    let marks = vec!["?"; keys.len()].join(", ");
    let rows = db.rows(
        &format!("SELECT key, value_json FROM settings WHERE key IN ({marks}) ORDER BY key"),
        keys,
    )?;
    rows.iter()
        .map(|r| {
            match (
                r.get("key").and_then(Value::as_str),
                r.get("value_json").and_then(Value::as_str),
            ) {
                (Some(k), Some(v)) => Ok((k.to_string(), v.to_string())),
                _ => Err(malformed("a settings row without key or value_json".into())),
            }
        })
        .collect()
}

/// The Worker's `put_setting`: upsert plus the content-version bump. The bump goes first: if
/// the request stops halfway, an extra bump is harmless, while a value without one could be
/// served from old cache keys.
///
/// # Errors
/// As [`Database::batch`].
pub fn put_setting(db: &Database, key: &str, value_json: &str, now: i64) -> Result<(), ApiError> {
    let now = now.to_string();
    db.batch(&[(BUMP, &[]), (PUT_SETTING, &[key, value_json, &now])])
        .map(drop)
}

/// `delete_setting`'s bump condition and delete (crates/worker/src/d1/settings.rs).
const BUMP_IF_SETTING: &str = "UPDATE site SET content_version = content_version + 1 \
     WHERE id = 1 AND (EXISTS (SELECT 1 FROM settings WHERE key = ?))";
const DELETE_SETTING: &str = "DELETE FROM settings WHERE key = ?";

/// The Worker's `delete_setting`: the bump (only if the key exists) and the delete in one
/// request, bump first as in [`put_setting`].
///
/// # Errors
/// As [`Database::batch`].
pub fn delete_setting(db: &Database, key: &str) -> Result<(), ApiError> {
    db.batch(&[(BUMP_IF_SETTING, &[key]), (DELETE_SETTING, &[key])])
        .map(drop)
}

/// The Worker's `ensure_site_id` write: stores `value_json` only if no site id exists.
///
/// # Errors
/// As [`Database::query`].
pub fn ensure_site_id(db: &Database, value_json: &str, now: i64) -> Result<(), ApiError> {
    db.query(ENSURE_SITE_ID, &[SITE_ID_KEY, value_json, &now.to_string()])
        .map(drop)
}

/// Whether the users table has any row.
///
/// # Errors
/// As [`Database::query`].
pub fn has_users(db: &Database) -> Result<bool, ApiError> {
    Ok(!db.rows("SELECT 1 AS x FROM users LIMIT 1", &[])?.is_empty())
}

/// The Worker's `put_setup_code` writes (hash first, then expiry, as it does), guarded by
/// "no user yet". Returns false when a user exists and nothing was written.
///
/// Hash before expiry: if the request stops between them, the new hash keeps the old expiry
/// (never longer than an old code was valid); the other order could extend an old code.
///
/// # Errors
/// As [`Database::batch`].
pub fn put_setup_code(
    db: &Database,
    code_hash: &str,
    expires_at: i64,
    now: i64,
) -> Result<bool, ApiError> {
    let now = now.to_string();
    let hash = setup_hash_json(code_hash);
    let expires = expires_at.to_string();
    let results = db.batch(&[
        (PUT_SETUP_CODE, &[SETUP_CODE_HASH_KEY, &hash, &now]),
        (PUT_SETUP_CODE, &[SETUP_CODE_EXPIRES_KEY, &expires, &now]),
    ])?;
    Ok(results[0].changes() > 0)
}
