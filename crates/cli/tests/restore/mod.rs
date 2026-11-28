//! `blog restore` against the fake API: every guard that refuses before anything changes, the
//! bookmark saved before the first change, the import replacing the database for real
//! (SQLite), pending migrations re-applied, and failed uploads and imports.

use super::{GOOD, TempDir, client};
use crate::deploy::{FakeClock, make_bundle, write_migrations};
use crate::fake_cf::{Failure, Fake, Recorded};
use crate::setup::{ALL, ok as setup_ok, run_with as setup_with, site};
use plumblog_cli::dump;
use plumblog_cli::migrations::{self, Migration};
use plumblog_cli::restore::{self, Options};
use plumblog_cli::setup::{Context, Failure as RunFailure, Options as SetupOptions};
use plumblog_cli::state::{self, SiteState};
use plumblog_cli::text::{Code, Text};
use rusqlite::Connection;
use rusqlite::types::ValueRef;
use serde_json::Value;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

const NOW: i64 = 1_790_000_000_000;
const DB: &str = "db-1";
const FILE: &str = "backups/20261101T080000Z-plumblog.sql";

struct Ran {
    result: Result<(), RunFailure>,
    out: String,
}

fn en() -> Text {
    Text::new("en")
}

/// A site set up with `migs`, and a release bundle with every migration.
fn set_up(migs: &[Migration]) -> (Fake, TempDir, PathBuf) {
    let fake = Fake::start(site(ALL));
    let dir = TempDir::new();
    setup_ok(&setup_with(
        &fake,
        &dir,
        &SetupOptions::default(),
        migs,
        &en(),
    ));
    let bundle = make_bundle(&dir);
    (fake, dir, bundle)
}

fn opts(bundle: &Path, yes: bool) -> Options {
    Options {
        file: PathBuf::from(FILE),
        yes,
        bundle: Some(bundle.to_path_buf()),
        ..Options::default()
    }
}

fn restore_with(fake: &Fake, dir: &TempDir, o: &Options) -> Ran {
    restore_in(fake, dir, o, &en())
}

fn restore_in(fake: &Fake, dir: &TempDir, o: &Options, text: &Text) -> Ran {
    let mut c = client(fake, GOOD);
    let migs = migrations::bundled();
    let cx = Context {
        dir: &dir.0,
        migrations: &migs,
        now_ms: NOW,
        text,
    };
    let mut buf = Vec::new();
    let result = restore::run(&mut c, None, o, &cx, &FakeClock::default(), &mut buf);
    Ran {
        result,
        out: String::from_utf8(buf).unwrap(),
    }
}

fn ok(r: &Ran) {
    if let Err(e) = &r.result {
        panic!("{:?}: {}\n{}", e.code, e.message, r.out);
    }
}

fn code(r: &Ran) -> Code {
    match &r.result {
        Ok(()) => panic!("expected a failure, got success:\n{}", r.out),
        Err(e) => e.code,
    }
}

fn message(r: &Ran) -> String {
    r.result.as_ref().unwrap_err().message.clone()
}

fn load(dir: &TempDir) -> SiteState {
    state::load(&dir.0).unwrap().unwrap()
}

fn literal(v: ValueRef) -> String {
    match v {
        ValueRef::Null => "NULL".into(),
        ValueRef::Integer(i) => i.to_string(),
        ValueRef::Real(f) => format!("{f:?}"),
        ValueRef::Text(t) => format!("'{}'", String::from_utf8_lossy(t).replace('\'', "''")),
        ValueRef::Blob(b) => {
            let mut s = String::from("X'");
            for x in b {
                write!(s, "{x:02X}").unwrap();
            }
            s + "'"
        }
    }
}

/// A dump in sqlite3 `.dump` style: tables with their rows, the virtual table's rows by
/// column name, `sqlite_sequence`, then indexes, triggers and views, and the row counts
/// `blog`'s own dump ends with (crates/cli/src/dump.rs).
fn dump(conn: &Connection) -> String {
    let mut out = String::from("PRAGMA defer_foreign_keys=TRUE;\n");
    let mut stmt = conn
        .prepare(
            "SELECT type, name, sql FROM sqlite_master WHERE sql IS NOT NULL \
             AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '_cf_%' ORDER BY rowid",
        )
        .unwrap();
    let objs: Vec<(String, String, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let virtuals: Vec<&str> = objs
        .iter()
        .filter(|(_, _, sql)| sql.starts_with("CREATE VIRTUAL TABLE"))
        .map(|(_, n, _)| n.as_str())
        .collect();
    let shadow = |n: &str| virtuals.iter().any(|v| n.starts_with(&format!("{v}_")));
    let mut counts = String::new();
    for (ty, name, sql) in &objs {
        if ty != "table" || shadow(name) {
            continue;
        }
        let n: i64 = conn
            .query_row(&format!("SELECT count(*) FROM \"{name}\""), [], |r| {
                r.get(0)
            })
            .unwrap();
        writeln!(counts, "{}\"{name}\" {n}", dump::ROWS).unwrap();
        writeln!(out, "{sql};").unwrap();
        let virt = virtuals.contains(&name.as_str());
        let cols: Vec<String> = conn
            .prepare(&format!("PRAGMA table_info(\"{name}\")"))
            .unwrap()
            .query_map([], |r| r.get::<_, String>(1))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        let select = if virt {
            format!("SELECT rowid, * FROM \"{name}\"")
        } else {
            format!("SELECT * FROM \"{name}\"")
        };
        let mut q = conn.prepare(&select).unwrap();
        let n = q.column_count();
        let mut rows = q.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            let vals: Vec<String> = (0..n).map(|i| literal(row.get_ref(i).unwrap())).collect();
            if virt {
                let names: Vec<String> = cols.iter().map(|c| format!("\"{c}\"")).collect();
                writeln!(
                    out,
                    "INSERT INTO \"{name}\"(rowid,{}) VALUES({});",
                    names.join(","),
                    vals.join(",")
                )
                .unwrap();
            } else {
                writeln!(out, "INSERT INTO \"{name}\" VALUES({});", vals.join(",")).unwrap();
            }
        }
    }
    let seq: Vec<(String, i64)> = conn
        .prepare("SELECT name, seq FROM sqlite_sequence")
        .map(|mut s| {
            s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect()
        })
        .unwrap_or_default();
    if !seq.is_empty() {
        out.push_str("DELETE FROM sqlite_sequence;\n");
        for (n, v) in seq {
            writeln!(out, "INSERT INTO \"sqlite_sequence\" VALUES('{n}',{v});").unwrap();
        }
    }
    for (ty, _, sql) in &objs {
        if ty != "table" {
            writeln!(out, "{sql};").unwrap();
        }
    }
    out + &counts + dump::END + "\n"
}

/// Writes the live database's dump to [`FILE`] and returns it.
fn save_backup(fake: &Fake, dir: &TempDir) -> String {
    let text = dump(&fake.state().sqlite[DB]);
    write_file(dir, &text);
    text
}

fn write_file(dir: &TempDir, text: &str) {
    let p = dir.0.join(FILE);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

fn setting(fake: &Fake, key: &str) -> Option<String> {
    fake.sql(
        DB,
        &format!("SELECT value_json FROM settings WHERE key = '{key}'"),
    )
    .first()
    .and_then(|r| r["value_json"].as_str().map(str::to_string))
}

fn put(fake: &Fake, key: &str, value: &str) {
    fake.exec(
        DB,
        &format!(
            "INSERT INTO settings (key, value_json, updated_at) VALUES ('{key}', '\"{value}\"', 1)"
        ),
    );
}

fn content_version(fake: &Fake) -> i64 {
    fake.sql(DB, "SELECT content_version FROM site WHERE id = 1")[0]["content_version"]
        .as_i64()
        .unwrap()
}

fn tables(fake: &Fake) -> Vec<String> {
    fake.sql(
        DB,
        "SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name",
    )
    .iter()
    .map(|r| r["name"].as_str().unwrap().to_string())
    .collect()
}

fn is_import(r: &Recorded) -> bool {
    r.path.ends_with("/import") || r.path.starts_with("/__upload/")
}

/// A request that may change something: anything but token checks, GETs and D1 queries
/// made only of `SELECT`s.
fn mutating(r: &Recorded) -> bool {
    if r.method == "GET" {
        return false;
    }
    if r.path.ends_with("/query") {
        let body: Value = serde_json::from_str(&r.body).unwrap_or_default();
        let sqls: Vec<String> = match body.get("batch").and_then(Value::as_array) {
            Some(b) => b
                .iter()
                .map(|s| s["sql"].as_str().unwrap_or("").to_string())
                .collect(),
            None => vec![body["sql"].as_str().unwrap_or("").to_string()],
        };
        return !sqls
            .iter()
            .all(|s| s.trim_start().to_ascii_uppercase().starts_with("SELECT"));
    }
    true
}

#[test]
fn without_yes_prints_the_plan_and_changes_nothing() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    save_backup(&fake, &dir);
    let before = fake.requests().len();
    let state_before = load(&dir);
    let r = restore_with(&fake, &dir, &opts(&bundle, false));
    assert_eq!(code(&r), Code::RestoreUnconfirmed, "{}", r.out);
    assert!(r.out.contains(FILE), "{}", r.out);
    assert!(r.out.contains("2026-11-01T08:00:00Z"), "{}", r.out);
    assert!(message(&r).contains("--yes"), "{}", message(&r));
    let after = fake.requests();
    let changed: Vec<String> = after[before..]
        .iter()
        .filter(|r| mutating(r) || r.path.contains("time_travel"))
        .map(|r| format!("{} {} {}", r.method, r.path, r.body))
        .collect();
    assert!(changed.is_empty(), "{changed:?}");
    assert_eq!(load(&dir), state_before);
}

#[test]
fn a_file_that_is_not_a_backup_is_refused_before_any_call() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    let before = fake.requests().len();
    for text in [
        "hello, this is not SQL",
        // Tables, but no d1_migrations: not a plumblog database.
        "CREATE TABLE settings (key TEXT PRIMARY KEY, value_json TEXT, updated_at INTEGER);\n",
        "CREATE TABLE d1_migrations (id INTEGER, name TEXT, applied_at TEXT);\n",
    ] {
        write_file(&dir, text);
        let r = restore_with(&fake, &dir, &opts(&bundle, true));
        assert_eq!(code(&r), Code::RestoreNotABackup, "{text}\n{}", r.out);
    }
    std::fs::remove_file(dir.0.join(FILE)).unwrap();
    let r = restore_with(&fake, &dir, &opts(&bundle, true));
    assert_eq!(code(&r), Code::RestoreUnreadable, "{}", r.out);
    assert_eq!(fake.requests().len(), before);
}

#[test]
fn another_sites_backup_needs_allow_other_site() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    let ours = setting(&fake, "site_id").unwrap();
    let text = save_backup(&fake, &dir);
    let theirs = "\"0123456789abcdef0123456789abcdef\"";
    assert_ne!(ours, theirs);
    write_file(&dir, &text.replace(&ours, theirs));
    let before = fake.requests().len();
    let r = restore_with(&fake, &dir, &opts(&bundle, true));
    assert_eq!(code(&r), Code::RestoreOtherSite, "{}", r.out);
    assert!(
        message(&r).contains("--allow-other-site"),
        "{}",
        message(&r)
    );
    assert!(!fake.requests()[before..].iter().any(mutating));
    assert_eq!(setting(&fake, "site_id").unwrap(), ours);

    let o = Options {
        allow_other_site: true,
        ..opts(&bundle, true)
    };
    ok(&restore_with(&fake, &dir, &o));
    assert_eq!(setting(&fake, "site_id").unwrap(), theirs);
}

#[test]
fn a_backup_newer_than_the_code_is_refused() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    fake.exec(
        DB,
        "INSERT INTO d1_migrations (name) VALUES ('0099_future.sql')",
    );
    save_backup(&fake, &dir);
    let before = fake.requests().len();
    let r = restore_with(&fake, &dir, &opts(&bundle, true));
    assert_eq!(code(&r), Code::RestoreBackupNewer, "{}", r.out);
    assert!(message(&r).contains("0099_future.sql"), "{}", message(&r));
    assert!(!fake.requests()[before..].iter().any(mutating));
}

#[test]
fn the_bookmark_is_saved_before_the_first_change() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    save_backup(&fake, &dir);
    let seen: Arc<Mutex<Option<SiteState>>> = Arc::default();
    let (s, d) = (Arc::clone(&seen), dir.0.clone());
    fake.on_request("/import", move || {
        *s.lock().unwrap() = state::load(&d).unwrap();
    });
    let before = fake.requests().len();
    let r = restore_with(&fake, &dir, &opts(&bundle, true));
    ok(&r);
    let reqs = fake.requests()[before..].to_vec();
    let mark = reqs
        .iter()
        .position(|r| r.path.ends_with("/time_travel/bookmark"))
        .expect("a bookmark");
    let first_change = reqs.iter().position(mutating).expect("a change");
    assert!(
        mark < first_change,
        "bookmark {mark}, change {first_change}"
    );
    assert!(is_import(&reqs[first_change]), "{:?}", reqs[first_change]);
    let at_import = seen.lock().unwrap().clone().expect("state at the import");
    let rec = at_import.rollbacks.last().expect("a restore record");
    assert_eq!(rec.kind, "file");
    assert_eq!(rec.to, FILE);
    let bm = rec.previous.clone().unwrap();
    assert!(bm.starts_with("bm-db-1-"), "{bm}");
    assert!(
        r.out.contains(&format!("blog rollback --db {bm} --yes")),
        "{}",
        r.out
    );
    let done = load(&dir);
    let rec = done.rollbacks.last().unwrap();
    assert_eq!(rec.previous.as_deref(), Some(bm.as_str()));
    assert_eq!(rec.outcome.as_deref(), Some("restored"));
}

#[test]
fn a_full_restore_replaces_every_row_and_table() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    put(&fake, "only_in_backup", "b");
    fake.exec(
        DB,
        "INSERT INTO posts_fts (rowid, title, body) VALUES (7, 'kept title', 'kept body')",
    );
    save_backup(&fake, &dir);
    fake.exec(DB, "DELETE FROM settings WHERE key = 'only_in_backup'");
    fake.exec(DB, "DELETE FROM posts_fts");
    put(&fake, "only_in_live", "l");
    fake.exec(
        DB,
        "CREATE TABLE old_extra (x INTEGER); CREATE INDEX old_extra_x ON old_extra (x); \
         CREATE VIEW old_view AS SELECT x FROM old_extra; \
         CREATE TRIGGER old_trigger AFTER INSERT ON old_extra BEGIN \
           INSERT INTO old_extra (x) SELECT 1 WHERE 0; END;",
    );
    fake.exec(DB, "UPDATE site SET content_version = 40 WHERE id = 1");
    let tables_in_backup: Vec<String> = tables(&fake)
        .into_iter()
        .filter(|t| t != "old_extra")
        .collect();

    let before = fake.requests().len();
    let r = restore_with(&fake, &dir, &opts(&bundle, true));
    ok(&r);
    assert_eq!(setting(&fake, "only_in_backup").as_deref(), Some("\"b\""));
    assert_eq!(setting(&fake, "only_in_live"), None);
    assert_eq!(tables(&fake), tables_in_backup);
    assert!(
        fake.sql(DB, "SELECT name FROM sqlite_master WHERE name LIKE 'old_%'")
            .is_empty()
    );
    let fts = fake.sql(
        DB,
        "SELECT rowid AS id FROM posts_fts WHERE posts_fts MATCH 'kept'",
    );
    assert_eq!(fts.len(), 1);
    // Above every version used before the restore, so no cached page can be served.
    assert!(content_version(&fake) > 40, "{}", content_version(&fake));

    let reqs: Vec<Recorded> = fake.requests()[before..].to_vec();
    let put_req = reqs
        .iter()
        .find(|r| r.method == "PUT" && r.path.starts_with("/__upload/"))
        .expect("an upload");
    assert_eq!(put_req.header("authorization"), None);
    assert!(!put_req.headers.iter().any(|(_, v)| v.contains(GOOD)));
    // The uploaded file drops what exists, then is the dump itself.
    assert!(put_req.body.contains("DROP TABLE IF EXISTS \"old_extra\""));
    assert!(put_req.body.contains("DROP VIEW IF EXISTS \"old_view\""));
    assert!(!put_req.body.contains("\"sqlite_sequence\";"));
    assert!(!put_req.body.contains("DROP TABLE IF EXISTS \"_cf_"));
    assert!(
        put_req
            .body
            .contains(&std::fs::read_to_string(dir.0.join(FILE)).unwrap())
    );
    // No token and no upload URL in what the owner sees.
    assert!(
        !r.out.contains(GOOD) && !r.out.contains("X-Amz-Signature"),
        "{}",
        r.out
    );
    let log = fake.sql(DB, "SELECT detail FROM system_log");
    assert!(
        log.iter()
            .any(|row| row["detail"].as_str().unwrap_or("").contains(FILE)),
        "{log:?}"
    );
}

#[test]
fn an_old_backup_gets_the_missing_migrations_again() {
    let mut old = migrations::bundled();
    let last = old.pop().unwrap();
    let (fake, dir, bundle) = set_up(&old);
    save_backup(&fake, &dir);
    // The live site moved on to the newest code.
    write_migrations(&bundle, &migrations::bundled());
    fake.exec(DB, &last.sql);
    fake.exec(
        DB,
        &format!("INSERT INTO d1_migrations (name) VALUES ('{}')", last.name),
    );
    let r = restore_with(&fake, &dir, &opts(&bundle, true));
    ok(&r);
    assert!(r.out.contains(&last.name), "{}", r.out);
    let names: Vec<String> = fake
        .sql(DB, "SELECT name FROM d1_migrations ORDER BY id")
        .iter()
        .map(|r| r["name"].as_str().unwrap().to_string())
        .collect();
    let all: Vec<String> = migrations::bundled().into_iter().map(|m| m.name).collect();
    assert_eq!(names, all);
    assert_eq!(load(&dir).migrations.applied, all);
}

#[test]
fn a_damaged_upload_stops_before_ingest() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    save_backup(&fake, &dir);
    put(&fake, "only_in_live", "l");
    fake.state().upload_etag = Some("0".repeat(32));
    let before = fake.requests().len();
    let r = restore_with(&fake, &dir, &opts(&bundle, true));
    assert_eq!(code(&r), Code::RestoreUploadMismatch, "{}", r.out);
    let reqs = fake.requests()[before..].to_vec();
    assert!(
        !reqs.iter().any(|r| r.body.contains("\"ingest\"")),
        "ingest was sent"
    );
    assert_eq!(setting(&fake, "only_in_live").as_deref(), Some("\"l\""));
    let rec = load(&dir).rollbacks.last().cloned().unwrap();
    assert_eq!(rec.outcome.as_deref(), Some("upload_failed"));
}

#[test]
fn an_import_error_stops_with_the_undo_command_and_keeps_the_bookmark() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    save_backup(&fake, &dir);
    put(&fake, "only_in_live", "l");
    fake.state().import_error = Some("near \"x\": syntax error".into());
    fake.state().import_polls = 2;
    let r = restore_with(&fake, &dir, &opts(&bundle, true));
    assert_eq!(code(&r), Code::RestoreImportFailed, "{}", r.out);
    let rec = load(&dir).rollbacks.last().cloned().unwrap();
    let bm = rec.previous.clone().unwrap();
    assert_eq!(rec.outcome.as_deref(), Some("import_failed"));
    let undo = format!("blog rollback --db {bm} --yes");
    assert!(message(&r).contains(&undo), "{}", message(&r));
    assert!(message(&r).contains("syntax error"), "{}", message(&r));
    // The fake's import is a transaction: nothing was applied, nor anything after it.
    assert_eq!(setting(&fake, "only_in_live").as_deref(), Some("\"l\""));
}

#[test]
fn an_import_that_never_finishes_may_have_been_applied() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    save_backup(&fake, &dir);
    fake.state().import_polls = usize::MAX;
    let r = restore_with(&fake, &dir, &opts(&bundle, true));
    assert_eq!(code(&r), Code::RestoreImportUncertain, "{}", r.out);
    let rec = load(&dir).rollbacks.last().cloned().unwrap();
    assert_eq!(rec.outcome.as_deref(), Some("import_uncertain"));
    let undo = format!("blog rollback --db {} --yes", rec.previous.unwrap());
    assert!(message(&r).contains(&undo), "{}", message(&r));
}

#[test]
fn the_content_version_moves_past_a_bump_made_after_the_checks() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    save_backup(&fake, &dir);
    // The site bumps the version while the owner reads the plan: after step 2 read it.
    fake.state().after_query.push((
        "type IN ('table', 'view', 'trigger'".into(),
        "UPDATE site SET content_version = 500 WHERE id = 1".into(),
    ));
    let r = restore_with(&fake, &dir, &opts(&bundle, true));
    ok(&r);
    // Past 500 by the margin, so bumps during the upload and import cannot catch up.
    assert!(content_version(&fake) > 1500, "{}", content_version(&fake));
}

#[test]
fn rows_the_live_site_adds_after_the_import_are_not_a_mismatch() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    save_backup(&fake, &dir);
    fake.state().after_import = Some(
        "INSERT INTO settings (key, value_json, updated_at) VALUES ('written_meanwhile', '1', 1)"
            .into(),
    );
    let r = restore_with(&fake, &dir, &opts(&bundle, true));
    ok(&r);
    assert_eq!(
        load(&dir).rollbacks.last().unwrap().outcome.as_deref(),
        Some("restored")
    );
}

#[test]
fn missing_rows_still_get_the_migrations_and_the_version_bump() {
    let mut old = migrations::bundled();
    let last = old.pop().unwrap();
    let (fake, dir, bundle) = set_up(&old);
    put(&fake, "lost_in_import", "x");
    save_backup(&fake, &dir);
    write_migrations(&bundle, &migrations::bundled());
    fake.exec(DB, &last.sql);
    fake.exec(
        DB,
        &format!("INSERT INTO d1_migrations (name) VALUES ('{}')", last.name),
    );
    fake.exec(DB, "UPDATE site SET content_version = 40 WHERE id = 1");
    fake.state().after_import = Some("DELETE FROM settings WHERE key = 'lost_in_import'".into());
    let r = restore_with(&fake, &dir, &opts(&bundle, true));
    assert_eq!(code(&r), Code::RestoreCountMismatch, "{}", r.out);
    assert!(message(&r).contains("settings "), "{}", message(&r));
    assert_eq!(
        load(&dir).rollbacks.last().unwrap().outcome.as_deref(),
        Some("count_mismatch")
    );
    // The database still matches the code, and no page cached before is served.
    let names: Vec<String> = fake
        .sql(DB, "SELECT name FROM d1_migrations ORDER BY id")
        .iter()
        .map(|r| r["name"].as_str().unwrap().to_string())
        .collect();
    let all: Vec<String> = migrations::bundled().into_iter().map(|m| m.name).collect();
    assert_eq!(names, all);
    assert!(content_version(&fake) > 40, "{}", content_version(&fake));
}

#[test]
fn ids_handed_out_after_the_backup_are_not_reused() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    fake.exec(
        DB,
        "INSERT INTO tags (id, slug, name) VALUES (1, 'a', 'A'), (2, 'b', 'B')",
    );
    save_backup(&fake, &dir);
    // Made and deleted after the backup: a webhook or a link may still name ids 3 and 4.
    fake.exec(
        DB,
        "INSERT INTO tags (slug, name) VALUES ('c', 'C'), ('d', 'D'); DELETE FROM tags WHERE id > 2",
    );
    let r = restore_with(&fake, &dir, &opts(&bundle, true));
    ok(&r);
    fake.exec(DB, "INSERT INTO tags (slug, name) VALUES ('e', 'E')");
    let id = fake.sql(DB, "SELECT id FROM tags WHERE slug = 'e'")[0]["id"]
        .as_i64()
        .unwrap();
    assert_eq!(id, 5);
}

#[test]
fn a_failed_upload_is_told_in_the_owners_language() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    save_backup(&fake, &dir);
    fake.state().upload_status = Some(503);
    let vi = Text::new("vi");
    let r = restore_in(&fake, &dir, &opts(&bundle, true), &vi);
    assert_eq!(code(&r), Code::RestoreUploadFailed, "{}", r.out);
    let reason = vi.t(
        plumblog_i18n::key!("cli.restore.reason_upload_status"),
        &[("status", "503".into())],
    );
    assert!(message(&r).contains(&reason), "{}", message(&r));
}

/// `blog deploy` (or `blog rollback` with `rb`) from the restore test's folder.
fn deploy_or_rollback(
    fake: &Fake,
    dir: &TempDir,
    bundle: &Path,
    rb: Option<&plumblog_cli::deploy::RollbackOptions>,
) -> Ran {
    let mut c = client(fake, GOOD);
    let migs = migrations::bundled();
    let text = en();
    let cx = Context {
        dir: &dir.0,
        migrations: &migs,
        now_ms: NOW,
        text: &text,
    };
    let clock = FakeClock::default();
    let rt = plumblog_cli::deploy::Runtime {
        clock: &clock,
        smoke_base: Some(fake.base.clone()),
        smoke_budget: plumblog_cli::deploy::SMOKE_BUDGET,
    };
    let opts = plumblog_cli::deploy::Options {
        bundle: Some(bundle.to_path_buf()),
        ..plumblog_cli::deploy::Options::default()
    };
    let mut buf = Vec::new();
    let result = match rb {
        Some(o) => plumblog_cli::deploy::rollback(&mut c, None, o, &cx, &rt, &mut buf),
        None => plumblog_cli::deploy::run(&mut c, None, &opts, &cx, &rt, &mut buf),
    };
    Ran {
        result,
        out: String::from_utf8(buf).unwrap(),
    }
}

#[test]
fn a_migration_failing_after_the_restore_points_back_to_before_the_restore() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    save_backup(&fake, &dir);
    let mut migs = migrations::bundled();
    migs.push(Migration {
        name: "9999_broken.sql".into(),
        sql: "CREATE TABLE posts (x);".into(),
    });
    write_migrations(&bundle, &migs);
    let r = restore_with(&fake, &dir, &opts(&bundle, true));
    assert_eq!(code(&r), Code::MigrationFailed, "{}", r.out);
    let st = load(&dir);
    let mark = st.rollbacks.last().unwrap().previous.clone().unwrap();
    let after_import = st.migrations.last_bookmark.clone().unwrap();
    assert_ne!(mark, after_import);
    let undo = format!("blog rollback --db {mark} --yes");
    // The restore's own undo, in the error, in the state and in what deploy says.
    assert!(message(&r).contains(&undo), "{}", message(&r));
    assert!(!message(&r).contains(&after_import), "{}", message(&r));
    assert_eq!(st.migrations.failed.unwrap().bookmark, mark);
    let d = deploy_or_rollback(&fake, &dir, &bundle, None);
    assert_eq!(code(&d), Code::MigrationRestorePending, "{}", d.out);
    assert!(message(&d).contains(&undo), "{}", message(&d));
    assert!(!message(&d).contains(&after_import), "{}", message(&d));
    // Running the printed undo clears the failure.
    let rb = plumblog_cli::deploy::RollbackOptions {
        db: Some(mark.clone()),
        yes: true,
        ..plumblog_cli::deploy::RollbackOptions::default()
    };
    ok(&deploy_or_rollback(&fake, &dir, &bundle, Some(&rb)));
    assert_eq!(load(&dir).migrations.failed, None);
    let d = deploy_or_rollback(&fake, &dir, &bundle, None);
    if let Err(e) = &d.result {
        assert_ne!(e.code, Code::MigrationRestorePending, "{}", e.message);
    }
}

#[test]
fn rows_the_site_prunes_itself_only_warn_but_other_tables_still_stop() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    fake.exec(
        DB,
        "INSERT INTO auth_challenges (challenge_hash, purpose, expires_at) \
           VALUES ('h1', 'login', 1);",
    );
    save_backup(&fake, &dir);
    // The cron removes the expired challenge between the import and the count.
    fake.state().after_import = Some("DELETE FROM auth_challenges".into());
    let r = restore_with(&fake, &dir, &opts(&bundle, true));
    ok(&r);
    assert!(r.out.contains("auth_challenges 1 -> 0"), "{}", r.out);
    // A row lost from a table the site never prunes is still a mismatch.
    fake.state().after_import = Some("DELETE FROM settings WHERE key = 'site_id'".into());
    let r = restore_with(&fake, &dir, &opts(&bundle, true));
    assert_eq!(code(&r), Code::RestoreCountMismatch, "{}", r.out);
}

/// Credentials as the backup holds them: an owner with a live and a revoked token, a
/// session, a pending and a used invite, a `WebAuthn` challenge, an owner reset code and a
/// setup code, plus a login failure (rate limiting, not a credential).
fn add_credentials(fake: &Fake) {
    fake.exec(
        DB,
        "INSERT INTO users (id, login, slug, display_name, role, password_hash, created_at, \
           updated_at) VALUES (1, 'owner', 'owner', 'Owner', 'owner', 'h', 1, 1); \
         INSERT INTO api_tokens (id, user_id, name, token_hash, scope, created_at, revoked_at) \
           VALUES (1, 1, 'live', 'th1', 'owner', 1, NULL), (2, 1, 'old', 'th2', 'owner', 1, 5); \
         INSERT INTO sessions (token_hash, user_id, created_at, expires_at) \
           VALUES ('sh1', 1, 1, 9999999999999); \
         INSERT INTO invites (code_hash, role, created_by, created_at, expires_at, used_at, \
           used_by) VALUES ('ih1', 'writer', 1, 1, 9999999999999, NULL, NULL), \
           ('ih2', 'writer', 1, 1, 9999999999999, 2, 1); \
         INSERT INTO auth_challenges (challenge_hash, purpose, user_id, expires_at) \
           VALUES ('ch1', 'register', 1, 9999999999999); \
         INSERT INTO login_failures (key_hash, count, window_start) VALUES ('lf1', 3, 1);",
    );
    for k in plumblog_store::OWNER_RESET_KEYS.iter().chain(&[
        plumblog_store::SETUP_CODE_HASH_KEY,
        plumblog_store::SETUP_CODE_EXPIRES_KEY,
    ]) {
        fake.exec(
            DB,
            &format!(
                "INSERT OR REPLACE INTO settings (key, value_json, updated_at) VALUES ('{k}', '1', 1)"
            ),
        );
    }
}

fn count(fake: &Fake, sql: &str) -> i64 {
    fake.sql(DB, &format!("SELECT count(*) AS n FROM {sql}"))[0]["n"]
        .as_i64()
        .unwrap()
}

/// Every credential the backup brought back is unusable; passwords, passkeys, used invites
/// and the rate limiter are left as restored.
fn assert_credentials_reset(fake: &Fake) {
    assert_eq!(count(fake, "sessions"), 0);
    assert_eq!(count(fake, "api_tokens WHERE revoked_at IS NULL"), 0);
    assert_eq!(
        count(
            fake,
            "api_tokens WHERE id = 1 AND revoked_at = 1790000000000"
        ),
        1
    );
    assert_eq!(count(fake, "api_tokens WHERE id = 2 AND revoked_at = 5"), 1);
    assert_eq!(count(fake, "invites WHERE used_at IS NULL"), 0);
    assert_eq!(count(fake, "invites WHERE code_hash = 'ih2'"), 1);
    assert_eq!(count(fake, "auth_challenges"), 0);
    assert_eq!(count(fake, "login_failures"), 1);
    assert_eq!(count(fake, "users WHERE password_hash = 'h'"), 1);
    for k in plumblog_store::OWNER_RESET_KEYS.iter().chain(&[
        plumblog_store::SETUP_CODE_HASH_KEY,
        plumblog_store::SETUP_CODE_EXPIRES_KEY,
    ]) {
        assert_eq!(setting(fake, k), None, "{k}");
    }
}

#[test]
fn credentials_revoked_after_the_backup_stay_revoked() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    add_credentials(&fake);
    save_backup(&fake, &dir);
    // Revoked on the live site after the backup.
    fake.exec(
        DB,
        "UPDATE api_tokens SET revoked_at = 7 WHERE id = 1; DELETE FROM sessions; \
         DELETE FROM invites WHERE used_at IS NULL; DELETE FROM settings WHERE key LIKE 'owner_reset%'",
    );
    let r = restore_with(&fake, &dir, &opts(&bundle, true));
    ok(&r);
    assert_credentials_reset(&fake);
    assert!(r.out.contains("sign in again"), "{}", r.out);
    assert!(r.out.contains("2026-11-01T08:00:00Z"), "{}", r.out);
    assert!(r.out.contains("Restore from"), "{}", r.out);
}

#[test]
fn an_old_backup_gets_its_credentials_reset_after_the_migrations() {
    let mut old = migrations::bundled();
    let last = old.pop().unwrap();
    let (fake, dir, bundle) = set_up(&old);
    add_credentials(&fake);
    save_backup(&fake, &dir);
    write_migrations(&bundle, &migrations::bundled());
    fake.exec(DB, &last.sql);
    fake.exec(
        DB,
        &format!("INSERT INTO d1_migrations (name) VALUES ('{}')", last.name),
    );
    let r = restore_with(&fake, &dir, &opts(&bundle, true));
    ok(&r);
    assert_credentials_reset(&fake);
    let out = &r.out;
    let migrated = out.find(&last.name).expect("migration line");
    let reset = out.find("sign in again").expect("reset line");
    assert!(migrated < reset, "{out}");
}

#[test]
fn a_failed_credential_reset_is_an_error_with_the_rerun() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    add_credentials(&fake);
    save_backup(&fake, &dir);
    fake.fail_body("UPDATE api_tokens SET revoked_at", Failure::Status(400));
    let r = restore_with(&fake, &dir, &opts(&bundle, true));
    assert_eq!(code(&r), Code::RestoreCredentialsNotReset, "{}", r.out);
    assert_eq!(code(&r).as_str(), "restore.credentials_not_reset");
    let m = message(&r);
    assert!(m.contains("blog restore --reset-credentials --yes"), "{m}");
    assert!(m.contains("blog rollback --db "), "{m}");
    assert!(!r.out.contains("Restore from"), "{}", r.out);
    assert!(!r.out.contains("sign in again"), "{}", r.out);
    assert_eq!(count(&fake, "api_tokens WHERE revoked_at IS NULL"), 1);
    assert_eq!(
        load(&dir).rollbacks.last().unwrap().outcome.as_deref(),
        Some("credentials_not_reset")
    );

    // The re-run finishes the job, and is safe to repeat.
    for _ in 0..2 {
        let r = reset_with(&fake, &dir);
        ok(&r);
        assert_credentials_reset(&fake);
        assert!(r.out.contains("sign in again"), "{}", r.out);
        assert!(r.out.contains("2026-11-01T08:00:00Z"), "{}", r.out);
    }
    assert_eq!(
        load(&dir).rollbacks.last().unwrap().outcome.as_deref(),
        Some("restored")
    );
}

#[test]
fn a_table_missing_from_the_backup_does_not_stop_the_reset() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    add_credentials(&fake);
    fake.exec(DB, "DROP TABLE auth_challenges");
    save_backup(&fake, &dir);
    let r = restore_with(&fake, &dir, &opts(&bundle, true));
    ok(&r);
    assert!(!tables(&fake).contains(&"auth_challenges".to_string()));
    assert_eq!(count(&fake, "sessions"), 0);
    assert_eq!(count(&fake, "api_tokens WHERE revoked_at IS NULL"), 0);
    let r = reset_with(&fake, &dir);
    ok(&r);
}

fn reset_with(fake: &Fake, dir: &TempDir) -> Ran {
    let mut c = client(fake, GOOD);
    let migs = migrations::bundled();
    let text = en();
    let cx = Context {
        dir: &dir.0,
        migrations: &migs,
        now_ms: NOW,
        text: &text,
    };
    let mut buf = Vec::new();
    let result = restore::reset_credentials(&mut c, None, &cx, &mut buf);
    Ran {
        result,
        out: String::from_utf8(buf).unwrap(),
    }
}

/// tools/worker-test replays the restore's credential reset from this file, every table
/// present; `$now` stands for the time. `BLESS=1` rewrites it.
#[test]
fn the_worker_test_fixture_is_the_credential_reset_sql() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/worker-test/fixtures/restore-reset.json");
    let all = [
        "sessions",
        "api_tokens",
        "invites",
        "auth_challenges",
        "settings",
    ];
    let stmts: Vec<Value> = restore::credential_reset_statements(&all, "$now")
        .into_iter()
        .map(|(sql, params)| serde_json::json!({"sql": sql, "params": params}))
        .collect();
    assert_eq!(stmts.len(), all.len());
    let want = serde_json::to_string_pretty(&Value::Array(stmts)).unwrap() + "\n";
    if std::env::var_os("BLESS").is_some() {
        std::fs::write(&path, &want).unwrap();
    }
    let got = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        got == want,
        "{} is stale: run `BLESS=1 cargo test -p plumblog-cli --test cloudflare restore::` after changing the reset SQL",
        path.display()
    );
}

fn backup_with(fake: &Fake, dir: &TempDir, out: &str) -> Ran {
    let mut c = client(fake, GOOD);
    let migs = migrations::bundled();
    let text = en();
    let cx = Context {
        dir: &dir.0,
        migrations: &migs,
        now_ms: NOW,
        text: &text,
    };
    let o = plumblog_cli::backup::Options {
        no_media: true,
        out: Some(PathBuf::from(out)),
    };
    let mut buf = Vec::new();
    let result = plumblog_cli::backup::run(&mut c, None, &o, &cx, &mut buf);
    Ran {
        result,
        out: String::from_utf8(buf).unwrap(),
    }
}

const REMINDER: &str = "did not finish resetting credentials";

#[test]
fn a_restore_killed_after_the_import_began_is_remembered_until_the_reset() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    add_credentials(&fake);
    save_backup(&fake, &dir);
    // The state file as it is while the import runs: what a Ctrl-C or a crash leaves.
    let path = state::path(&dir.0);
    let seen = Arc::new(Mutex::new(String::new()));
    let keep = Arc::clone(&seen);
    let p = path.clone();
    fake.on_request("/import", move || {
        *keep.lock().unwrap() = std::fs::read_to_string(&p).unwrap();
    });
    ok(&restore_with(&fake, &dir, &opts(&bundle, true)));
    let killed = seen.lock().unwrap().clone();
    std::fs::write(&path, &killed).unwrap();
    assert_eq!(
        load(&dir).rollbacks.last().unwrap().outcome.as_deref(),
        Some("importing")
    );
    // The backup's credentials are live again (the import ran, step 8 never did).
    add_credentials_again(&fake);

    let b = backup_with(&fake, &dir, "b1");
    ok(&b);
    assert!(b.out.contains(REMINDER), "{}", b.out);
    assert!(
        b.out.contains("blog restore --reset-credentials --yes"),
        "{}",
        b.out
    );
    let d = deploy_or_rollback(&fake, &dir, &bundle, None);
    assert!(d.out.contains(REMINDER), "{}", d.out);
    let plan = restore_with(&fake, &dir, &opts(&bundle, false));
    assert_eq!(code(&plan), Code::RestoreUnconfirmed);
    assert!(plan.out.contains(REMINDER), "{}", plan.out);

    let r = reset_with(&fake, &dir);
    ok(&r);
    assert_credentials_reset(&fake);
    assert_eq!(
        load(&dir).rollbacks.last().unwrap().outcome.as_deref(),
        Some("restored")
    );
    let b = backup_with(&fake, &dir, "b2");
    ok(&b);
    assert!(!b.out.contains(REMINDER), "{}", b.out);
}

/// The live database after a killed restore: the backup's rows came back, unreset.
fn add_credentials_again(fake: &Fake) {
    fake.exec(
        DB,
        "INSERT OR IGNORE INTO sessions (token_hash, user_id, created_at, expires_at) \
           VALUES ('sh1', 1, 1, 9999999999999); \
         UPDATE api_tokens SET revoked_at = NULL WHERE id = 1;",
    );
}

#[test]
fn a_restored_database_without_an_owner_says_to_run_setup() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    save_backup(&fake, &dir);
    assert_eq!(count(&fake, "users"), 0);
    let r = restore_with(&fake, &dir, &opts(&bundle, true));
    ok(&r);
    assert!(r.out.contains("has no users"), "{}", r.out);
    assert!(r.out.contains("`blog setup`"), "{}", r.out);
    assert!(!r.out.contains("sign in again"), "{}", r.out);
    assert!(r.out.contains("check webhooks"), "{}", r.out);
    // With an owner: sign in, and `blog admin reset` if that fails.
    add_credentials(&fake);
    save_backup(&fake, &dir);
    let r = restore_with(&fake, &dir, &opts(&bundle, true));
    ok(&r);
    assert!(r.out.contains("sign in again"), "{}", r.out);
    assert!(r.out.contains("`blog admin reset`"), "{}", r.out);
    assert!(!r.out.contains("no active owner"), "{}", r.out);
    assert!(!r.out.contains("has no users"), "{}", r.out);
}

#[test]
fn a_restored_database_with_writers_but_no_owner_says_to_recover_by_hand() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    fake.exec(
        DB,
        "INSERT INTO users (id, login, slug, display_name, role, password_hash, created_at, \
           updated_at) VALUES (1, 'w', 'w', 'W', 'writer', 'h', 1, 1), \
           (2, 'o', 'o', 'O', 'owner', 'h', 1, 1); \
         UPDATE users SET disabled_at = 5 WHERE id = 2;",
    );
    save_backup(&fake, &dir);
    let r = restore_with(&fake, &dir, &opts(&bundle, true));
    ok(&r);
    assert!(r.out.contains("sign in again"), "{}", r.out);
    assert!(r.out.contains("no active owner"), "{}", r.out);
    assert!(r.out.contains("by hand in the D1 database"), "{}", r.out);
    assert!(!r.out.contains("`blog setup`"), "{}", r.out);
}

#[test]
fn a_row_count_mismatch_still_resets_and_a_failed_reset_is_only_printed() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    add_credentials(&fake);
    put(&fake, "lost_in_import", "x");
    save_backup(&fake, &dir);
    fake.state().after_import = Some("DELETE FROM settings WHERE key = 'lost_in_import'".into());
    let r = restore_with(&fake, &dir, &opts(&bundle, true));
    assert_eq!(code(&r), Code::RestoreCountMismatch, "{}", r.out);
    assert_credentials_reset(&fake);

    add_credentials_again(&fake);
    fake.state().after_import = Some("DELETE FROM settings WHERE key = 'lost_in_import'".into());
    fake.fail_body("UPDATE api_tokens SET revoked_at", Failure::Status(400));
    let r = restore_with(&fake, &dir, &opts(&bundle, true));
    assert_eq!(code(&r), Code::RestoreCountMismatch, "{}", r.out);
    assert!(r.out.contains("restore.credentials_not_reset"), "{}", r.out);
    assert!(
        r.out.contains("blog restore --reset-credentials --yes"),
        "{}",
        r.out
    );
}

#[test]
fn a_failed_migration_still_resets_and_a_failed_reset_is_only_printed() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    add_credentials(&fake);
    save_backup(&fake, &dir);
    let mut migs = migrations::bundled();
    migs.push(Migration {
        name: "9999_broken.sql".into(),
        sql: "CREATE TABLE posts (x);".into(),
    });
    write_migrations(&bundle, &migs);
    let r = restore_with(&fake, &dir, &opts(&bundle, true));
    assert_eq!(code(&r), Code::MigrationFailed, "{}", r.out);
    assert_credentials_reset(&fake);

    add_credentials_again(&fake);
    fake.fail_body("UPDATE api_tokens SET revoked_at", Failure::Status(400));
    let r = restore_with(&fake, &dir, &opts(&bundle, true));
    assert_eq!(code(&r), Code::MigrationFailed, "{}", r.out);
    assert!(r.out.contains("restore.credentials_not_reset"), "{}", r.out);
}
