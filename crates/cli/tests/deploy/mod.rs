//! `blog deploy` and `blog rollback` against the fake API: real migrations in SQLite, a fake
//! site answering `/healthz` with the active version, and a virtual clock (no real waits).

use super::{GOOD, TempDir, client};
use crate::fake_cf::{Failure, Fake, Recorded};
use crate::setup::{ALL, Ran as SetupRan, ok as setup_ok, run_with as setup_with, secret, site};
use plumblog_cli::deploy::{self, Clock, Options, RollbackOptions, Runtime};
use plumblog_cli::migrations::{self, Migration};
use plumblog_cli::setup::{Context, Failure as RunFailure, Options as SetupOptions};
use plumblog_cli::state::{self, SiteState};
use plumblog_cli::text::{Code, Text};
use serde_json::{Value, json};
use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

const NOW: i64 = 1_790_000_000_000;
/// Version ids the fake hands out, in order: setup's placeholder, then each upload.
const V1: &str = "00000000-0000-4000-8000-000000000001";
const V2: &str = "00000000-0000-4000-8000-000000000002";
const WORKERS: &str = "/accounts/acc1/workers/scripts/plumblog";
/// The test smoke URL is `<fake>/<host><path>`.
const HOST: &str = "/plumblog.steven.workers.dev";

/// Real time plus virtual waits: `sleep` moves time without waiting, slow calls still count.
struct FakeClock(std::time::Instant, Cell<Duration>);

impl Default for FakeClock {
    fn default() -> Self {
        Self(std::time::Instant::now(), Cell::new(Duration::ZERO))
    }
}

impl Clock for FakeClock {
    fn elapsed(&self) -> Duration {
        self.0.elapsed() + self.1.get()
    }
    fn sleep(&self, d: Duration) {
        self.1.set(self.1.get() + d);
    }
}

const V3: &str = "00000000-0000-4000-8000-000000000003";
/// What serves after setup: the placeholder's code again, made by the second secret put
/// (`WEBHOOK_KEY`, then `TURNSTILE_SECRET`), as Cloudflare makes a version per secret change.
const P: &str = "00000000-0000-4000-9000-000000000002";

/// Replaces the bundle's migrations/ with `migs`.
fn write_migrations(bundle: &Path, migs: &[Migration]) {
    let d = bundle.join("migrations");
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    for m in migs {
        std::fs::write(d.join(&m.name), &m.sql).unwrap();
    }
}

struct Ran {
    result: Result<(), RunFailure>,
    out: String,
    clock: Duration,
}

fn en() -> Text {
    Text::new("en")
}

fn assets() -> Vec<(&'static str, &'static [u8])> {
    vec![
        ("theme/minimal/site.css", b"body{margin:0}".as_slice()),
        ("theme/minimal/font.woff2", b"wOF2fake".as_slice()),
        ("admin/app.js", b"console.log(1)".as_slice()),
        ("admin/index.html", b"<!doctype html>".as_slice()),
        ("plugin/toc/toc.js", b"toc()".as_slice()),
    ]
}

/// A release bundle as scripts/build-bundle lays it out.
fn make_bundle(dir: &TempDir) -> PathBuf {
    let b = dir.0.join("bundle");
    let w = b.join("worker");
    std::fs::create_dir_all(w.join("worker")).unwrap();
    std::fs::write(w.join("package.json"), r#"{"main": "index.js"}"#).unwrap();
    std::fs::write(
        w.join("index.js"),
        "import source wasm from \"./index_bg.wasm\";\nexport default {};\n",
    )
    .unwrap();
    std::fs::write(w.join("index_bg.wasm"), b"\0asm\x01\0\0\0").unwrap();
    std::fs::write(w.join("worker/shim.mjs"), "export * from '../index.js';").unwrap();
    for (p, bytes) in assets() {
        let f = b.join("assets").join(p);
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(f, bytes).unwrap();
    }
    std::fs::write(
        b.join("assets/_headers"),
        "/admin/*\n  Cache-Control: no-cache\n",
    )
    .unwrap();
    write_migrations(&b, &migrations::bundled());
    b
}

/// A site made by `blog setup` with `migs` applied, and its bundle.
fn set_up(migs: &[Migration]) -> (Fake, TempDir, PathBuf) {
    let fake = Fake::start(site(ALL));
    let dir = TempDir::new();
    let r: SetupRan = setup_with(&fake, &dir, &SetupOptions::default(), migs, &en());
    setup_ok(&r);
    fake.state().site.script = "plumblog".into();
    let bundle = make_bundle(&dir);
    (fake, dir, bundle)
}

fn all_but_last() -> Vec<Migration> {
    let mut m = migrations::bundled();
    m.pop();
    m
}

fn deploy_with(fake: &Fake, dir: &TempDir, opts: &Options, migs: &[Migration], text: &Text) -> Ran {
    deploy_budget(fake, dir, opts, migs, text, deploy::SMOKE_BUDGET)
}

/// `migs` are written into the bundle: deploy takes migrations from it.
fn deploy_budget(
    fake: &Fake,
    dir: &TempDir,
    opts: &Options,
    migs: &[Migration],
    text: &Text,
    budget: Duration,
) -> Ran {
    if let Some(b) = &opts.bundle {
        write_migrations(b, migs);
    }
    let mut c = client(fake, GOOD);
    let cx = Context {
        dir: &dir.0,
        migrations: migs,
        now_ms: NOW,
        text,
    };
    let clock = FakeClock::default();
    let rt = Runtime {
        clock: &clock,
        smoke_base: Some(fake.base.clone()),
        smoke_budget: budget,
    };
    let mut buf = Vec::new();
    let result = deploy::run(&mut c, None, opts, &cx, &rt, &mut buf);
    Ran {
        result,
        out: String::from_utf8(buf).unwrap(),
        clock: clock.elapsed(),
    }
}

fn deploy(fake: &Fake, dir: &TempDir, bundle: &Path) -> Ran {
    let opts = Options {
        bundle: Some(bundle.to_path_buf()),
        ..Options::default()
    };
    deploy_with(fake, dir, &opts, &migrations::bundled(), &en())
}

fn rollback(fake: &Fake, dir: &TempDir, opts: &RollbackOptions) -> Ran {
    let mut c = client(fake, GOOD);
    let migs = migrations::bundled();
    let text = en();
    let cx = Context {
        dir: &dir.0,
        migrations: &migs,
        now_ms: NOW + 60_000,
        text: &text,
    };
    let clock = FakeClock::default();
    let rt = Runtime {
        clock: &clock,
        smoke_base: Some(fake.base.clone()),
        smoke_budget: deploy::SMOKE_BUDGET,
    };
    let mut buf = Vec::new();
    let result = deploy::rollback(&mut c, None, opts, &cx, &rt, &mut buf);
    Ran {
        result,
        out: String::from_utf8(buf).unwrap(),
        clock: clock.elapsed(),
    }
}

fn ok(r: &Ran) {
    if let Err(e) = &r.result {
        panic!("{:?}: {}\n{}", e.code, e.message, r.out);
    }
}

fn code(r: &Ran) -> Code {
    match &r.result {
        Ok(()) => panic!("expected a failure\n{}", r.out),
        Err(e) => e.code,
    }
}

fn load(dir: &TempDir) -> SiteState {
    state::load(&dir.0).unwrap().unwrap()
}

/// `METHOD /path` of every request after the first `skip`, D1 queries left out.
fn calls(reqs: &[Recorded], skip: usize) -> Vec<String> {
    reqs[skip..]
        .iter()
        .filter(|r| !r.path.ends_with("/query"))
        .map(|r| format!("{} {}", r.method, r.path))
        .collect()
}

fn body_json(r: &Recorded) -> Value {
    serde_json::from_str(&r.body).unwrap()
}

fn metadata(fake: &Fake, id: &str) -> Value {
    fake.state()
        .versions
        .iter()
        .find(|v| v.id == id)
        .unwrap()
        .metadata
        .clone()
}

fn active(fake: &Fake) -> String {
    fake.state().scripts["plumblog"].deployments[0].clone()
}

fn log_rows(fake: &Fake, kind: &str) -> Vec<String> {
    fake.sql(
        "db-1",
        &format!("SELECT detail FROM system_log WHERE kind = '{kind}' ORDER BY id"),
    )
    .iter()
    .map(|r| r["detail"].as_str().unwrap().to_string())
    .collect()
}

/// The version: modules, bindings, date, secrets kept, assets completion JWT and _headers.
fn check_version(fake: &Fake, reqs: &[Recorded], marker: &str) {
    let meta = metadata(fake, V2);
    assert_eq!(meta["main_module"], "index.js");
    assert_eq!(meta["compatibility_date"], deploy::COMPATIBILITY_DATE);
    assert_eq!(
        meta["bindings"],
        json!([
            {"type": "d1", "name": "DB", "id": "db-1"},
            {"type": "r2_bucket", "name": "MEDIA", "bucket_name": "plumblog-media"},
            {"type": "assets", "name": "ASSETS"},
            {"type": "version_metadata", "name": "CF_VERSION_METADATA"},
            {"type": "plain_text", "name": "PLUMBLOG_SITE_ID", "text": marker},
            {"type": "inherit", "name": "WEBHOOK_KEY"},
            {"type": "inherit", "name": "TURNSTILE_SECRET"},
        ])
    );
    assert_eq!(meta["keep_bindings"], json!(["secret_text", "secret_key"]));
    assert!(
        meta["assets"]["jwt"]
            .as_str()
            .unwrap()
            .starts_with("completionJWT")
    );
    assert!(
        meta["assets"]["config"]["_headers"]
            .as_str()
            .unwrap()
            .contains("Cache-Control")
    );
    let version = fake
        .state()
        .versions
        .iter()
        .find(|v| v.id == V2)
        .unwrap()
        .clone();
    assert_eq!(version.modules, ["index.js", "index_bg.wasm"]);
    let vreq = reqs.iter().find(|r| r.path.ends_with("/versions")).unwrap();
    assert!(
        vreq.body
            .contains("Content-Type: application/javascript+module")
    );
    assert!(vreq.body.contains("Content-Type: application/wasm"));
}

#[test]
fn happy_deploy_runs_the_steps_in_order_with_the_documented_shapes() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    let skip = fake.requests().len();
    let r = deploy(&fake, &dir, &bundle);
    ok(&r);
    let reqs = fake.requests();
    let w = WORKERS;
    assert_eq!(
        calls(&reqs, skip),
        [
            "GET /user/tokens/verify".to_string(),
            "GET /accounts".into(),
            "GET /accounts/acc1/d1/database/db-1/time_travel/bookmark".into(),
            format!("POST {w}/assets-upload-session"),
            "POST /accounts/acc1/workers/assets/upload".into(),
            "POST /accounts/acc1/workers/assets/upload".into(),
            "POST /accounts/acc1/workers/assets/upload".into(),
            format!("POST {w}/versions"),
            format!("GET {w}/deployments"),
            format!("POST {w}/deployments"),
            format!("GET {HOST}/healthz"),
            format!("GET {HOST}/"),
            format!("PUT {w}/schedules"),
        ]
    );
    // Manifest: every asset but the root _headers, 32-hex hashes.
    let session = reqs
        .iter()
        .find(|r| r.path.ends_with("/assets-upload-session"))
        .unwrap();
    let manifest = body_json(session)["manifest"].as_object().unwrap().clone();
    let mut paths: Vec<&str> = manifest.keys().map(String::as_str).collect();
    paths.sort_unstable();
    assert_eq!(
        paths,
        [
            "/admin/app.js",
            "/admin/index.html",
            "/plugin/toc/toc.js",
            "/theme/minimal/font.woff2",
            "/theme/minimal/site.css"
        ]
    );
    let marker = load(&dir).script_marker.unwrap();
    assert_eq!(marker.len(), 32);
    check_version(&fake, &reqs, &marker);
    // Deployment at 100% and the cron.
    let dep = reqs
        .iter()
        .rfind(|r| r.method == "POST" && r.path.ends_with("/deployments"))
        .unwrap();
    assert_eq!(
        body_json(dep),
        json!({"strategy": "percentage", "versions": [{"version_id": V2, "percentage": 100}]})
    );
    assert_eq!(active(&fake), V2);
    assert_eq!(
        fake.state().scripts["plumblog"].schedules,
        Some(json!([{"cron": "*/10 * * * *"}]))
    );
    // Smoke target got no token.
    for r in reqs.iter().filter(|r| r.path.starts_with(HOST)) {
        assert_eq!(r.header("authorization"), None);
    }
    // Record and system log.
    let rec = load(&dir).deploys.last().unwrap().clone();
    assert_eq!(rec.outcome, "deployed");
    assert_eq!(rec.version.as_deref(), Some(V2));
    assert_eq!(rec.previous.as_deref(), Some(P));
    assert_eq!(rec.smoke.as_deref(), Some("passed"));
    assert_eq!(rec.at, "2026-09-21T14:13:20Z");
    assert!(rec.bookmark.is_some() && rec.export.is_none() && rec.migrations.is_empty());
    let log = log_rows(&fake, "system.deploy");
    assert_eq!(log.len(), 1);
    assert!(
        log[0].starts_with(&format!("deploy {V2} deployed (previous {P})")),
        "{}",
        log[0]
    );
    assert!(
        r.out.contains(&format!("Deployed version {V2}")),
        "{}",
        r.out
    );
}

#[test]
fn no_pending_migration_takes_a_bookmark_but_no_export() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    let r = deploy(&fake, &dir, &bundle);
    ok(&r);
    let reqs = fake.requests();
    assert!(
        !reqs
            .iter()
            .any(|r| r.path.ends_with("/export") || r.path.starts_with("/__signed"))
    );
    assert!(!dir.0.join("backups").exists());
    assert!(load(&dir).deploys[0].bookmark.is_some());
}

#[test]
fn pending_migrations_export_first_into_a_private_file() {
    let (fake, dir, bundle) = set_up(&all_but_last());
    fake.state().export_polls = 2;
    let skip = fake.requests().len();
    let r = deploy(&fake, &dir, &bundle);
    ok(&r);
    let reqs = fake.requests();
    let c = calls(&reqs, skip);
    let bm = c
        .iter()
        .position(|x| x.ends_with("/time_travel/bookmark"))
        .unwrap();
    let exports: Vec<usize> = (0..c.len())
        .filter(|i| c[*i].ends_with("/export"))
        .collect();
    let signed = c
        .iter()
        .position(|x| x.starts_with("GET /__signed/"))
        .unwrap();
    assert_eq!(exports.len(), 3, "{c:?}");
    assert!(bm < exports[0] && exports[2] < signed, "{c:?}");
    // Re-polls carry the bookmark the first reply gave.
    let posts: Vec<&Recorded> = reqs
        .iter()
        .filter(|r| r.path.ends_with("/export"))
        .collect();
    assert_eq!(body_json(posts[0]), json!({"output_format": "polling"}));
    assert_eq!(body_json(posts[1])["current_bookmark"], "exp-bm");
    let signed_req = reqs
        .iter()
        .find(|r| r.path.starts_with("/__signed/"))
        .unwrap();
    assert_eq!(signed_req.header("authorization"), None);
    // The export happened before the migration ran.
    let last = migrations::bundled().pop().unwrap().name;
    let first_migration = reqs
        .iter()
        .position(|r| r.path.ends_with("/query") && r.body.contains(&last))
        .unwrap();
    let signed_at = reqs
        .iter()
        .position(|r| r.path.starts_with("/__signed/"))
        .unwrap();
    assert!(signed_at < first_migration);
    // The file: backups/<utc>-<db>.sql, 0600, holding the dump.
    let rec = load(&dir).deploys[0].clone();
    assert_eq!(
        rec.export.as_deref(),
        Some("backups/20260921T141320Z-plumblog.sql")
    );
    assert_eq!(rec.migrations, std::slice::from_ref(&last));
    let file = dir.0.join(rec.export.unwrap());
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("-- table posts")
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    assert_eq!(log_rows(&fake, "system.migration"), [last]);
}

#[test]
fn export_flag_exports_without_pending_migrations() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    let opts = Options {
        export: true,
        bundle: Some(bundle),
        ..Options::default()
    };
    let r = deploy_with(&fake, &dir, &opts, &migrations::bundled(), &en());
    ok(&r);
    assert!(load(&dir).deploys[0].export.is_some());
}

#[test]
fn a_migration_error_uploads_nothing_and_blocks_the_next_deploy() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    let mut migs = migrations::bundled();
    migs.push(Migration {
        name: "9999_broken.sql".into(),
        sql: "CREATE TABLE posts (x);".into(),
    });
    let opts = Options {
        bundle: Some(bundle.clone()),
        ..Options::default()
    };
    let skip0 = fake.requests().len();
    let r = deploy_with(&fake, &dir, &opts, &migs, &en());
    assert_eq!(code(&r), Code::MigrationFailed);
    let msg = &r.result.as_ref().unwrap_err().message;
    assert!(msg.contains("blog rollback --db bm-db-1-"), "{msg}");
    // The failure is in the system log too.
    let log = log_rows(&fake, "system.migration");
    assert!(
        log.iter().any(|l| l.starts_with("9999_broken.sql failed")),
        "{log:?}"
    );
    assert!(log_rows(&fake, "system.deploy")[0].contains("migration_failed"));
    for r in &fake.requests()[skip0..] {
        assert!(
            !r.path.contains("assets")
                && !r.path.ends_with("/versions")
                && !r.path.ends_with("/deployments"),
            "{} {}",
            r.method,
            r.path
        );
    }
    let st = load(&dir);
    assert_eq!(
        st.migrations.failed.as_ref().unwrap().name,
        "9999_broken.sql"
    );
    assert_eq!(st.deploys[0].outcome, "migration_failed");
    // The next deploy stops before taking a bookmark or exporting.
    let skip = fake.requests().len();
    let again = deploy_with(&fake, &dir, &opts, &migs, &en());
    assert_eq!(code(&again), Code::MigrationRestorePending);
    assert_eq!(
        calls(&fake.requests(), skip),
        ["GET /user/tokens/verify", "GET /accounts"]
    );
}

#[test]
fn only_missing_buckets_are_uploaded_with_the_session_jwt() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    // Cloudflare already has two of the five files.
    for (path, bytes) in &assets()[..2] {
        fake.state()
            .assets
            .insert(plumblog_cli::bundle::asset_hash(bytes, path));
    }
    let r = deploy(&fake, &dir, &bundle);
    ok(&r);
    let uploads: Vec<Recorded> = fake
        .requests()
        .into_iter()
        .filter(|r| r.path.ends_with("/workers/assets/upload"))
        .collect();
    assert_eq!(uploads.len(), 2, "3 missing files, 2 per bucket");
    let mut sent = 0;
    for u in &uploads {
        assert_eq!(u.header("authorization"), Some("Bearer sessionJWT1"));
        assert!(!u.body.contains(GOOD) && !u.headers.iter().any(|(_, v)| v.contains(GOOD)));
        assert_eq!(u.query, "base64=true");
        for (path, bytes) in &assets()[..2] {
            assert!(
                !u.body
                    .contains(&plumblog_cli::bundle::asset_hash(bytes, path))
            );
        }
        sent += u.body.matches("filename=").count();
    }
    assert_eq!(sent, 3);
    assert!(
        r.out.contains("3 uploaded, 2 already on Cloudflare"),
        "{}",
        r.out
    );
    // A second deploy uploads nothing: the session JWT is the completion token.
    let again = deploy(&fake, &dir, &bundle);
    ok(&again);
    let uploads = fake
        .requests()
        .iter()
        .filter(|r| r.path.ends_with("/assets/upload"))
        .count();
    assert_eq!(uploads, 2);
    let meta = metadata(&fake, "00000000-0000-4000-8000-000000000003");
    assert_eq!(meta["assets"]["jwt"], "sessionJWT2");
}

#[test]
fn secrets_survive_the_new_version() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    let webhook = secret(&fake, "WEBHOOK_KEY").unwrap();
    let turnstile = secret(&fake, "TURNSTILE_SECRET").unwrap();
    let r = deploy(&fake, &dir, &bundle);
    ok(&r);
    assert_eq!(
        secret(&fake, "WEBHOOK_KEY").as_deref(),
        Some(webhook.as_str())
    );
    assert_eq!(
        secret(&fake, "TURNSTILE_SECRET").as_deref(),
        Some(turnstile.as_str())
    );
    // Kept by keep_bindings, never re-sent.
    for r in fake.requests().iter().skip(1) {
        if r.path.ends_with("/versions") {
            assert!(!r.body.contains(&webhook) && !r.body.contains(&turnstile));
        }
    }
}

#[test]
fn a_slow_site_within_the_budget_passes() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    fake.state().site.ready_after = 4;
    let r = deploy(&fake, &dir, &bundle);
    ok(&r);
    // Waited 1 + 2 + 4 + 8 s of virtual time (plus the real milliseconds of the calls).
    assert!(r.clock >= Duration::from_secs(15) && r.clock < Duration::from_secs(16));
    assert_eq!(
        fake.requests()
            .iter()
            .filter(|r| r.path.ends_with("/healthz"))
            .count(),
        5
    );
}

#[test]
fn a_wrong_version_header_rolls_back() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    fake.state().site.version_override = Some("old-version".into());
    let r = deploy(&fake, &dir, &bundle);
    assert_eq!(code(&r), Code::SmokeFailed);
    assert!(r.clock <= deploy::SMOKE_BUDGET, "{:?}", r.clock);
    assert!(r.clock >= Duration::from_secs(30), "{:?}", r.clock);
    assert_eq!(active(&fake), P);
    let msg = &r.result.as_ref().unwrap_err().message;
    assert!(msg.contains("old-version") && msg.contains(P), "{msg}");
    let rec = load(&dir).deploys[0].clone();
    assert_eq!(rec.outcome, "rolled_back");
    assert_eq!(rec.smoke.as_deref(), Some("/healthz version old-version"));
    assert!(log_rows(&fake, "system.deploy")[0].contains("rolled_back"));
}

#[test]
fn a_5xx_home_page_rolls_back() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    fake.state().site.home_status = Some((V2.into(), 503));
    let r = deploy(&fake, &dir, &bundle);
    assert_eq!(code(&r), Code::SmokeFailed);
    assert_eq!(active(&fake), P);
    let rec = load(&dir).deploys[0].clone();
    assert_eq!(rec.outcome, "rolled_back");
    assert_eq!(rec.smoke.as_deref(), Some("/ http-503"));
    // Rolling back to the version that worked never retries the migrations.
    assert!(rec.migrations.is_empty());
}

#[test]
fn rollback_without_a_record_or_to_says_so() {
    let (fake, dir, _) = set_up(&migrations::bundled());
    let r = rollback(&fake, &dir, &RollbackOptions::default());
    assert_eq!(code(&r), Code::RollbackNoRecord);
    assert!(r.result.unwrap_err().message.contains("--to"));
}

#[test]
fn rollback_goes_to_the_previous_version_and_checks_it() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    ok(&deploy(&fake, &dir, &bundle));
    ok(&deploy(&fake, &dir, &bundle));
    assert_eq!(active(&fake), V3);
    let r = rollback(&fake, &dir, &RollbackOptions::default());
    ok(&r);
    assert_eq!(active(&fake), V2);
    let rb = load(&dir).rollbacks[0].clone();
    assert_eq!((rb.kind.as_str(), rb.to.as_str()), ("version", V2));
    assert_eq!(rb.previous.as_deref(), Some(V3));
    assert_eq!(rb.smoke.as_deref(), Some("passed"));
    // --to must be one of the script's versions.
    let bad = rollback(
        &fake,
        &dir,
        &RollbackOptions {
            to: Some("not-a-version".into()),
            ..RollbackOptions::default()
        },
    );
    assert_eq!(code(&bad), Code::RollbackVersionUnknown);
    let to = rollback(
        &fake,
        &dir,
        &RollbackOptions {
            to: Some(V3.into()),
            ..RollbackOptions::default()
        },
    );
    ok(&to);
    assert_eq!(active(&fake), V3);
}

#[test]
fn rollback_db_needs_yes_then_restores_and_clears_the_failure() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    let mut migs = migrations::bundled();
    migs.push(Migration {
        name: "9999_broken.sql".into(),
        sql: "CREATE TABLE posts (x);".into(),
    });
    let opts = Options {
        bundle: Some(bundle.clone()),
        ..Options::default()
    };
    assert_eq!(
        code(&deploy_with(&fake, &dir, &opts, &migs, &en())),
        Code::MigrationFailed
    );
    let bm = load(&dir).migrations.failed.unwrap().bookmark;
    let skip = fake.requests().len();
    let no = rollback(
        &fake,
        &dir,
        &RollbackOptions {
            db: Some(bm.clone()),
            ..RollbackOptions::default()
        },
    );
    assert_eq!(code(&no), Code::RollbackDbUnconfirmed);
    let msg = no.result.unwrap_err().message;
    assert!(
        msg.contains("overwrites the whole database") && msg.contains("cancelled"),
        "{msg}"
    );
    assert_eq!(fake.requests().len(), skip, "nothing sent without --yes");
    let yes = rollback(
        &fake,
        &dir,
        &RollbackOptions {
            db: Some(bm.clone()),
            yes: true,
            ..RollbackOptions::default()
        },
    );
    ok(&yes);
    let restore = fake
        .requests()
        .into_iter()
        .find(|r| r.path.ends_with("/restore"))
        .unwrap();
    assert_eq!(restore.method, "POST");
    assert_eq!(restore.query, format!("bookmark={bm}"));
    assert!(
        yes.out
            .contains("blog rollback --db bm-db-1-before-restore-1 --yes"),
        "{}",
        yes.out
    );
    let st = load(&dir);
    assert!(st.migrations.failed.is_none());
    assert_eq!(st.rollbacks[0].kind, "database");
    assert_eq!(
        st.rollbacks[0].previous.as_deref(),
        Some("bm-db-1-before-restore-1")
    );
    // Deploying works again.
    ok(&deploy(&fake, &dir, &bundle));
}

#[test]
fn a_missing_permission_on_versions_names_it() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    fake.fail_on(&format!("POST {WORKERS}/versions"), Failure::Status(403));
    let r = deploy(&fake, &dir, &bundle);
    assert_eq!(code(&r), Code::PermissionMissing);
    let msg = r.result.unwrap_err().message;
    assert!(msg.contains("Account | Workers Scripts | Edit"), "{msg}");
    assert_eq!(load(&dir).deploys[0].outcome, "failed");
}

#[test]
fn a_dropped_deployment_create_may_have_happened() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    // The switch happened but its reply was lost: the active version says so, and the
    // deploy goes on with the check.
    fake.fail_on(&format!("POST {WORKERS}/deployments"), Failure::DropAfter);
    let r = deploy(&fake, &dir, &bundle);
    ok(&r);
    assert!(r.out.contains("reply to the switch was lost"), "{}", r.out);
    assert_eq!(
        fake.requests()
            .iter()
            .filter(|r| r.path.ends_with("/healthz"))
            .count(),
        1
    );
    assert_eq!(load(&dir).deploys[0].outcome, "deployed");
    // The switch never happened: nothing serves the new version, the run says so.
    fake.fail_on(&format!("POST {WORKERS}/deployments"), Failure::Drop);
    let r = deploy(&fake, &dir, &bundle);
    assert_eq!(code(&r), Code::NetworkWriteUncertain);
    assert_eq!(active(&fake), V2);
    let rec = load(&dir).deploys.last().unwrap().clone();
    assert_eq!(
        (rec.outcome.as_str(), rec.version.as_deref()),
        ("failed", Some(V3))
    );
    // Neither the switch nor the read-back answered: uncertain, and rollback treats it as
    // what serves now.
    fake.fail_on(&format!("POST {WORKERS}/deployments"), Failure::DropAfter);
    let n = fake.requests().len();
    fake.state().blind_after_switch = Some(n);
    let r = deploy(&fake, &dir, &bundle);
    assert_eq!(code(&r), Code::NetworkWriteUncertain);
    let rec = load(&dir).deploys.last().unwrap().clone();
    assert_eq!(rec.outcome, "deploy_uncertain");
    assert_eq!(rec.previous.as_deref(), Some(V2));
    fake.state().blind_after_switch = None;
    ok(&rollback(&fake, &dir, &RollbackOptions::default()));
    assert_eq!(active(&fake), V2);
}

#[test]
fn a_cron_failure_keeps_the_deploy_and_rollback_still_finds_its_target() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    ok(&deploy(&fake, &dir, &bundle));
    fake.fail_on(&format!("PUT {WORKERS}/schedules"), Failure::Status(500));
    let r = deploy(&fake, &dir, &bundle);
    assert_eq!(code(&r), Code::CronFailed);
    assert!(r.result.unwrap_err().message.contains("blog deploy"));
    assert_eq!(active(&fake), V3);
    let rec = load(&dir).deploys.last().unwrap().clone();
    assert_eq!(rec.outcome, "deployed_cron_failed");
    assert_eq!(rec.smoke.as_deref(), Some("passed"));
    ok(&rollback(&fake, &dir, &RollbackOptions::default()));
    assert_eq!(active(&fake), V2);
}

#[test]
fn a_malformed_deployments_list_stops_before_switching() {
    for bad in [
        json!({}),
        json!({"deployments": []}),
        json!({"deployments": [{"versions": [{"percentage": 100}]}]}),
    ] {
        let (fake, dir, bundle) = set_up(&migrations::bundled());
        fake.state().deployments_override = Some(bad.clone());
        let r = deploy(&fake, &dir, &bundle);
        assert_eq!(code(&r), Code::ReplyMalformed, "{bad}");
        assert!(
            !fake
                .requests()
                .iter()
                .any(|r| r.method == "POST" && r.path.ends_with("/deployments")),
            "{bad}"
        );
        assert_eq!(load(&dir).deploys[0].outcome, "failed");
    }
}

#[test]
fn a_missing_secret_fails_the_version_upload() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    fake.state()
        .scripts
        .get_mut("plumblog")
        .unwrap()
        .secrets
        .remove("WEBHOOK_KEY");
    let r = deploy(&fake, &dir, &bundle);
    assert_eq!(code(&r), Code::HttpFailed);
    assert!(r.result.unwrap_err().message.contains("WEBHOOK_KEY"));
    assert_eq!(active(&fake), P);
    let v = fake
        .requests()
        .into_iter()
        .find(|r| r.path.ends_with("/versions"))
        .unwrap();
    assert_eq!(v.query, "bindings_inherit=strict");
}

#[test]
fn slow_calls_count_against_the_smoke_budget() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    fake.state().site.delay = Duration::from_secs(3);
    let opts = Options {
        bundle: Some(bundle),
        ..Options::default()
    };
    let started = std::time::Instant::now();
    let r = deploy_budget(
        &fake,
        &dir,
        &opts,
        &migrations::bundled(),
        &en(),
        Duration::from_secs(1),
    );
    assert_eq!(code(&r), Code::SmokeFailed);
    // One call, cut at the 1 s left, and no retry after the budget is spent.
    assert!(
        started.elapsed() < Duration::from_millis(2500),
        "{:?}",
        started.elapsed()
    );
    assert!(r.clock < Duration::from_millis(2500), "{:?}", r.clock);
    assert_eq!(active(&fake), P, "rolled back");
}

#[test]
fn rollback_never_lands_on_the_placeholder_and_undoes_a_bad_target() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    assert_eq!(load(&dir).placeholder_version.as_deref(), Some(V1));
    ok(&deploy(&fake, &dir, &bundle));
    // The last deploy's previous is the placeholder: refused, nothing sent.
    let skip = fake.requests().len();
    let r = rollback(&fake, &dir, &RollbackOptions::default());
    assert_eq!(code(&r), Code::RollbackPlaceholder);
    assert!(
        calls(&fake.requests(), skip)
            .iter()
            .all(|c| c.starts_with("GET"))
    );
    let to = RollbackOptions {
        to: Some(V1.into()),
        ..RollbackOptions::default()
    };
    assert_eq!(code(&rollback(&fake, &dir, &to)), Code::RollbackPlaceholder);
    // A target that fails its check: the version from before comes back.
    ok(&deploy(&fake, &dir, &bundle));
    fake.state().site.home_status = Some((V2.into(), 503));
    let r = rollback(&fake, &dir, &RollbackOptions::default());
    assert_eq!(code(&r), Code::SmokeFailed);
    let msg = r.result.unwrap_err().message;
    assert!(msg.contains(V2) && msg.contains(V3), "{msg}");
    assert_eq!(active(&fake), V3);
}

#[test]
fn bundles_bring_their_migrations_and_older_ones_are_refused() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    // No migrations/ at all.
    std::fs::remove_dir_all(bundle.join("migrations")).unwrap();
    let opts = Options {
        bundle: Some(bundle.clone()),
        ..Options::default()
    };
    let mut c = client(&fake, GOOD);
    let migs = migrations::bundled();
    let text = en();
    let cx = Context {
        dir: &dir.0,
        migrations: &migs,
        now_ms: NOW,
        text: &text,
    };
    let clock = FakeClock::default();
    let rt = Runtime {
        clock: &clock,
        smoke_base: Some(fake.base.clone()),
        smoke_budget: deploy::SMOKE_BUDGET,
    };
    let mut out = Vec::new();
    let e = deploy::run(&mut c, None, &opts, &cx, &rt, &mut out).unwrap_err();
    assert_eq!(e.code, Code::BundleInvalid);
    assert!(e.message.contains("migrations"), "{}", e.message);
    // A bundle older than the database.
    let skip = fake.requests().len();
    let r = deploy_with(&fake, &dir, &opts, &all_but_last(), &en());
    assert_eq!(code(&r), Code::BundleOlder);
    assert!(r.result.unwrap_err().message.contains("--allow-older"));
    assert!(
        calls(&fake.requests(), skip)
            .iter()
            .all(|c| c.starts_with("GET"))
    );
    let allow = Options {
        allow_older: true,
        ..opts.clone()
    };
    ok(&deploy_with(&fake, &dir, &allow, &all_but_last(), &en()));
}

#[test]
fn bundle_files_hash_with_their_type_and_symlinks_are_refused() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    std::fs::write(bundle.join("assets/a.css"), b"").unwrap();
    std::fs::write(bundle.join("assets/b.js"), b"").unwrap();
    ok(&deploy(&fake, &dir, &bundle));
    let session = fake
        .requests()
        .into_iter()
        .find(|r| r.path.ends_with("/assets-upload-session"))
        .unwrap();
    let m = body_json(&session)["manifest"].clone();
    assert_ne!(m["/a.css"]["hash"], m["/b.js"]["hash"]);
    assert!(
        fake.state()
            .assets
            .contains(m["/a.css"]["hash"].as_str().unwrap())
    );
    assert!(
        fake.state()
            .assets
            .contains(m["/b.js"]["hash"].as_str().unwrap())
    );
    #[cfg(unix)]
    {
        let w = bundle.join("worker");
        std::fs::rename(w.join("index_bg.wasm"), dir.0.join("real.wasm")).unwrap();
        std::os::unix::fs::symlink(dir.0.join("real.wasm"), w.join("index_bg.wasm")).unwrap();
        let skip = fake.requests().len();
        assert_eq!(code(&deploy(&fake, &dir, &bundle)), Code::BundleSymlink);
        assert!(
            calls(&fake.requests(), skip)
                .iter()
                .all(|c| c.starts_with("GET"))
        );
    }
}

#[test]
fn needs_setup_and_a_valid_bundle() {
    let fake = Fake::start(site(ALL));
    let dir = TempDir::new();
    let bundle = make_bundle(&dir);
    assert_eq!(code(&deploy(&fake, &dir, &bundle)), Code::NotSetUp);
    assert!(fake.requests().is_empty());

    let (fake, dir, bundle) = set_up(&migrations::bundled());
    std::fs::remove_file(bundle.join("worker/index_bg.wasm")).unwrap();
    let skip = fake.requests().len();
    assert_eq!(code(&deploy(&fake, &dir, &bundle)), Code::BundleInvalid);
    assert!(fake.requests()[skip..].iter().all(|r| r.method == "GET"));
}

#[test]
fn no_token_or_secret_in_output_state_or_backups() {
    let (fake, dir, bundle) = set_up(&all_but_last());
    fake.state().site.version_override = Some("x".into());
    let r = deploy(&fake, &dir, &bundle);
    assert_eq!(code(&r), Code::SmokeFailed);
    let secrets = [
        GOOD.to_string(),
        secret(&fake, "WEBHOOK_KEY").unwrap(),
        secret(&fake, "TURNSTILE_SECRET").unwrap(),
        "sessionJWT".into(),
        "completionJWT".into(),
        "s1g".into(),
    ];
    let mut texts = vec![r.out.clone(), r.result.unwrap_err().message];
    texts.push(std::fs::read_to_string(state::path(&dir.0)).unwrap());
    for e in std::fs::read_dir(dir.0.join("backups")).unwrap() {
        texts.push(e.unwrap().file_name().to_string_lossy().into_owned());
    }
    for t in &texts {
        for s in &secrets {
            assert!(!t.contains(s.as_str()), "{s} in {t}");
        }
    }
}

/// Every Vietnamese line comes from a key (no raw `cli.` key, no English fallback).
#[test]
fn vietnamese_output() {
    let (fake, dir, bundle) = set_up(&all_but_last());
    let opts = Options {
        bundle: Some(bundle),
        ..Options::default()
    };
    let r = deploy_with(&fake, &dir, &opts, &migrations::bundled(), &Text::new("vi"));
    ok(&r);
    assert!(!r.out.contains("cli."), "{}", r.out);
    let vi_done = Text::new("vi").t(
        plumblog_i18n::key!("cli.deploy.done"),
        &[("version", V2.into()), ("origin", String::new())],
    );
    assert!(r.out.contains(vi_done.trim_end_matches('.')), "{}", r.out);
    assert!(!r.out.contains("Deployed version"), "{}", r.out);
    eprintln!("{}", r.out);
}

#[test]
fn the_binary_refuses_a_db_restore_without_yes_before_any_call() {
    let dir = TempDir::new();
    dir.write_env(GOOD);
    let out = Command::new(env!("CARGO_BIN_EXE_blog"))
        .args(["--lang", "en", "rollback", "--db", "bm-1"])
        .current_dir(&dir.0)
        .env("PLUMBLOG_CF_API_BASE", "http://127.0.0.1:9")
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("[rollback.db_unconfirmed]") && err.contains("--yes"),
        "{err}"
    );
    let out = Command::new(env!("CARGO_BIN_EXE_blog"))
        .args(["--lang", "en", "deploy"])
        .current_dir(&dir.0)
        .env("PLUMBLOG_CF_API_BASE", "http://127.0.0.1:9")
        .output()
        .unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("[deploy.not_set_up]") && err.contains("blog setup"),
        "{err}"
    );
}

#[test]
fn a_second_run_in_the_same_folder_waits_its_turn() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    std::fs::write(dir.0.join(deploy::LOCK_FILE), "123\n").unwrap();
    let skip = fake.requests().len();
    let r = deploy(&fake, &dir, &bundle);
    assert_eq!(code(&r), Code::DeployLocked);
    assert_eq!(fake.requests().len(), skip, "nothing sent while locked");
    assert_eq!(
        code(&rollback(&fake, &dir, &RollbackOptions::default())),
        Code::DeployLocked
    );
    std::fs::remove_file(dir.0.join(deploy::LOCK_FILE)).unwrap();
    ok(&deploy(&fake, &dir, &bundle));
    assert!(
        !dir.0.join(deploy::LOCK_FILE).exists(),
        "the lock goes with the run"
    );
}

#[test]
fn restoring_another_bookmark_keeps_the_failed_marker_unless_told() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    let mut migs = migrations::bundled();
    migs.push(Migration {
        name: "9999_broken.sql".into(),
        sql: "CREATE TABLE posts (x);".into(),
    });
    let opts = Options {
        bundle: Some(bundle),
        ..Options::default()
    };
    assert_eq!(
        code(&deploy_with(&fake, &dir, &opts, &migs, &en())),
        Code::MigrationFailed
    );
    let st = load(&dir);
    let failed_bm = st.migrations.failed.clone().unwrap().bookmark;
    let other = st.deploys[0].bookmark.clone().unwrap();
    assert_ne!(other, failed_bm);
    let db = |clear| RollbackOptions {
        db: Some(other.clone()),
        yes: true,
        clear_failed_migration: clear,
        ..RollbackOptions::default()
    };
    let r = rollback(&fake, &dir, &db(false));
    ok(&r);
    assert!(load(&dir).migrations.failed.is_some());
    assert!(
        r.out.contains(&failed_bm) && r.out.contains("--clear-failed-migration"),
        "{}",
        r.out
    );
    // Restored and undo come first: they stay true whatever happens after.
    let restored = r.out.find("Database restored").unwrap();
    let undo = r.out.find("To undo this restore").unwrap();
    let kept = r.out.find("stays recorded").unwrap();
    assert!(restored < undo && undo < kept, "{}", r.out);
    ok(&rollback(&fake, &dir, &db(true)));
    assert!(load(&dir).migrations.failed.is_none());
}

#[test]
fn a_lost_placeholder_reply_still_keeps_rollback_off_the_placeholder() {
    let fake = Fake::start(site(ALL));
    let dir = TempDir::new();
    fake.fail_on(
        "PUT /accounts/acc1/workers/scripts/plumblog",
        Failure::DropAfter,
    );
    let first = setup_with(
        &fake,
        &dir,
        &SetupOptions::default(),
        &migrations::bundled(),
        &en(),
    );
    assert!(first.result.is_err());
    assert_eq!(load(&dir).placeholder_version, None, "the reply never came");
    // The re-run adopts the script (its marker was saved before the upload) and records
    // what serves now, never deployed by blog, as the placeholder.
    setup_ok(&setup_with(
        &fake,
        &dir,
        &SetupOptions::default(),
        &migrations::bundled(),
        &en(),
    ));
    assert_eq!(load(&dir).placeholder_version.as_deref(), Some(V1));
    assert_eq!(fake.state().scripts["plumblog"].uploads, 1);
    fake.state().site.script = "plumblog".into();
    let bundle = make_bundle(&dir);
    ok(&deploy(&fake, &dir, &bundle));
    let skip = fake.requests().len();
    assert_eq!(
        code(&rollback(&fake, &dir, &RollbackOptions::default())),
        Code::RollbackPlaceholder
    );
    assert!(
        calls(&fake.requests(), skip)
            .iter()
            .all(|c| c.starts_with("GET"))
    );
    assert_eq!(active(&fake), V2);
}

#[test]
fn an_unrecorded_migration_in_a_deploy_blocks_the_next_one() {
    let (fake, dir, bundle) = set_up(&all_but_last());
    let last = migrations::bundled().pop().unwrap().name;
    fake.fail_body(&format!("\"params\":[\"{last}\"]"), Failure::Status(400));
    let r = deploy(&fake, &dir, &bundle);
    assert_eq!(code(&r), Code::MigrationUnrecorded);
    let st = load(&dir);
    let failed = st.migrations.failed.clone().unwrap();
    assert_eq!(failed.name, last);
    assert_eq!(Some(failed.bookmark), st.migrations.last_bookmark);
    assert_eq!(st.deploys.last().unwrap().outcome, "migration_failed");
    assert!(
        calls(&fake.requests(), 0)
            .iter()
            .all(|c| !c.ends_with("/versions")),
        "no code uploaded"
    );
    let skip = fake.requests().len();
    assert_eq!(
        code(&deploy(&fake, &dir, &bundle)),
        Code::MigrationRestorePending
    );
    assert_eq!(fake.requests().len(), skip + 2, "token verify only");
}

/// A site set up with `--domain blog.example.com`; `workers_dev`: the account has a
/// workers.dev subdomain.
fn set_up_domain(workers_dev: bool) -> (Fake, TempDir, PathBuf) {
    let mut st = site(ALL);
    if !workers_dev {
        st.subdomain = None;
    }
    let fake = Fake::start(st);
    let dir = TempDir::new();
    let opts = SetupOptions {
        domain: Some("blog.example.com".into()),
        ..SetupOptions::default()
    };
    setup_ok(&setup_with(
        &fake,
        &dir,
        &opts,
        &migrations::bundled(),
        &en(),
    ));
    fake.state().site.script = "plumblog".into();
    let bundle = make_bundle(&dir);
    (fake, dir, bundle)
}

fn smoke_hosts(fake: &Fake, from: usize) -> Vec<String> {
    fake.state().site.hosts[from..].to_vec()
}

#[test]
fn a_new_custom_domain_is_checked_on_workers_dev_and_only_warned_about() {
    let (fake, dir, bundle) = set_up_domain(true);
    fake.state().site.down_hosts = vec!["blog.example.com".into()];
    let r = deploy(&fake, &dir, &bundle);
    ok(&r);
    assert_eq!(
        active(&fake),
        V2,
        "no rollback for a certificate still on its way"
    );
    assert!(
        r.out
            .contains("Warning: https://blog.example.com does not serve the new version yet"),
        "{}",
        r.out
    );
    assert_eq!(
        smoke_hosts(&fake, 0),
        [
            "plumblog.steven.workers.dev",
            "plumblog.steven.workers.dev",
            "blog.example.com"
        ]
    );
    assert!(!load(&dir).domain_smoke_passed);
    // The certificate arrived: workers.dev decides once more, and the domain passes.
    fake.state().site.down_hosts.clear();
    let n = fake.state().site.hosts.len();
    let r = deploy(&fake, &dir, &bundle);
    ok(&r);
    assert!(
        r.out
            .contains("https://blog.example.com serves the new version too"),
        "{}",
        r.out
    );
    assert_eq!(smoke_hosts(&fake, n)[0], "plumblog.steven.workers.dev");
    assert!(load(&dir).domain_smoke_passed);
    // From now on the domain is the target, and its failure rolls back.
    let n = fake.state().site.hosts.len();
    ok(&deploy(&fake, &dir, &bundle));
    assert!(
        smoke_hosts(&fake, n)
            .iter()
            .all(|h| h == "blog.example.com")
    );
    fake.state().site.down_hosts = vec!["blog.example.com".into()];
    let r = deploy(&fake, &dir, &bundle);
    assert_eq!(code(&r), Code::SmokeFailed);
}

#[test]
fn without_workers_dev_a_new_domain_decides_with_a_warning() {
    let (fake, dir, bundle) = set_up_domain(false);
    let r = deploy(&fake, &dir, &bundle);
    ok(&r);
    assert!(r.out.contains("certificate may need a moment"), "{}", r.out);
    assert!(
        smoke_hosts(&fake, 0)
            .iter()
            .all(|h| h == "blog.example.com")
    );
    assert!(load(&dir).domain_smoke_passed);
    // A domain that never answers: the 60 s bound, then the previous version again.
    let (fake, dir, bundle) = set_up_domain(false);
    fake.state().site.down_hosts = vec!["blog.example.com".into()];
    let r = deploy(&fake, &dir, &bundle);
    assert_eq!(code(&r), Code::SmokeFailed);
    assert!(r.out.contains("certificate may need a moment"), "{}", r.out);
    assert!(
        r.clock < deploy::SMOKE_BUDGET + Duration::from_secs(5),
        "{:?}",
        r.clock
    );
    assert_eq!(active(&fake), P);
    assert!(!load(&dir).domain_smoke_passed);
}

#[test]
fn rollback_to_a_version_older_than_the_database_needs_allow_older() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    ok(&deploy(&fake, &dir, &bundle));
    assert_eq!(
        load(&dir).deploys[0].bundle_migrations,
        Some(migrations::bundled().into_iter().map(|m| m.name).collect())
    );
    let mut migs = migrations::bundled();
    migs.push(Migration {
        name: "0099_extra.sql".into(),
        sql: "CREATE TABLE extra (x INTEGER);".into(),
    });
    let opts = Options {
        bundle: Some(bundle.clone()),
        ..Options::default()
    };
    ok(&deploy_with(&fake, &dir, &opts, &migs, &en()));
    assert_eq!(active(&fake), V3);
    let to = |v: &str, allow| RollbackOptions {
        to: Some(v.into()),
        allow_older: allow,
        ..RollbackOptions::default()
    };
    let skip = fake.requests().len();
    for r in [
        rollback(&fake, &dir, &to(V2, false)),
        rollback(&fake, &dir, &RollbackOptions::default()),
    ] {
        assert_eq!(code(&r), Code::RollbackOlder);
        let msg = r.result.unwrap_err().message;
        assert!(
            msg.contains("0099_extra.sql") && msg.contains("--allow-older"),
            "{msg}"
        );
    }
    assert!(
        calls(&fake.requests(), skip)
            .iter()
            .all(|c| c.starts_with("GET"))
    );
    assert_eq!(active(&fake), V3);
    let r = rollback(&fake, &dir, &to(V2, true));
    ok(&r);
    assert!(
        r.out.contains("Warning") && r.out.contains("0099_extra.sql"),
        "{}",
        r.out
    );
    assert_eq!(active(&fake), V2);

    // A version blog did not deploy: its migrations are unknown.
    {
        let mut st = fake.state();
        st.versions.push(crate::fake_cf::Version {
            id: "by-hand".into(),
            metadata: json!({"bindings": [{"type": "d1", "name": "DB", "id": "db-1"}]}),
            modules: vec![],
        });
        st.scripts
            .get_mut("plumblog")
            .unwrap()
            .versions
            .push("by-hand".into());
    }
    let r = rollback(&fake, &dir, &to("by-hand", false));
    assert_eq!(code(&r), Code::RollbackOlder);
    assert!(r.result.unwrap_err().message.contains("no record"));
    assert_eq!(active(&fake), V2);
    ok(&rollback(&fake, &dir, &to("by-hand", true)));
    assert_eq!(active(&fake), "by-hand");
}

#[test]
fn versions_made_by_secret_changes_are_placeholders_too() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    // Setup's upload, then one version per secret, the last one serving.
    assert_eq!(
        fake.state().scripts["plumblog"].versions,
        [V1, "00000000-0000-4000-9000-000000000001", P]
    );
    assert_eq!(active(&fake), P);
    assert_eq!(load(&dir).placeholder_version.as_deref(), Some(V1));
    ok(&deploy(&fake, &dir, &bundle));
    assert_eq!(load(&dir).deploys[0].previous.as_deref(), Some(P));
    // The previous version is the placeholder's code with a secret: refused, nothing sent.
    let skip = fake.requests().len();
    let r = rollback(&fake, &dir, &RollbackOptions::default());
    assert_eq!(code(&r), Code::RollbackPlaceholder);
    assert!(
        calls(&fake.requests(), skip)
            .iter()
            .all(|c| c.starts_with("GET"))
    );
    for v in [V1, "00000000-0000-4000-9000-000000000001", P] {
        let to = RollbackOptions {
            to: Some(v.into()),
            ..RollbackOptions::default()
        };
        assert_eq!(
            code(&rollback(&fake, &dir, &to)),
            Code::RollbackPlaceholder,
            "{v}"
        );
    }
    assert_eq!(active(&fake), V2);
    // After a second deploy, rollback goes to the first real version.
    ok(&deploy(&fake, &dir, &bundle));
    assert_eq!(active(&fake), V3);
    ok(&rollback(&fake, &dir, &RollbackOptions::default()));
    assert_eq!(active(&fake), V2);
}
