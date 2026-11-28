//! `blog backup` against the fake API: the database export, the media copy with its
//! manifest, the checks that refuse a bad object, resuming, and the `last_backup` record.

use super::{GOOD, TempDir, client};
use crate::fake_cf::{Failure, Fake, Object};
use crate::setup::{ALL, ok as setup_ok, run_with as setup_with, site};
use plumblog_cli::backup::{self, Options};
use plumblog_cli::migrations;
use plumblog_cli::setup::{Context, Failure as RunFailure, Options as SetupOptions};
use plumblog_cli::state::{self, SiteState};
use plumblog_cli::text::{Code, Text};
use serde_json::{Value, json};
use std::path::PathBuf;

const NOW: i64 = 1_790_000_000_000;
const BUCKET: &str = "plumblog-media";

struct Ran {
    result: Result<(), RunFailure>,
    out: String,
}

fn en() -> Text {
    Text::new("en")
}

fn set_up() -> (Fake, TempDir) {
    let fake = Fake::start(site(ALL));
    let dir = TempDir::new();
    let r = setup_with(
        &fake,
        &dir,
        &SetupOptions::default(),
        &migrations::bundled(),
        &en(),
    );
    setup_ok(&r);
    (fake, dir)
}

fn backup_with(fake: &Fake, dir: &TempDir, opts: &Options) -> Ran {
    backup_later(fake, dir, opts, 0)
}

/// `secs` after [`NOW`]: the export file is named by the second, so a re-run needs a new one.
fn backup_later(fake: &Fake, dir: &TempDir, opts: &Options, secs: i64) -> Ran {
    let mut c = client(fake, GOOD);
    let migs = migrations::bundled();
    let text = en();
    let cx = Context {
        dir: &dir.0,
        migrations: &migs,
        now_ms: NOW + secs * 1000,
        text: &text,
    };
    let mut buf = Vec::new();
    let result = backup::run(&mut c, None, opts, &cx, &mut buf);
    Ran {
        result,
        out: String::from_utf8(buf).unwrap(),
    }
}

fn backup(fake: &Fake, dir: &TempDir) -> Ran {
    backup_with(fake, dir, &Options::default())
}

fn again(fake: &Fake, dir: &TempDir, secs: i64) -> Ran {
    backup_later(fake, dir, &Options::default(), secs)
}

fn ok(r: &Ran) {
    if let Err(e) = &r.result {
        panic!("{:?}: {}\n{}", e.code, e.message, r.out);
    }
}

fn code(r: &Ran) -> Code {
    r.result.as_ref().unwrap_err().code
}

fn load(dir: &TempDir) -> SiteState {
    state::load(&dir.0).unwrap().unwrap()
}

fn put_object(fake: &Fake, key: &str, body: &str, etag: Option<&str>) {
    fake.state().objects.insert(
        format!("{BUCKET}/{key}"),
        Object {
            body: body.to_string(),
            etag: etag.map(str::to_string),
        },
    );
}

/// A media row (`bytes` as D1 records it) with variants `(key, bytes)`; no objects yet.
fn row(fake: &Fake, id: i64, key: &str, bytes: usize, variants: &[(&str, usize)]) {
    let vs: Vec<Value> = variants
        .iter()
        .map(
            |(k, b)| json!({"key": k, "width": 10, "height": 10, "bytes": b, "mime": "image/webp"}),
        )
        .collect();
    let variants = Value::Array(vs).to_string().replace('\'', "''");
    fake.exec(
        "db-1",
        &format!(
            "INSERT INTO media (id, r2_key, mime, bytes, width, height, variants_json, created_at) \
             VALUES ({id}, '{}', 'image/png', {bytes}, 10, 10, '{variants}', 1)",
            key.replace('\'', "''")
        ),
    );
}

/// A row whose original and variants are in R2 with exactly the recorded sizes.
fn media(fake: &Fake, id: i64, key: &str, body: &str, variants: &[(&str, &str)]) {
    let vs: Vec<(&str, usize)> = variants.iter().map(|(k, b)| (*k, b.len())).collect();
    row(fake, id, key, body.len(), &vs);
    put_object(fake, key, body, None);
    for (k, b) in variants {
        put_object(fake, k, b, None);
    }
}

fn object_gets(fake: &Fake) -> Vec<String> {
    fake.requests()
        .iter()
        .filter(|r| r.method == "GET" && r.path.contains("/objects/"))
        .map(|r| r.path.clone())
        .collect()
}

fn last_backup_setting(fake: &Fake) -> Value {
    let rows = fake.sql(
        "db-1",
        "SELECT value_json FROM settings WHERE key = 'last_backup'",
    );
    serde_json::from_str(rows[0]["value_json"].as_str().unwrap()).unwrap()
}

fn media_file(dir: &TempDir, key: &str) -> PathBuf {
    dir.0.join("backups/media").join(key)
}

fn manifest(dir: &TempDir) -> Value {
    serde_json::from_slice(&std::fs::read(dir.0.join("backups/media/manifest.json")).unwrap())
        .unwrap()
}

#[test]
fn the_database_alone_is_exported_without_a_deploy() {
    let (fake, dir) = set_up();
    let skip = fake.requests().len();
    let r = backup_with(
        &fake,
        &dir,
        &Options {
            no_media: true,
            out: None,
        },
    );
    ok(&r);
    assert!(r.out.contains("The site stays up"), "{}", r.out);
    let file = dir.0.join("backups/20260921T141320Z-plumblog.sql");
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("\nCREATE TABLE posts ("), "{text}");
    assert!(text.ends_with("-- end of plumblog dump\n"));
    // The header names the bookmark the run took first.
    let bm = fake.state().bookmarks.last().cloned().unwrap();
    assert!(text.contains(&format!("bookmark taken just before: {bm}\n")));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    // No deploy, no media read, no media listing.
    for req in &fake.requests()[skip..] {
        let line = format!("{} {}", req.method, req.path);
        assert!(
            !line.contains("/versions")
                && !line.contains("/deployments")
                && !line.contains("/objects/")
                && !line.contains("/assets"),
            "{line}"
        );
        // The dump reads the media table; the media copy's listing must not run.
        assert!(!req.body.contains("SELECT id, r2_key"), "{}", req.body);
    }
    // The record: in the state file and in the site's own setting.
    let rec = load(&dir).last_backup.unwrap();
    assert!(rec.database && rec.media.is_none() && !rec.complete);
    assert_eq!(rec.at, 1_790_000_000);
    assert_eq!(
        last_backup_setting(&fake),
        json!({"at": 1_790_000_000, "database": true, "media": null, "complete": false})
    );
    assert!(!dir.0.join(".blog-deploy.lock").exists());
}

#[test]
fn media_and_variants_are_copied_with_a_manifest() {
    let (fake, dir) = set_up();
    media(
        &fake,
        1,
        "media/aaa/one.png",
        "ONE",
        &[("media/aaa/one-w1.webp", "ONEWEBP")],
    );
    media(&fake, 2, "media/bbb/a b.png", "TWO!", &[]);
    let r = backup(&fake, &dir);
    ok(&r);
    assert_eq!(
        std::fs::read_to_string(media_file(&dir, "media/aaa/one.png")).unwrap(),
        "ONE"
    );
    assert_eq!(
        std::fs::read_to_string(media_file(&dir, "media/aaa/one-w1.webp")).unwrap(),
        "ONEWEBP"
    );
    assert_eq!(
        std::fs::read_to_string(media_file(&dir, "media/bbb/a b.png")).unwrap(),
        "TWO!"
    );
    // The key is sent with its slashes and each segment encoded.
    let gets = object_gets(&fake);
    assert!(
        gets.iter()
            .any(|p| p.ends_with("/objects/media/bbb/a%20b.png")),
        "{gets:?}"
    );
    let req = fake
        .requests()
        .into_iter()
        .find(|r| r.path.contains("/objects/"))
        .unwrap();
    assert_eq!(
        req.header("authorization"),
        Some(format!("Bearer {GOOD}").as_str())
    );
    // Manifest: key -> bytes and the SHA-256 of "ONE".
    let m = manifest(&dir);
    assert_eq!(m["objects"]["media/aaa/one.png"]["bytes"], 3);
    assert_eq!(m["objects"]["media/aaa/one.png"]["sha256"], sha("ONE"));
    assert_eq!(m["objects"].as_object().unwrap().len(), 3);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(media_file(&dir, "media/aaa/one.png"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    // No temp file is left behind.
    let mut files = Vec::new();
    crate::files_under(&dir.0.join("backups"), &mut files);
    assert!(
        files
            .iter()
            .all(|f| !f.to_string_lossy().ends_with(".blog-part")),
        "{files:?}"
    );
    // The record counts what is in the backup.
    let want = json!({"at": 1_790_000_000, "database": true,
        "media": {"objects": 3, "bytes": 3 + 7 + 4, "missing": 0, "failed": 0}, "complete": true});
    assert_eq!(last_backup_setting(&fake), want);
    let rec = load(&dir).last_backup.unwrap();
    assert!(rec.complete && rec.media.unwrap().objects == 3);
    assert!(r.out.contains("Backup complete"), "{}", r.out);
}

fn sha(s: &str) -> String {
    plumblog_auth::base64url::hex(&plumblog_auth::sha256::sha256(s.as_bytes()))
}

#[test]
fn a_second_run_downloads_nothing_it_has_verified() {
    let (fake, dir) = set_up();
    media(
        &fake,
        1,
        "media/aaa/one.png",
        "ONE",
        &[("media/aaa/two.png", "TWO")],
    );
    ok(&backup(&fake, &dir));
    assert_eq!(object_gets(&fake).len(), 2);
    let rerun = again(&fake, &dir, 1);
    ok(&rerun);
    assert_eq!(object_gets(&fake).len(), 2, "re-run fetched again");
    assert!(
        rerun.out.contains("0 copied now, 2 already there"),
        "{}",
        rerun.out
    );
    // A file changed on disk no longer matches the recorded hash: only it is fetched again.
    std::fs::write(media_file(&dir, "media/aaa/two.png"), "XXX").unwrap();
    ok(&again(&fake, &dir, 2));
    assert_eq!(object_gets(&fake).len(), 3);
    assert_eq!(
        std::fs::read_to_string(media_file(&dir, "media/aaa/two.png")).unwrap(),
        "TWO"
    );
}

#[test]
fn a_wrong_size_is_refused_and_leaves_no_file() {
    let (fake, dir) = set_up();
    row(&fake, 1, "media/aaa/bad.png", 99, &[]);
    put_object(&fake, "media/aaa/bad.png", "short", None);
    media(&fake, 2, "media/bbb/good.png", "GOOD", &[]);
    let r = backup(&fake, &dir);
    assert_eq!(code(&r), Code::BackupIncomplete);
    assert!(r.out.contains("backup.size_mismatch"), "{}", r.out);
    assert!(!media_file(&dir, "media/aaa/bad.png").exists());
    assert!(
        !dir.0.join("backups/media/media/aaa").exists()
            || std::fs::read_dir(media_file(&dir, "media/aaa"))
                .unwrap()
                .next()
                .is_none()
    );
    // The others still went through, and the record says it is not complete.
    assert!(media_file(&dir, "media/bbb/good.png").exists());
    let rec = load(&dir).last_backup.unwrap();
    assert!(!rec.complete);
    assert_eq!(rec.media.unwrap().failed, 1);
    assert_eq!(last_backup_setting(&fake)["media"]["failed"], 1);
}

#[test]
fn a_wrong_md5_etag_is_refused_but_other_etags_are_not_checks() {
    let (fake, dir) = set_up();
    let md5 = |s: &str| {
        use md5::{Digest, Md5};
        plumblog_auth::base64url::hex(&Md5::digest(s.as_bytes()))
    };
    for (id, key, etag) in [
        (
            1,
            "media/a/bad.png",
            Some("d41d8cd98f00b204e9800998ecf8427e".to_string()),
        ),
        (2, "media/a/good.png", Some(md5("GOOD"))),
        (
            3,
            "media/a/multi.png",
            Some("d41d8cd98f00b204e9800998ecf8427e-4".to_string()),
        ),
        (4, "media/a/none.png", None),
    ] {
        let body = if id == 2 { "GOOD" } else { "BODY" };
        row(&fake, id, key, body.len(), &[]);
        put_object(&fake, key, body, etag.as_deref());
    }
    let r = backup(&fake, &dir);
    assert_eq!(code(&r), Code::BackupIncomplete);
    assert!(r.out.contains("backup.checksum_mismatch"), "{}", r.out);
    assert!(!media_file(&dir, "media/a/bad.png").exists());
    for ok_key in ["good", "multi", "none"] {
        assert!(
            media_file(&dir, &format!("media/a/{ok_key}.png")).exists(),
            "{ok_key}"
        );
    }
    assert_eq!(load(&dir).last_backup.unwrap().media.unwrap().failed, 1);
}

#[test]
fn an_unsafe_key_is_refused_before_any_request_for_it() {
    let (fake, dir) = set_up();
    row(&fake, 1, "media/../../evil.png", 3, &[]);
    row(&fake, 2, "/abs/evil.png", 3, &[("media\\x\\evil.png", 3)]);
    media(&fake, 3, "media/ok/ok.png", "OK!", &[]);
    let r = backup(&fake, &dir);
    assert_eq!(code(&r), Code::BackupIncomplete);
    assert_eq!(r.out.matches("backup.key_unsafe").count(), 3, "{}", r.out);
    assert_eq!(object_gets(&fake).len(), 1, "{:?}", object_gets(&fake));
    assert!(!dir.0.join("evil.png").exists() && !dir.0.join("backups/evil.png").exists());
    assert_eq!(load(&dir).last_backup.unwrap().media.unwrap().failed, 3);
}

#[test]
fn a_missing_object_fails_the_run_after_copying_the_rest() {
    let (fake, dir) = set_up();
    row(&fake, 1, "media/gone/gone.png", 5, &[]);
    media(&fake, 2, "media/ok/ok.png", "OK!", &[]);
    let r = backup(&fake, &dir);
    assert_eq!(code(&r), Code::BackupIncomplete, "{}", r.out);
    assert!(r.out.contains("backup.object_missing"), "{}", r.out);
    let msg = r.result.as_ref().unwrap_err().message.clone();
    assert!(
        msg.contains("1 media files") && msg.contains("media/gone/gone.png"),
        "{msg}"
    );
    assert!(media_file(&dir, "media/ok/ok.png").exists());
    let rec = load(&dir).last_backup.unwrap();
    assert!(!rec.complete);
    let m = rec.media.unwrap();
    assert_eq!((m.objects, m.missing, m.failed), (1, 1, 0));
}

#[test]
fn every_object_missing_fails_with_a_capped_list_and_a_hint() {
    let (fake, dir) = set_up();
    for id in 1..=30 {
        row(&fake, id, &format!("media/m{id}.png"), 5, &[]);
    }
    let r = backup(&fake, &dir);
    assert_eq!(code(&r), Code::BackupIncomplete, "{}", r.out);
    let msg = r.result.as_ref().unwrap_err().message.clone();
    assert!(
        msg.contains("30 media files") && msg.contains("and 10 more"),
        "{msg}"
    );
    assert!(msg.contains("R2 permission"), "{msg}");
    assert_eq!(
        r.out.matches("backup.object_missing").count(),
        20,
        "{}",
        r.out
    );
    let rec = load(&dir).last_backup.unwrap();
    assert!(!rec.complete);
    assert_eq!(rec.media.unwrap().missing, 30);
}

#[test]
fn an_interrupted_run_resumes_where_it_stopped() {
    let (fake, dir) = set_up();
    for (i, k) in ["a", "b", "c"].iter().enumerate() {
        media(
            &fake,
            i64::try_from(i).unwrap() + 1,
            &format!("media/{k}/{k}.png"),
            &k.to_uppercase(),
            &[],
        );
    }
    // The token loses its permission on the second object: nothing after it is tried.
    fake.fail_on("/objects/media/b/b.png", Failure::Status(403));
    let first = backup(&fake, &dir);
    assert_eq!(code(&first), Code::PermissionMissing);
    assert!(media_file(&dir, "media/a/a.png").exists());
    assert!(!media_file(&dir, "media/b/b.png").exists());
    assert_eq!(manifest(&dir)["objects"].as_object().unwrap().len(), 1);
    let rec = load(&dir).last_backup.unwrap();
    assert!(!rec.complete && rec.media.unwrap().objects == 1);
    let before = object_gets(&fake).len();
    let second = again(&fake, &dir, 1);
    ok(&second);
    let gets = object_gets(&fake);
    assert_eq!(gets.len() - before, 2, "{gets:?}");
    assert!(
        gets[before..].iter().all(|p| !p.ends_with("a.png")),
        "{gets:?}"
    );
    assert!(load(&dir).last_backup.unwrap().complete);
}

#[test]
fn many_rows_are_read_in_pages() {
    let (fake, dir) = set_up();
    for i in 1..=120 {
        media(&fake, i, &format!("media/p{i}/f.png"), "x", &[]);
    }
    ok(&backup(&fake, &dir));
    let selects = fake
        .requests()
        .iter()
        .filter(|r| r.body.contains("FROM media"))
        .count();
    assert_eq!(selects, 2);
    assert_eq!(load(&dir).last_backup.unwrap().media.unwrap().objects, 120);
}

#[test]
fn out_chooses_the_folder_and_a_held_lock_stops_the_run() {
    let (fake, dir) = set_up();
    media(&fake, 1, "media/a/a.png", "A", &[]);
    ok(&backup_with(
        &fake,
        &dir,
        &Options {
            no_media: false,
            out: Some(PathBuf::from("elsewhere")),
        },
    ));
    assert!(dir.0.join("elsewhere/media/media/a/a.png").exists());
    assert!(!dir.0.join("backups").exists());
    std::fs::write(dir.0.join(".blog-deploy.lock"), "1\n").unwrap();
    assert_eq!(code(&backup(&fake, &dir)), Code::DeployLocked);
}

#[test]
fn a_site_that_is_not_set_up_is_refused_before_any_call() {
    let fake = Fake::start(site(ALL));
    let dir = TempDir::new();
    let r = backup(&fake, &dir);
    assert_eq!(code(&r), Code::NotSetUp);
    assert!(fake.requests().is_empty());
}

#[test]
fn an_unreadable_media_row_is_told_in_the_owners_language() {
    let (fake, dir) = set_up();
    fake.exec(
        "db-1",
        "INSERT INTO media (id, r2_key, mime, bytes, width, height, variants_json, created_at) \
         VALUES (1, 'a.png', 'image/png', 1, 10, 10, '{}', 1)",
    );
    let mut c = client(&fake, GOOD);
    let migs = migrations::bundled();
    let vi = Text::new("vi");
    let cx = Context {
        dir: &dir.0,
        migrations: &migs,
        now_ms: NOW,
        text: &vi,
    };
    let mut buf = Vec::new();
    let r = backup::run(&mut c, None, &Options::default(), &cx, &mut buf);
    let out = String::from_utf8(buf).unwrap();
    assert!(r.is_err(), "{out}");
    let reason = vi.t(
        plumblog_i18n::key!("cli.backup.reason_variants_not_list"),
        &[],
    );
    assert!(out.contains(&reason), "{out}");
}
