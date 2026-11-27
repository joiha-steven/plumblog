//! `site.local.toml`: what `blog` created for this site on Cloudflare, so later commands
//! (`blog deploy`, `blog rollback`) know the names and ids. Git-ignored (`scripts/check`
//! fails if it is tracked).
//!
//! It never holds the API token, Worker secrets or the setup code: [`save`] refuses to write
//! text containing any secret it is given. Cloudflare stays the truth: `blog setup` looks every
//! resource up by name again, so a lost or stale file is never a problem.

use crate::cloudflare::ApiToken;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const FILE: &str = "site.local.toml";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct D1State {
    pub name: String,
    pub id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct R2State {
    pub bucket: String,
}

/// A migration that failed, and the bookmark from before its run: the database may be half
/// migrated until it is restored to that bookmark.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedMigration {
    pub name: String,
    pub bookmark: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MigrationsState {
    /// Every migration recorded in the database, in order.
    #[serde(default)]
    pub applied: Vec<String>,
    /// The bookmark taken before the last run that applied migrations: what
    /// `blog rollback --db <bookmark> --yes` restores.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_bookmark: Option<String>,
    /// Set when a migration fails; while set, no migration runs and `last_bookmark` is
    /// never replaced. Cleared by `blog rollback --db` or `blog setup --clear-failed-migration`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<FailedMigration>,
}

/// One `blog deploy` run, written before anything changes and updated after each step, so
/// a run that stops halfway still says what it did.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeployRecord {
    /// UTC, `2026-11-28T10:15:00Z`.
    pub at: String,
    /// `started`, `migration_failed`, `failed` (nothing switched), `deployed`,
    /// `deployed_cron_failed` (serving, cron not set), `deploy_uncertain` (switch sent, the
    /// active version unreadable) or `rolled_back`.
    pub outcome: String,
    /// The Time Travel bookmark taken before anything changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bookmark: Option<String>,
    /// The SQL export, relative to the site folder (`backups/...`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub migrations: Vec<String>,
    /// The uploaded Worker version id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// The version that served the site before this deploy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
    /// `passed`, or what the smoke check saw when it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smoke: Option<String>,
    /// Every migration the bundle contained: `blog rollback` compares it with `d1_migrations`
    /// before going back to this version. `None` in records older than this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_migrations: Option<Vec<String>>,
}

/// One `blog rollback`: code (`kind = "version"`) or database (`kind = "database"`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollbackRecord {
    pub at: String,
    pub kind: String,
    /// The version id or bookmark restored.
    pub to: String,
    /// What it replaced: the version that was serving, or Time Travel's `previous_bookmark`
    /// (restoring that undoes the restore).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smoke: Option<String>,
}

/// Records kept per list; older ones are dropped.
pub const MAX_RECORDS: usize = 20;

/// The file's content. Plain values first, tables last (TOML requires it).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// Worker script name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worker: Option<String>,
    /// `https://host`, as stored in the site's `origin` setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
    /// Custom domain host, when the site has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// Public Turnstile site key (the secret is a Worker secret only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turnstile_site_key: Option<String>,
    /// The version `blog setup` uploaded (503, no body): never a rollback target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder_version: Option<String>,
    /// Value of the script's `PLUMBLOG_SITE_ID` plain-text binding: marks the script as made
    /// by `blog` for this site, so setup never adopts someone else's Worker of the same name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_marker: Option<String>,
    /// The custom domain has served a version once: from then on it is the smoke target.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub domain_smoke_passed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub d1: Option<D1State>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r2: Option<R2State>,
    #[serde(default)]
    pub migrations: MigrationsState,
    /// Newest last.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deploys: Vec<DeployRecord>,
    /// Newest last.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rollbacks: Vec<RollbackRecord>,
}

/// Appends `item`, keeping the newest [`MAX_RECORDS`].
pub fn push_capped<T>(list: &mut Vec<T>, item: T) {
    list.push(item);
    if list.len() > MAX_RECORDS {
        list.drain(..list.len() - MAX_RECORDS);
    }
}

/// Why the file could not be read or written: `(path, reason)`.
#[derive(Debug)]
pub enum StateError {
    Unreadable(String, String),
    Unwritable(String, String),
}

#[must_use]
pub fn path(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

/// The saved state; `None` if there is no file yet.
///
/// # Errors
/// [`StateError::Unreadable`] if the file exists but cannot be read or parsed.
pub fn load(dir: &Path) -> Result<Option<SiteState>, StateError> {
    let p = path(dir);
    let unreadable = |reason: String| StateError::Unreadable(p.display().to_string(), reason);
    let text = match fs::read_to_string(&p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(unreadable(e.to_string())),
    };
    toml::from_str(&text)
        .map(Some)
        .map_err(|e| unreadable(e.message().to_string()))
}

/// Writes the state atomically (temp file, then rename) with mode 0600 on Unix, so a crash
/// leaves the old file or the new one, never half of one.
///
/// # Errors
/// [`StateError::Unwritable`]; also when the text would contain one of `secrets`, which
/// would be a bug, so nothing is written.
pub fn save(dir: &Path, state: &SiteState, secrets: &[&ApiToken]) -> Result<(), StateError> {
    let p = path(dir);
    let fail = |reason: String| StateError::Unwritable(p.display().to_string(), reason);
    let text = toml::to_string(state).map_err(|e| fail(e.to_string()))?;
    if secrets
        .iter()
        .any(|s| !s.expose().is_empty() && text.contains(s.expose()))
    {
        return Err(fail("refusing to write a secret value into it".into()));
    }
    let tmp = dir.join(format!(".{FILE}.{}.tmp", std::process::id()));
    let write = || -> std::io::Result<()> {
        // A leftover temp file would keep its old mode: `mode` applies only on creation.
        let _ = fs::remove_file(&tmp);
        let mut opts = fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        drop(f);
        // The renamed file keeps the temp file's 0600, even over an older, wider file.
        fs::rename(&tmp, &p)
    };
    write().map_err(|e| {
        let _ = fs::remove_file(&tmp);
        fail(e.to_string())
    })
}

/// Read-modify-write: loads the file as it is now, applies `change`, saves atomically and
/// returns what was saved. Callers hold the folder lock, so the only other writer is a person
/// editing the file; their edit survives because nothing writes back an older copy.
///
/// # Errors
/// As [`load`] and [`save`].
pub fn update(
    dir: &Path,
    secrets: &[&ApiToken],
    change: impl FnOnce(&mut SiteState),
) -> Result<SiteState, StateError> {
    let mut st = load(dir)?.unwrap_or_default();
    change(&mut st);
    save(dir, &st, secrets)?;
    Ok(st)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("plumblog-state-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn round_trips_and_is_private() {
        let d = dir("rt");
        assert_eq!(load(&d).unwrap(), None);
        let s = SiteState {
            account_id: Some("acc1".into()),
            worker: Some("blog".into()),
            d1: Some(D1State {
                name: "blog".into(),
                id: "db-1".into(),
            }),
            migrations: MigrationsState {
                applied: vec!["0001_init.sql".into()],
                last_bookmark: Some("bm-1".into()),
                failed: Some(FailedMigration {
                    name: "0002_auth.sql".into(),
                    bookmark: "bm-1".into(),
                }),
            },
            ..SiteState::default()
        };
        save(&d, &s, &[]).unwrap();
        assert_eq!(load(&d).unwrap(), Some(s));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(path(&d)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        // No temp file is left behind.
        assert_eq!(fs::read_dir(&d).unwrap().count(), 1);
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn refuses_to_write_a_secret() {
        let d = dir("secret");
        let secret = ApiToken::new("s3cr3t-value").unwrap();
        let s = SiteState {
            worker: Some("s3cr3t-value".into()),
            ..SiteState::default()
        };
        assert!(matches!(
            save(&d, &s, &[&secret]),
            Err(StateError::Unwritable(..))
        ));
        assert!(!path(&d).exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_broken_file_is_an_error_not_a_reset() {
        let d = dir("broken");
        fs::write(path(&d), "worker = [").unwrap();
        assert!(matches!(load(&d), Err(StateError::Unreadable(..))));
        let _ = fs::remove_dir_all(&d);
    }
}
