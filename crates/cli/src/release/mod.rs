//! Releases (issue #21; docs/ARCHITECTURE.md, Release): the trust root baked into `blog`, the
//! release bundle `blog deploy` downloads and verifies, and `blog update`.
//!
//! - [`fetch`]: downloads from GitHub Releases.
//! - [`archive`]: strict gzip, tar and zip readers.
//! - [`minisign`]: release signatures.
//! - [`update`]: `blog update`.

pub mod archive;
pub mod fetch;
pub mod minisign;
pub mod reason;
pub mod update;

use crate::setup::{Failure, Run};
use crate::text::{Code, Text};
use fetch::{FetchError, Source};
use plumblog_i18n::key;
use reason::Reason;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// This binary's version (the workspace version, equal to the release tag without `v`).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// SHA-256 of the exact `plumblog-bundle-vX.Y.Z.tar.gz` this binary was released with,
/// baked by build.rs from `PLUMBLOG_BUNDLE_SHA256`; empty in a dev build.
pub const BAKED_BUNDLE_SHA256: &str = env!("PLUMBLOG_BAKED_BUNDLE_SHA256");
/// The target triple this binary was built for; names its release archive.
pub const TARGET: &str = env!("PLUMBLOG_TARGET");
/// The release signing key (`release/minisign.pub`); the placeholder means none yet.
pub const PUBLIC_KEY: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../release/minisign.pub"
));
/// Largest bundle archive and largest unpacked bundle accepted.
const MAX_BUNDLE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_UNPACKED_BYTES: usize = 512 * 1024 * 1024;
const BUNDLE_JSON: &str = "bundle.json";
const BUNDLE_TOP: &[&str] = &["worker", "assets", "migrations"];

/// `X.Y.Z`, numbers only: it becomes part of URLs and paths.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version(pub u32, pub u32, pub u32);

impl Version {
    /// `X.Y.Z` or `vX.Y.Z`.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.strip_prefix('v').unwrap_or(s);
        let mut it = s.split('.').map(|p| {
            (!p.is_empty() && p.len() <= 9 && p.bytes().all(|b| b.is_ascii_digit()))
                .then(|| p.parse().ok())
                .flatten()
        });
        let v = Self(it.next()??, it.next()??, it.next()??);
        it.next().is_none().then_some(v)
    }

    /// The release tag, `vX.Y.Z`.
    #[must_use]
    pub fn tag(self) -> String {
        format!("v{self}")
    }

    /// This binary's version.
    ///
    /// # Panics
    /// Never for a workspace version of the form `X.Y.Z` (a test checks).
    #[must_use]
    pub fn current() -> Self {
        Self::parse(VERSION).expect("workspace version is X.Y.Z")
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.0, self.1, self.2)
    }
}

/// The release bundle `blog deploy` uses by default.
#[derive(Clone, Debug)]
pub struct Release {
    pub version: Version,
    /// Lowercase hex SHA-256 of the bundle archive.
    pub sha256: String,
    pub source: Source,
    /// `<cache>/plumblog/bundles`; each version gets `vX.Y.Z/` under it.
    pub cache: PathBuf,
}

/// The user cache folder for bundles: `%LOCALAPPDATA%\plumblog\bundles` on Windows, else
/// `$XDG_CACHE_HOME/plumblog/bundles` or `~/.cache/plumblog/bundles`.
#[must_use]
pub fn cache_root(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let abs = |v: Option<String>| v.map(PathBuf::from).filter(|p| p.is_absolute());
    let base = if cfg!(windows) {
        abs(env("LOCALAPPDATA"))?
    } else {
        abs(env("XDG_CACHE_HOME")).or_else(|| abs(env("HOME")).map(|h| h.join(".cache")))?
    };
    Some(base.join("plumblog").join("bundles"))
}

/// The release this binary deploys: `None` in a dev build (no baked hash).
///
/// # Errors
/// A refused `PLUMBLOG_RELEASE_BASE`, or no cache folder.
pub fn baked(text: &Text) -> Result<Option<Release>, Failure> {
    if BAKED_BUNDLE_SHA256.is_empty() {
        return Ok(None);
    }
    let source = Source::from_env().map_err(|e| fetch_failure(&e, text))?;
    let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
    let cache = cache_root(&env).ok_or_else(|| {
        Failure::new(
            Code::ReleaseCacheUnwritable,
            text,
            key!("cli.error.release_cache_unwritable"),
            &[
                (
                    "path",
                    if cfg!(windows) {
                        "%LOCALAPPDATA%"
                    } else {
                        "~/.cache"
                    }
                    .into(),
                ),
                ("reason", Reason::NotSet.render(text)),
            ],
        )
    })?;
    Ok(Some(Release {
        version: Version::current(),
        sha256: BAKED_BUNDLE_SHA256.into(),
        source,
        cache,
    }))
}

#[must_use]
pub fn fetch_failure(e: &FetchError, text: &Text) -> Failure {
    match e {
        FetchError::BaseRefused(v) => Failure::new(
            Code::ReleaseBaseRefused,
            text,
            key!("cli.error.release_base_refused"),
            &[("var", fetch::BASE_ENV.into()), ("value", v.clone())],
        ),
        FetchError::NotFound(url) => Failure::new(
            Code::ReleaseNotFound,
            text,
            key!("cli.error.release_not_found"),
            &[("url", url.clone()), ("page", fetch::RELEASES_PAGE.into())],
        ),
        FetchError::RedirectRefused(host) => Failure::new(
            Code::ReleaseRedirectRefused,
            text,
            key!("cli.error.release_redirect_refused"),
            &[("host", host.clone())],
        ),
        FetchError::Failed(url, detail) => Failure::new(
            Code::ReleaseDownloadFailed,
            text,
            key!("cli.error.release_download_failed"),
            &[("url", url.clone()), ("detail", detail.clone())],
        ),
    }
}

#[must_use]
pub fn sha256_hex(data: &[u8]) -> String {
    plumblog_auth::base64url::hex(&plumblog_auth::sha256::sha256(data))
}

pub(crate) fn archive_failure(e: &archive::Bad, text: &Text) -> Failure {
    Failure::new(
        Code::ReleaseArchiveInvalid,
        text,
        key!("cli.error.release_archive_invalid"),
        &[("path", e.path.clone()), ("reason", e.reason.render(text))],
    )
}

fn cache_failure(path: &Path, e: &std::io::Error, text: &Text) -> Failure {
    Failure::new(
        Code::ReleaseCacheUnwritable,
        text,
        key!("cli.error.release_cache_unwritable"),
        &[
            ("path", path.display().to_string()),
            ("reason", e.to_string()),
        ],
    )
}

/// A unique sibling name for a temp file or folder.
pub(crate) fn temp_name(dir: &Path, stem: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    dir.join(format!(
        ".{stem}-{}-{nanos}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ))
}

/// The bundle's files (bundle.json included), checked: safe paths under the bundle's top-level folders only, every
/// file listed in bundle.json with its SHA-256 and none missing, and bundle.json's version
/// equal to `want`.
///
/// # Errors
/// `release.archive_invalid` or `release.version_mismatch`.
pub fn unpack_bundle(gz: &[u8], want: Version, text: &Text) -> Result<archive::Files, Failure> {
    let tar = archive::gunzip(gz, MAX_UNPACKED_BYTES).map_err(|e| archive_failure(&e, text))?;
    let allowed = |p: &str, dir: bool| {
        (p == BUNDLE_JSON && !dir) || BUNDLE_TOP.contains(&p.split('/').next().unwrap_or(""))
    };
    let mut files = archive::untar(&tar, &allowed).map_err(|e| archive_failure(&e, text))?;
    let invalid = |path: &str, reason: Reason| {
        archive_failure(
            &archive::Bad {
                path: path.into(),
                reason,
            },
            text,
        )
    };
    let manifest = files
        .remove(BUNDLE_JSON)
        .ok_or_else(|| invalid(BUNDLE_JSON, Reason::Missing))?;
    let raw_manifest = manifest;
    let manifest: serde_json::Value = serde_json::from_slice(&raw_manifest)
        .map_err(|e| invalid(BUNDLE_JSON, Reason::Detail(e.to_string())))?;
    let version = manifest
        .get("version")
        .and_then(|v| v.as_str())
        .ok_or_else(|| invalid(BUNDLE_JSON, Reason::ManifestField("version")))?;
    if Version::parse(version) != Some(want) || version.starts_with('v') {
        return Err(Failure::new(
            Code::ReleaseVersionMismatch,
            text,
            key!("cli.error.release_version_mismatch"),
            &[
                (
                    "got",
                    version
                        .chars()
                        .filter(|c| !c.is_control())
                        .take(40)
                        .collect(),
                ),
                ("want", want.to_string()),
            ],
        ));
    }
    let listed = manifest
        .get("files")
        .and_then(|v| v.as_object())
        .ok_or_else(|| invalid(BUNDLE_JSON, Reason::ManifestField("files")))?;
    for (path, bytes) in &files {
        let want = listed
            .get(path)
            .and_then(|v| v.as_str())
            .ok_or_else(|| invalid(path, Reason::NotListed))?;
        if !want.eq_ignore_ascii_case(&sha256_hex(bytes)) {
            return Err(invalid(path, Reason::HashDiffers));
        }
    }
    if let Some(missing) = listed.keys().find(|p| !files.contains_key(*p)) {
        return Err(invalid(missing, Reason::ListedNotInArchive));
    }
    // Kept with the unpacked bundle: it says which release the folder is.
    files.insert(BUNDLE_JSON.into(), raw_manifest);
    Ok(files)
}

/// True if `dir` holds exactly `files` (regular files, no symlinks, same bytes).
fn holds(dir: &Path, files: &archive::Files) -> bool {
    fn walk(root: &Path, dir: &Path, n: &mut usize, files: &archive::Files) -> bool {
        let Ok(entries) = fs::read_dir(dir) else {
            return false;
        };
        for e in entries {
            let Ok(e) = e else { return false };
            let p = e.path();
            let Ok(meta) = fs::symlink_metadata(&p) else {
                return false;
            };
            if meta.is_dir() {
                if !walk(root, &p, n, files) {
                    return false;
                }
                continue;
            }
            let rel = p.strip_prefix(root).ok().map(|r| {
                r.components()
                    .map(|c| c.as_os_str().to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join("/")
            });
            let same = meta.is_file()
                && rel
                    .and_then(|r| files.get(&r))
                    .is_some_and(|want| fs::read(&p).is_ok_and(|got| &got == want));
            if !same {
                return false;
            }
            *n += 1;
        }
        true
    }
    let mut n = 0;
    fs::symlink_metadata(dir).is_ok_and(|m| m.is_dir())
        && walk(dir, dir, &mut n, files)
        && n == files.len()
}

fn write_tree(dir: &Path, files: &archive::Files) -> std::io::Result<()> {
    fs::create_dir_all(dir)?;
    for (rel, bytes) in files {
        let p = dir.join(rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&p)
            .and_then(|mut f| std::io::Write::write_all(&mut f, bytes))?;
    }
    Ok(())
}

/// Puts `files` at `out`: kept if it already holds exactly them, else written to a temp folder
/// and renamed into place, so a crash or a second deploy never sees half a bundle.
fn place(out: &Path, files: &archive::Files, text: &Text) -> Result<(), Failure> {
    if holds(out, files) {
        return Ok(());
    }
    let parent = out.parent().unwrap_or(Path::new("."));
    let tmp = temp_name(parent, "bundle");
    if let Err(e) = write_tree(&tmp, files) {
        let _ = fs::remove_dir_all(&tmp);
        return Err(cache_failure(&tmp, &e, text));
    }
    if fs::symlink_metadata(out).is_ok() {
        let old = temp_name(parent, "old");
        if let Err(e) = fs::rename(out, &old) {
            let _ = fs::remove_dir_all(&tmp);
            return Err(cache_failure(out, &e, text));
        }
        let _ = fs::remove_dir_all(&old).or_else(|_| fs::remove_file(&old));
    }
    match fs::rename(&tmp, out) {
        Ok(()) => Ok(()),
        // Another deploy placed the same bundle in between.
        Err(_) if holds(out, files) => {
            let _ = fs::remove_dir_all(&tmp);
            Ok(())
        }
        Err(e) => {
            let _ = fs::remove_dir_all(&tmp);
            Err(cache_failure(out, &e, text))
        }
    }
}

fn hash_mismatch(r: &Release, file: &str, got: &str, dir: &Path, text: &Text) -> Failure {
    Failure::new(
        Code::ReleaseHashMismatch,
        text,
        key!("cli.error.release_hash_mismatch"),
        &[
            ("file", file.into()),
            ("got", got.into()),
            ("want", r.sha256.clone()),
            ("dir", dir.display().to_string()),
            ("page", fetch::RELEASES_PAGE.into()),
        ],
    )
}

/// The release bundle for `r`, downloaded once into the cache and verified against the baked
/// hash before anything is unpacked; returns the unpacked folder.
///
/// # Errors
/// [`Failure`] with a `release.*` code; nothing is deployed.
pub(crate) fn bundle(r: &Release, run: &mut Run) -> Result<PathBuf, Failure> {
    let text = run.cx.text;
    let tag = r.version.tag();
    let file = format!("plumblog-bundle-{tag}.tar.gz");
    let dir = r.cache.join(&tag);
    let archive_path = dir.join(&file);
    let bytes = if archive_path.is_file() {
        let b = fs::read(&archive_path).map_err(|e| cache_failure(&archive_path, &e, text))?;
        let got = sha256_hex(&b);
        if got != r.sha256 {
            return Err(hash_mismatch(r, &file, &got, &dir, text));
        }
        run.line(
            key!("cli.release.cached"),
            &[("file", archive_path.display().to_string())],
        );
        b
    } else {
        let url = r.source.file_url(&tag, &file);
        run.line(key!("cli.release.downloading"), &[("url", url.clone())]);
        let b = r
            .source
            .get(&url, "application/octet-stream", MAX_BUNDLE_BYTES)
            .map_err(|e| fetch_failure(&e, text))?;
        let got = sha256_hex(&b);
        if got != r.sha256 {
            return Err(hash_mismatch(r, &file, &got, &dir, text));
        }
        fs::create_dir_all(&dir).map_err(|e| cache_failure(&dir, &e, text))?;
        let tmp = temp_name(&dir, "download");
        fs::write(&tmp, &b)
            .and_then(|()| fs::rename(&tmp, &archive_path))
            .map_err(|e| {
                let _ = fs::remove_file(&tmp);
                cache_failure(&archive_path, &e, text)
            })?;
        b
    };
    run.line(
        key!("cli.release.verified"),
        &[("file", file.clone()), ("hash", r.sha256.clone())],
    );
    let files = unpack_bundle(&bytes, r.version, text)?;
    let out = dir.join("bundle");
    place(&out, &files, text)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_parse_strictly_and_order() {
        assert_eq!(Version::parse("v1.2.3"), Some(Version(1, 2, 3)));
        assert_eq!(Version::parse("0.10.0"), Some(Version(0, 10, 0)));
        for bad in [
            "",
            "v",
            "1.2",
            "1.2.3.4",
            "1.2.x",
            "1..3",
            "1.2.3-rc1",
            "../1.2.3",
            "+1.2.3",
        ] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
        assert!(Version(0, 10, 0) > Version(0, 9, 9));
        assert_eq!(Version(1, 2, 3).tag(), "v1.2.3");
        assert_eq!(Version::current().to_string(), VERSION);
    }

    #[test]
    fn cache_root_follows_the_platform() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| (*v).to_string())
            }
        };
        if cfg!(windows) {
            assert!(cache_root(&env(&[("LOCALAPPDATA", "C:\\U\\AppData\\Local")])).is_some());
        } else {
            assert_eq!(
                cache_root(&env(&[("HOME", "/home/a")])).unwrap(),
                Path::new("/home/a/.cache/plumblog/bundles")
            );
            assert_eq!(
                cache_root(&env(&[("HOME", "/home/a"), ("XDG_CACHE_HOME", "/c")])).unwrap(),
                Path::new("/c/plumblog/bundles")
            );
            assert_eq!(cache_root(&env(&[("HOME", "rel")])), None);
        }
    }
}
