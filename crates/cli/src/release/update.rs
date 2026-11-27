//! `blog update`: the latest (or `--to`) release's `SHA256SUMS`, verified with the embedded
//! minisign key, names this platform's archive and its SHA-256; the new `blog` replaces the
//! running one atomically. Nothing is replaced unless every check passed.

use super::fetch::{FetchError, RELEASES_PAGE, Source};
use super::minisign::{KeyError, PublicKey, SigError, key_id_hex};
use super::{Version, archive, archive_failure, fetch_failure, sha256_hex, temp_name};
use crate::setup::Failure;
use crate::text::{Code, Text};
use plumblog_i18n::{Key, key};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

const SUMS: &str = "SHA256SUMS";
const SMALL: u64 = 1024 * 1024;
const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
const MAX_BINARY_BYTES: usize = 512 * 1024 * 1024;
/// The running binary's old copy on Windows, removed on the next run.
pub const OLD_EXE: &str = "blog.old.exe";

/// What `blog update` needs; `main` fills it from the binary, tests from a temp dir.
#[derive(Clone, Debug)]
pub struct Update {
    pub current: Version,
    /// Target triple: names the release archive.
    pub target: String,
    /// The minisign public key file text (the embedded one outside tests).
    pub key: String,
    /// The executable to replace.
    pub exe: PathBuf,
    pub source: Source,
    pub to: Option<Version>,
    /// Only report whether a newer release exists.
    pub check: bool,
}

fn line(out: &mut dyn Write, text: &Text, k: Key, args: &[(&str, String)]) {
    let _ = writeln!(out, "{}", text.t(k, args));
}

fn fail(code: Code, text: &Text, k: Key, args: &[(&str, String)]) -> Failure {
    Failure::new(code, text, k, args)
}

fn latest(src: &Source, text: &Text) -> Result<Version, Failure> {
    let body = src
        .get(src.latest_url(), "application/vnd.github+json", SMALL)
        .map_err(|e| fetch_failure(&e, text))?;
    let invalid = |detail: &str| {
        fail(
            Code::ReleaseReplyInvalid,
            text,
            key!("cli.error.release_reply_invalid"),
            &[("detail", detail.into())],
        )
    };
    let v: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| invalid(&e.to_string()))?;
    let tag = v
        .get("tag_name")
        .and_then(|t| t.as_str())
        .ok_or_else(|| invalid("no tag_name"))?;
    tag.starts_with('v')
        .then(|| Version::parse(tag))
        .flatten()
        .ok_or_else(|| invalid("tag_name is not vX.Y.Z"))
}

/// The release archive for `target`, and the binary's name inside it.
#[must_use]
pub fn archive_name(v: Version, target: &str) -> (String, &'static str) {
    if target.contains("windows") {
        (format!("blog-{}-{target}.zip", v.tag()), "blog.exe")
    } else {
        (format!("blog-{}-{target}.tar.gz", v.tag()), "blog")
    }
}

/// The SHA-256 `SHA256SUMS` gives for `name` (sha256sum format, text or binary mode).
fn sum_for<'s>(sums: &'s str, name: &str) -> Option<&'s str> {
    sums.lines().find_map(|l| {
        let (hash, file) = l.split_once(' ')?;
        let file = file.strip_prefix(' ').or_else(|| file.strip_prefix('*'))?;
        (file == name && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
            .then_some(hash)
    })
}

fn verify_sums(key: &PublicKey, tag: &str, src: &Source, text: &Text) -> Result<String, Failure> {
    let get = |file: &str| src.get(&src.file_url(tag, file), "application/octet-stream", SMALL);
    let sums = get(SUMS).map_err(|e| fetch_failure(&e, text))?;
    let sig = match get(&format!("{SUMS}.minisig")) {
        Ok(s) => s,
        Err(FetchError::NotFound(_)) => {
            return Err(fail(
                Code::UpdateSignatureMissing,
                text,
                key!("cli.error.update_signature_missing"),
                &[("tag", tag.into()), ("page", RELEASES_PAGE.into())],
            ));
        }
        Err(e) => return Err(fetch_failure(&e, text)),
    };
    let invalid = |reason: &str| {
        fail(
            Code::UpdateSignatureInvalid,
            text,
            key!("cli.error.update_signature_invalid"),
            &[
                ("tag", tag.into()),
                ("reason", reason.into()),
                ("page", RELEASES_PAGE.into()),
            ],
        )
    };
    let sig = String::from_utf8(sig).map_err(|_| invalid("not text"))?;
    key.verify(&sig, &sums).map_err(|e| match e {
        SigError::KeyMismatch { want, got } => fail(
            Code::UpdateKeyMismatch,
            text,
            key!("cli.error.update_key_mismatch"),
            &[
                ("tag", tag.into()),
                ("got", got),
                ("want", want),
                ("page", RELEASES_PAGE.into()),
            ],
        ),
        SigError::Prehashed => invalid("prehashed signature; releases use the legacy algorithm"),
        SigError::Malformed(why) => invalid(why),
        SigError::Invalid => invalid("bad signature"),
    })?;
    String::from_utf8(sums).map_err(|_| invalid("SHA256SUMS is not text"))
}

/// Writes `bytes` next to `exe`, then renames it over `exe` (Windows: moves the running exe
/// to [`OLD_EXE`] first, as a running exe cannot be overwritten but can be renamed).
fn replace(exe: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = exe.parent().unwrap_or(Path::new("."));
    let tmp = temp_name(dir, "blog-update");
    let moved = write_new(&tmp, bytes).and_then(|()| swap(&tmp, exe));
    if moved.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    moved
}

fn write_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    f.write_all(bytes)?;
    #[cfg(unix)]
    {
        // Set after creating, so the umask cannot leave it unexecutable.
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(fs::Permissions::from_mode(0o755))?;
    }
    f.sync_all()
}

#[cfg(not(windows))]
fn swap(new: &Path, exe: &Path) -> std::io::Result<()> {
    fs::rename(new, exe)
}

#[cfg(windows)]
fn swap(new: &Path, exe: &Path) -> std::io::Result<()> {
    let old = exe.parent().unwrap_or(Path::new(".")).join(OLD_EXE);
    let _ = fs::remove_file(&old);
    fs::rename(exe, &old)?;
    let moved = fs::rename(new, exe);
    if moved.is_err() {
        // Put the running blog back where it was.
        let _ = fs::rename(&old, exe);
    }
    moved
}

/// Removes the Windows leftover of the last update; nothing elsewhere.
pub fn clean_old(exe: &Path) {
    if cfg!(windows)
        && let Some(dir) = exe.parent()
    {
        let _ = fs::remove_file(dir.join(OLD_EXE));
    }
}

/// `blog update`.
///
/// # Errors
/// [`Failure`] with an `update.*` or `release.*` code; the executable is unchanged.
pub fn run(u: &Update, text: &Text, out: &mut dyn Write) -> Result<(), Failure> {
    if u.check {
        let newest = latest(&u.source, text)?;
        let args = [
            ("current", u.current.to_string()),
            ("latest", newest.to_string()),
        ];
        if newest > u.current {
            line(out, text, key!("cli.update.newer"), &args);
        } else {
            line(out, text, key!("cli.update.up_to_date"), &args);
        }
        return Ok(());
    }
    // The key first: without it nothing may be installed, so nothing is downloaded either.
    let key = PublicKey::parse(&u.key).map_err(|e| match e {
        KeyError::Placeholder => fail(
            Code::UpdateNoKey,
            text,
            key!("cli.error.update_no_key"),
            &[("page", RELEASES_PAGE.into())],
        ),
        KeyError::Malformed(why) => fail(
            Code::Internal,
            text,
            key!("cli.error.internal"),
            &[("detail", format!("embedded release key: {why}"))],
        ),
    })?;
    let target = if let Some(v) = u.to {
        v
    } else {
        let newest = latest(&u.source, text)?;
        if newest < u.current {
            return Err(fail(
                Code::UpdateDowngrade,
                text,
                key!("cli.error.update_downgrade"),
                &[
                    ("latest", newest.to_string()),
                    ("current", u.current.to_string()),
                    ("tag", newest.tag()),
                ],
            ));
        }
        newest
    };
    if target == u.current {
        let args = [
            ("current", u.current.to_string()),
            ("latest", target.to_string()),
        ];
        line(out, text, key!("cli.update.up_to_date"), &args);
        return Ok(());
    }
    let tag = target.tag();
    let sums = verify_sums(&key, &tag, &u.source, text)?;
    line(
        out,
        text,
        key!("cli.update.signature_ok"),
        &[("file", SUMS.into()), ("key_id", key_id_hex(&key.key_id))],
    );
    let binary = download_binary(u, target, &sums, text, out)?;
    replace(&u.exe, &binary).map_err(|e| {
        fail(
            Code::UpdateReplaceFailed,
            text,
            key!("cli.error.update_replace_failed"),
            &[
                ("path", u.exe.display().to_string()),
                ("reason", e.to_string()),
            ],
        )
    })?;
    line(
        out,
        text,
        key!("cli.update.done"),
        &[
            ("from", u.current.to_string()),
            ("to", target.to_string()),
            ("path", u.exe.display().to_string()),
        ],
    );
    Ok(())
}

/// This platform's archive, checked against the signed `sums`; the `blog` binary in it.
fn download_binary(
    u: &Update,
    target: Version,
    sums: &str,
    text: &Text,
    out: &mut dyn Write,
) -> Result<Vec<u8>, Failure> {
    let tag = target.tag();
    let (name, inner) = archive_name(target, &u.target);
    let want = sum_for(sums, &name).ok_or_else(|| {
        fail(
            Code::UpdateNoBuild,
            text,
            key!("cli.error.update_no_build"),
            &[
                ("tag", tag.clone()),
                ("target", u.target.clone()),
                ("page", RELEASES_PAGE.into()),
            ],
        )
    })?;
    let url = u.source.file_url(&tag, &name);
    line(
        out,
        text,
        key!("cli.update.downloading"),
        &[("url", url.clone())],
    );
    let data = u
        .source
        .get(&url, "application/octet-stream", MAX_ARCHIVE_BYTES)
        .map_err(|e| fetch_failure(&e, text))?;
    let got = sha256_hex(&data);
    if !got.eq_ignore_ascii_case(want) {
        return Err(fail(
            Code::UpdateHashMismatch,
            text,
            key!("cli.error.update_hash_mismatch"),
            &[
                ("file", name),
                ("got", got),
                ("want", want.to_ascii_lowercase()),
                ("page", RELEASES_PAGE.into()),
            ],
        ));
    }
    let zip = Path::new(&name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("zip"));
    if zip {
        archive::unzip_one(&data, inner, MAX_BINARY_BYTES)
    } else {
        archive::gunzip(&data, MAX_BINARY_BYTES)
            .and_then(|t| archive::untar(&t, &|_, _| true))
            .map(|mut files| files.remove(inner))
    }
    .map_err(|e| archive_failure(&e, text))?
    .filter(|b| !b.is_empty())
    .ok_or_else(|| {
        archive_failure(
            &archive::Bad {
                path: format!("{name}/{inner}"),
                reason: "missing".into(),
            },
            text,
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_lines_match_exact_names() {
        let h = "a".repeat(64);
        let sums = format!(
            "{h}  blog-v1.0.0-x.tar.gz\n{} *b.zip\nbad line\n",
            "b".repeat(64)
        );
        assert_eq!(sum_for(&sums, "blog-v1.0.0-x.tar.gz"), Some(h.as_str()));
        assert!(sum_for(&sums, "b.zip").is_some());
        assert_eq!(sum_for(&sums, "blog-v1.0.0-x.tar"), None);
        assert_eq!(sum_for("abc  f\n", "f"), None);
        assert_eq!(
            archive_name(Version(1, 2, 3), "x86_64-pc-windows-msvc").0,
            "blog-v1.2.3-x86_64-pc-windows-msvc.zip"
        );
    }
}
