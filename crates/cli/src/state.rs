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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub d1: Option<D1State>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r2: Option<R2State>,
    #[serde(default)]
    pub migrations: MigrationsState,
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
