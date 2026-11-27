//! D1 migrations over the REST API, tracked the way wrangler tracks them, so a contributor's
//! `wrangler d1 migrations apply` and `blog` agree on what is applied.
//!
//! Table, columns and names come from wrangler 4.160.0 (`getCreateMigrationsTableQuery`,
//! `buildMigrationQuery` in wrangler-dist/cli.js): `d1_migrations(id, name, applied_at)`, one
//! row per file, `name` = the file name (`0001_init.sql`), files in number order.
//!
//! Differences from wrangler, on purpose: wrangler appends its `INSERT INTO d1_migrations` to
//! the migration's own request. The REST batch is not documented as atomic, so here the row
//! is written only after the migration's request succeeded, and a Time Travel bookmark is
//! taken before anything is applied, so a half-applied migration can be rolled back.

use crate::cloudflare::{ApiError, Call, Endpoint};
use crate::d1::Database;
use serde_json::Value;

include!(concat!(env!("OUT_DIR"), "/migrations.rs"));

/// wrangler's default table name.
pub const TABLE: &str = "d1_migrations";

/// wrangler's `CREATE TABLE` for the table, byte for byte.
const CREATE_TABLE: &str = "CREATE TABLE IF NOT EXISTS \"d1_migrations\"(\n\
\t\tid         INTEGER PRIMARY KEY AUTOINCREMENT,\n\
\t\tname       TEXT UNIQUE,\n\
\t\tapplied_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP NOT NULL\n\
);";

/// One migration file.
#[derive(Clone, Debug)]
pub struct Migration {
    /// File name, e.g. `0001_init.sql`.
    pub name: String,
    pub sql: String,
}

/// The repo's `migrations/*.sql`, embedded at build time, in file-name order (the names are
/// zero-padded numbers, so this is wrangler's number order too).
#[must_use]
pub fn bundled() -> Vec<Migration> {
    MIGRATIONS
        .iter()
        .map(|(name, sql)| Migration {
            name: (*name).to_string(),
            sql: (*sql).to_string(),
        })
        .collect()
}

/// What [`apply`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Applied by this run, in order.
    pub applied: Vec<String>,
    /// Every migration recorded in the database afterwards, in order.
    pub recorded: Vec<String>,
    /// Taken before the first migration of this run; `None` when nothing was pending.
    pub bookmark: Option<String>,
}

/// Why [`apply`] stopped.
#[derive(Debug)]
pub enum Error {
    /// Before any migration was sent: nothing changed by this run.
    Before(ApiError),
    /// The database has tables but no `d1_migrations`: not made by `blog` or wrangler, so
    /// it is left alone. Holds the table names.
    Foreign(Vec<String>),
    /// Migration `name` failed. Its request may have been applied partly (not atomic);
    /// `sql_succeeded`: the migration ran but writing its `d1_migrations` row did not.
    Failed {
        name: String,
        error: Box<ApiError>,
        bookmark: String,
        applied: Vec<String>,
        sql_succeeded: bool,
    },
}

/// Tables the database's owner made: not SQLite's own (`sqlite_*`), not D1's (`_cf_*`).
///
/// # Errors
/// As [`Database::query`].
pub fn user_tables(db: &Database) -> Result<Vec<String>, ApiError> {
    let rows = db.rows(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite\\_%' \
         ESCAPE '\\' AND name NOT LIKE '\\_cf\\_%' ESCAPE '\\' ORDER BY name",
        &[],
    )?;
    Ok(rows
        .iter()
        .filter_map(|r| r.get("name").and_then(Value::as_str).map(str::to_string))
        .collect())
}

/// Refuses a database someone else made: tables, but no `d1_migrations`.
///
/// # Errors
/// [`Error::Foreign`], or [`Error::Before`] if the check itself failed.
pub fn check_ours(db: &Database) -> Result<(), Error> {
    let tables = user_tables(db).map_err(Error::Before)?;
    if tables.is_empty() || tables.iter().any(|t| t == TABLE) {
        Ok(())
    } else {
        Err(Error::Foreign(tables))
    }
}

/// Names recorded in `d1_migrations`, in order; empty if the table does not exist.
///
/// # Errors
/// As [`Database::query`].
pub fn recorded(db: &Database) -> Result<Vec<String>, ApiError> {
    let exists = db.rows(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = ?",
        &[TABLE],
    )?;
    if exists.is_empty() {
        return Ok(Vec::new());
    }
    // wrangler's `getListAppliedMigrationsQuery` orders by id.
    let rows = db.rows("SELECT name FROM \"d1_migrations\" ORDER BY id", &[])?;
    Ok(rows
        .iter()
        .filter_map(|r| r.get("name").and_then(Value::as_str).map(str::to_string))
        .collect())
}

/// A Time Travel bookmark for the database as it is now.
/// <https://developers.cloudflare.com/api/resources/d1/subresources/database/subresources/time_travel/methods/get_bookmark/>
///
/// # Errors
/// The API error, or `Malformed` if the reply has no bookmark.
pub fn bookmark(db: &Database, client: &crate::cloudflare::Client) -> Result<String, ApiError> {
    let call = Call::new(Endpoint::D1BookmarkGet).param("database_id", db.id().to_string());
    let result = client.call(&call)?;
    result
        .get("bookmark")
        .and_then(Value::as_str)
        .filter(|b| !b.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ApiError {
            kind: crate::cloudflare::ApiErrorKind::Malformed,
            endpoint: Some(Endpoint::D1BookmarkGet),
            status: None,
            errors: Vec::new(),
            detail: "no bookmark in the reply".into(),
            tries: 1,
        })
}

/// Applies every migration not yet recorded, in order, one request each; records each only
/// after its request succeeded. Takes a bookmark first whenever anything is pending, also
/// for a new database: its bookmark is the clean state to restore if the first migration
/// fails halfway.
///
/// # Errors
/// [`Error::Before`] if nothing was applied; [`Error::Foreign`] for a database `blog` did not
/// make; [`Error::Failed`] with the migration's name and
/// the bookmark otherwise.
pub fn apply(
    db: &Database,
    client: &crate::cloudflare::Client,
    migrations: &[Migration],
) -> Result<Report, Error> {
    check_ours(db)?;
    let mut done = recorded(db).map_err(Error::Before)?;
    let pending: Vec<&Migration> = migrations
        .iter()
        .filter(|m| !done.contains(&m.name))
        .collect();
    if pending.is_empty() {
        return Ok(Report {
            applied: Vec::new(),
            recorded: done,
            bookmark: None,
        });
    }
    let mark = bookmark(db, client).map_err(Error::Before)?;
    db.query(CREATE_TABLE, &[]).map_err(Error::Before)?;
    let mut applied = Vec::new();
    for m in pending {
        let fail = |error, sql_succeeded, applied: &Vec<String>| Error::Failed {
            name: m.name.clone(),
            error: Box::new(error),
            bookmark: mark.clone(),
            applied: applied.clone(),
            sql_succeeded,
        };
        if let Err(e) = db.query(&m.sql, &[]) {
            return Err(fail(e, false, &applied));
        }
        // wrangler's row: `INSERT INTO d1_migrations (name) values ('<name>')`, bound here.
        if let Err(e) = db.query(
            "INSERT INTO \"d1_migrations\" (name) values (?)",
            &[&m.name],
        ) {
            return Err(fail(e, true, &applied));
        }
        applied.push(m.name.clone());
        done.push(m.name.clone());
    }
    Ok(Report {
        applied,
        recorded: done,
        bookmark: Some(mark),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_migrations_are_the_repo_files_in_order() {
        let names: Vec<String> = bundled().into_iter().map(|m| m.name).collect();
        let mut files: Vec<String> =
            std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../../migrations"))
                .unwrap()
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .filter(|n| {
                    std::path::Path::new(n)
                        .extension()
                        .is_some_and(|x| x == "sql")
                })
                .collect();
        files.sort();
        assert_eq!(names, files);
        assert!(names[0].starts_with("0001_"));
    }
}
