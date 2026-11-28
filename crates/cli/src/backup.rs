//! `blog backup`: the database (plumblog's dump, as `blog deploy --export` makes) and the media
//! (every R2 original and variant the `media` table lists) copied into `backups/`, with no
//! deploy. Restore is a separate command.
//!
//! Media land at `<dir>/media/<r2_key>` through a temp file and a rename, each checked
//! against the size D1 recorded and, when R2's `ETag` is a plain MD5, against that too.
//! `<dir>/media/manifest.json` records what was verified (key, bytes, SHA-256), written
//! after every object, so an interrupted run resumes and a finished one downloads nothing.

use crate::cloudflare::{ApiError, ApiErrorKind, Call, Client, Endpoint};
use crate::d1::{self, Database};
use crate::deploy::{BACKUP_DIR, Lock, Purpose, Site, start, utc};
use crate::migrations;
use crate::setup::{Context, Failure, Run};
use crate::state::{BackupMedia, BackupRecord};
use crate::text::Code;
use plumblog_auth::{base64url::hex, sha256::sha256};
use plumblog_i18n::key;
use plumblog_model::api::MAX_MEDIA_BODY;
use plumblog_store::LAST_BACKUP_KEY;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Media rows read per D1 query.
const PAGE: u32 = 100;
/// The largest object read: the upload cap, and a margin for a variant that is a little
/// larger than its original. The body is held in memory (the client does not stream).
const MAX_OBJECT: usize = MAX_MEDIA_BODY + 1024 * 1024;
const MANIFEST: &str = "manifest.json";
/// Temp files end with this; no media key may.
const PART: &str = ".blog-part";

/// What the owner asked for.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Skip the media.
    pub no_media: bool,
    /// Backup folder, relative to the site folder or absolute (default `backups`).
    pub out: Option<PathBuf>,
}

/// A media key becomes a path inside the backup folder, so it must be a plain relative path:
/// no empty, `.` or `..` segment (which also rules out an absolute path), no backslash, colon
/// or control character (Windows paths), and nothing that could collide with the manifest or
/// a temp file.
#[must_use]
pub fn safe_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 1024
        && key != MANIFEST
        && !key.ends_with(PART)
        && key
            .chars()
            .all(|c| !c.is_control() && c != '\\' && c != ':')
        && key
            .split('/')
            .all(|s| !s.is_empty() && s != "." && s != "..")
}

#[derive(Default, Serialize, Deserialize)]
struct Manifest {
    #[serde(default)]
    objects: BTreeMap<String, Entry>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Entry {
    bytes: u64,
    sha256: String,
}

/// Writes `bytes` to `path` through a temp file in the same folder and a rename, mode 0600
/// (folders 0700): a reader never sees half a file.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        let mut b = fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        b.create(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(PART);
    let tmp = PathBuf::from(tmp);
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let written = opts.open(&tmp).and_then(|mut f| {
        f.write_all(bytes)?;
        f.sync_all()
    });
    if let Err(e) = written.and_then(|()| fs::rename(&tmp, path)) {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    Ok(())
}

pub(crate) fn md5_hex(bytes: &[u8]) -> String {
    use md5::{Digest, Md5};
    hex(&Md5::digest(bytes))
}

/// A single-part upload's `ETag` is the MD5 of the bytes: exactly 32 hex digits. A multipart
/// `ETag` carries `-<parts>`, and anything else is not a checksum of the bytes.
fn plain_md5(etag: &str) -> Option<&str> {
    (etag.len() == 32 && etag.bytes().all(|b| b.is_ascii_hexdigit())).then_some(etag)
}

/// Why a media row's keys cannot be read; each is a sentence in `locales/` (`cli.backup.reason_*`).
enum RowReason {
    NoKey,
    NoBytes,
    /// The JSON parser's message: raw detail, not prose.
    VariantsUnreadable(String),
    VariantsNotList,
    VariantNoKey,
    VariantNoBytes,
}

impl RowReason {
    fn render(&self, text: &crate::text::Text) -> String {
        match self {
            Self::NoKey => text.t(key!("cli.backup.reason_no_r2_key"), &[]),
            Self::NoBytes => text.t(key!("cli.backup.reason_no_bytes"), &[]),
            Self::VariantsUnreadable(e) => text.t(
                key!("cli.backup.reason_variants_unreadable"),
                &[("detail", e.clone())],
            ),
            Self::VariantsNotList => text.t(key!("cli.backup.reason_variants_not_list"), &[]),
            Self::VariantNoKey => text.t(key!("cli.backup.reason_variant_no_key"), &[]),
            Self::VariantNoBytes => text.t(key!("cli.backup.reason_variant_no_bytes"), &[]),
        }
    }
}

/// The keys one media row names: the original, then its variants.
fn row_keys(row: &serde_json::Map<String, Value>) -> Result<Vec<(String, u64)>, RowReason> {
    let key = row
        .get("r2_key")
        .and_then(Value::as_str)
        .ok_or(RowReason::NoKey)?;
    let bytes = row
        .get("bytes")
        .and_then(Value::as_u64)
        .ok_or(RowReason::NoBytes)?;
    let mut keys = vec![(key.to_string(), bytes)];
    let variants: Value = serde_json::from_str(
        row.get("variants_json")
            .and_then(Value::as_str)
            .unwrap_or("[]"),
    )
    .map_err(|e| RowReason::VariantsUnreadable(e.to_string()))?;
    for v in variants.as_array().ok_or(RowReason::VariantsNotList)? {
        let key = v
            .get("key")
            .and_then(Value::as_str)
            .ok_or(RowReason::VariantNoKey)?;
        let bytes = v
            .get("bytes")
            .and_then(Value::as_u64)
            .ok_or(RowReason::VariantNoBytes)?;
        keys.push((key.to_string(), bytes));
    }
    Ok(keys)
}

/// Counts of one media pass.
#[derive(Default)]
struct Tally {
    copied: u64,
    kept: u64,
    bytes: u64,
    missing: u64,
    failed: u64,
}

impl Tally {
    fn record(&self) -> BackupMedia {
        BackupMedia {
            objects: self.copied + self.kept,
            bytes: self.bytes,
            missing: self.missing,
            failed: self.failed,
        }
    }
}

/// Prints one per-object problem as an error line; the run goes on.
fn problem(run: &mut Run, code: Code, key: plumblog_i18n::Key, args: &[(&str, String)]) {
    let line = run.cx.text.error_line(code, &run.t(key, args));
    let _ = writeln!(run.out, "{line}");
}

struct Media<'a, 'b> {
    run: &'a mut Run<'b>,
    site: &'a Site,
    dir: PathBuf,
    manifest: Manifest,
    tally: Tally,
    seen: HashSet<String>,
}

impl Media<'_, '_> {
    fn save_manifest(&self) -> Result<(), Failure> {
        let path = self.dir.join(MANIFEST);
        let text = serde_json::to_string_pretty(&self.manifest).unwrap_or_default();
        write_atomic(&path, text.as_bytes()).map_err(|e| self.unwritable(&path, &e))
    }

    fn unwritable(&self, path: &Path, e: &std::io::Error) -> Failure {
        Failure::new(
            Code::BackupMediaUnwritable,
            self.run.cx.text,
            key!("cli.error.backup_media_unwritable"),
            &[
                ("path", path.display().to_string()),
                ("reason", e.to_string()),
            ],
        )
    }

    /// Whether `key`'s file is already here, with the size D1 gives and the recorded hash.
    fn verified(&self, key: &str, want: u64, path: &Path) -> bool {
        let Some(e) = self.manifest.objects.get(key) else {
            return false;
        };
        e.bytes == want
            && fs::read(path).is_ok_and(|b| b.len() as u64 == want && hex(&sha256(&b)) == e.sha256)
    }

    /// One object. `Err` only for what stops the run (a bad token, a full disk).
    fn object(&mut self, key: &str, want: u64) -> Result<(), Failure> {
        if !self.seen.insert(key.to_string()) {
            return Ok(());
        }
        let shown = key.chars().filter(|c| !c.is_control()).collect::<String>();
        if !safe_key(key) {
            self.tally.failed += 1;
            problem(
                self.run,
                Code::BackupKeyUnsafe,
                key!("cli.error.backup_key_unsafe"),
                &[("key", shown)],
            );
            return Ok(());
        }
        let path = self.dir.join(key);
        if self.verified(key, want, &path) {
            self.tally.kept += 1;
            self.tally.bytes += want;
            return Ok(());
        }
        if usize::try_from(want).is_ok_and(|w| w > MAX_OBJECT) {
            self.tally.failed += 1;
            problem(
                self.run,
                Code::BackupObjectTooBig,
                key!("cli.error.backup_object_too_big"),
                &[("key", shown), ("limit", MAX_OBJECT.to_string())],
            );
            return Ok(());
        }
        let call = Call::new(Endpoint::R2ObjectGet)
            .param("bucket_name", self.site.bucket.clone())
            .param("object_key", key);
        let object = match self.run.client.get_object(&call, MAX_OBJECT) {
            Ok(Some(o)) => o,
            Ok(None) => {
                self.tally.missing += 1;
                problem(
                    self.run,
                    Code::BackupObjectMissing,
                    key!("cli.error.backup_object_missing"),
                    &[("key", shown)],
                );
                return Ok(());
            }
            Err(e) => return self.read_failed(&e, shown),
        };
        let got = object.bytes.len() as u64;
        if got != want {
            self.tally.failed += 1;
            problem(
                self.run,
                Code::BackupSizeMismatch,
                key!("cli.error.backup_size_mismatch"),
                &[
                    ("key", shown),
                    ("got", got.to_string()),
                    ("want", want.to_string()),
                ],
            );
            return Ok(());
        }
        if let Some(etag) = object.etag.as_deref().and_then(plain_md5)
            && !etag.eq_ignore_ascii_case(&md5_hex(&object.bytes))
        {
            self.tally.failed += 1;
            problem(
                self.run,
                Code::BackupChecksumMismatch,
                key!("cli.error.backup_checksum_mismatch"),
                &[("key", shown), ("etag", etag.to_string())],
            );
            return Ok(());
        }
        write_atomic(&path, &object.bytes).map_err(|e| self.unwritable(&path, &e))?;
        self.manifest.objects.insert(
            key.to_string(),
            Entry {
                bytes: got,
                sha256: hex(&sha256(&object.bytes)),
            },
        );
        self.save_manifest()?;
        self.tally.copied += 1;
        self.tally.bytes += got;
        Ok(())
    }

    /// A failed read stops the run when the token is the problem (every later object would
    /// fail the same way); otherwise it counts against this object and the run goes on.
    fn read_failed(&mut self, e: &ApiError, key: String) -> Result<(), Failure> {
        if matches!(
            e.kind,
            ApiErrorKind::InvalidToken
                | ApiErrorKind::MissingPermission(_)
                | ApiErrorKind::BaseRefused
        ) {
            return Err(self.run.api(e));
        }
        self.tally.failed += 1;
        let reason = self.run.api(e).message;
        problem(
            self.run,
            Code::BackupObjectFailed,
            key!("cli.error.backup_object_failed"),
            &[("key", key), ("reason", reason)],
        );
        Ok(())
    }

    /// Every media row, a page at a time (`id` order, so a row added meanwhile is simply
    /// the next page's).
    fn all(&mut self, db: &Database) -> Result<(), Failure> {
        let mut after: u64 = 0;
        loop {
            let sql = format!(
                "SELECT id, r2_key, bytes, variants_json FROM media WHERE id > {after} \
                 ORDER BY id LIMIT {PAGE}"
            );
            let rows = db.rows(&sql, &[]).map_err(|e| self.run.api(&e))?;
            let Some(last) = rows
                .last()
                .and_then(|r| r.get("id"))
                .and_then(Value::as_u64)
            else {
                return Ok(());
            };
            for row in &rows {
                match row_keys(row) {
                    Ok(keys) => {
                        for (key, bytes) in keys {
                            self.object(&key, bytes)?;
                        }
                    }
                    Err(reason) => {
                        self.tally.failed += 1;
                        let id = row.get("id").map_or_else(String::new, ToString::to_string);
                        let reason = reason.render(self.run.cx.text);
                        problem(
                            self.run,
                            Code::BackupRowInvalid,
                            key!("cli.error.backup_row_invalid"),
                            &[("id", id), ("reason", reason)],
                        );
                    }
                }
            }
            after = last;
            if rows.len() < PAGE as usize {
                return Ok(());
            }
        }
    }
}

/// Copies every media file into `<dir>/media/`. The tally is returned with the error that
/// stopped the run, if any, so the record still says how far it got.
fn media(run: &mut Run, site: &Site, db: &Database, dir: &str) -> (Tally, Option<Failure>) {
    let root = run.cx.dir.join(dir).join("media");
    run.line(key!("cli.backup.media_start"), &[("dir", dir.to_string())]);
    let manifest = match fs::read_to_string(root.join(MANIFEST)) {
        Ok(t) => serde_json::from_str(&t).unwrap_or_else(|e| {
            run.line(
                key!("cli.backup.manifest_reset"),
                &[
                    ("path", root.join(MANIFEST).display().to_string()),
                    ("reason", e.to_string()),
                ],
            );
            Manifest::default()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Manifest::default(),
        Err(e) => {
            run.line(
                key!("cli.backup.manifest_reset"),
                &[
                    ("path", root.join(MANIFEST).display().to_string()),
                    ("reason", e.to_string()),
                ],
            );
            Manifest::default()
        }
    };
    let mut m = Media {
        run,
        site,
        dir: root,
        manifest,
        tally: Tally::default(),
        seen: HashSet::new(),
    };
    let stopped = m.all(db).err();
    (m.tally, stopped)
}

/// `{at, database, media, complete}`, the setting's value.
fn record_json(r: &BackupRecord) -> String {
    json!({
        "at": r.at,
        "database": r.database,
        "media": r.media.map(|m| json!({
            "objects": m.objects, "bytes": m.bytes, "missing": m.missing, "failed": m.failed,
        })),
        "complete": r.complete,
    })
    .to_string()
}

/// Keeps the record in `site.local.toml` and in the site's `last_backup` setting.
fn record(run: &mut Run, db: &Database, rec: BackupRecord) -> Result<(), Failure> {
    run.update(|s| s.last_backup = Some(rec))?;
    d1::put_setting(db, LAST_BACKUP_KEY, &record_json(&rec), run.cx.now_ms)
        .map_err(|e| run.api(&e))?;
    run.line(key!("cli.backup.recorded"), &[]);
    Ok(())
}

/// `blog backup`. Progress goes to `out` as each step finishes.
///
/// # Errors
/// [`Failure`] with a stable code. `backup.incomplete` means the run finished but some media
/// file failed: the lines above name them, and a re-run retries only those.
pub fn run(
    client: &mut Client,
    account_id: Option<&str>,
    opts: &Options,
    cx: &Context,
    out: &mut dyn Write,
) -> Result<(), Failure> {
    let _lock = Lock::take(cx)?;
    let (st, site) = start(client, account_id, cx)?;
    let client: &Client = client;
    let mut run = Run {
        client,
        cx,
        out,
        state: st,
        secrets: Vec::new(),
    };
    let dir = opts.out.as_ref().map_or_else(
        || BACKUP_DIR.to_string(),
        |p| {
            let s = p.to_string_lossy();
            let t = s.trim_end_matches('/');
            if t.is_empty() {
                s.into_owned()
            } else {
                t.to_string()
            }
        },
    );
    let (_, stamp) = utc(cx.now_ms);
    run.line(key!("cli.backup.warning"), &[]);
    let db = Database::new(client, site.db_id.clone());
    let mark = migrations::bookmark(&db, client).map_err(|e| run.api(&e))?;
    run.line(key!("cli.backup.bookmark"), &[("bookmark", mark.clone())]);
    crate::dump::export(&mut run, &site, &db, &mark, &stamp, &dir, Purpose::Backup)?;
    let (tally, stopped) = if opts.no_media {
        run.line(key!("cli.backup.media_skipped"), &[]);
        (None, None)
    } else {
        let (t, stopped) = media(&mut run, &site, &db, &dir);
        (Some(t), stopped)
    };
    let media = tally.as_ref().map(Tally::record);
    let complete = stopped.is_none() && media.is_some_and(|m| m.failed == 0 && m.missing == 0);
    let rec = BackupRecord {
        at: cx.now_ms.div_euclid(1000),
        database: true,
        media,
        complete,
    };
    let saved = record(&mut run, &db, rec);
    if let Some(f) = stopped {
        // The run stopped early: its own error is the one to show.
        if let Err(e) = saved {
            let _ = writeln!(run.out, "{}", cx.text.error_line(e.code, &e.message));
        }
        return Err(f);
    }
    let Some(t) = tally else {
        run.line(key!("cli.backup.database_only"), &[]);
        return saved;
    };
    run.line(
        key!("cli.backup.media_done"),
        &[
            ("objects", (t.copied + t.kept).to_string()),
            ("bytes", t.bytes.to_string()),
            ("copied", t.copied.to_string()),
            ("kept", t.kept.to_string()),
        ],
    );
    saved?;
    if t.failed > 0 {
        return Err(Failure::new(
            Code::BackupIncomplete,
            cx.text,
            key!("cli.error.backup_incomplete"),
            &[
                ("failed", t.failed.to_string()),
                ("missing", t.missing.to_string()),
                ("objects", (t.copied + t.kept).to_string()),
            ],
        ));
    }
    if t.missing > 0 {
        run.line(
            key!("cli.backup.with_missing"),
            &[("missing", t.missing.to_string())],
        );
    } else {
        run.line(key!("cli.backup.complete"), &[]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_must_be_plain_relative_paths() {
        for ok in ["media/abc_-/photo.jpg", "media/a/b c.png", "a/b"] {
            assert!(safe_key(ok), "{ok}");
        }
        for bad in [
            "",
            "/etc/passwd",
            "media/../x",
            "../x",
            "media//x",
            "media/./x",
            "media\\x",
            "C:/x",
            "media/x\n",
            "manifest.json",
            "media/x.blog-part",
            "media/",
        ] {
            assert!(!safe_key(bad), "{bad:?}");
        }
    }

    #[test]
    fn only_a_plain_md5_etag_is_a_checksum() {
        assert!(plain_md5("d41d8cd98f00b204e9800998ecf8427e").is_some());
        assert!(plain_md5("d41d8cd98f00b204e9800998ecf8427e-3").is_none());
        assert!(plain_md5("abc").is_none());
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
    }
}
