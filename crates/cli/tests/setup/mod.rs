//! `blog setup` against the fake API, with the real migrations run in SQLite.

use super::{GOOD, TempDir, client, en};
use crate::fake_cf::{Failure, Fake, Recorded, State, Token};
use plumblog_cli::migrations::{self, Migration};
use plumblog_cli::setup::{self, Context, Failure as SetupFailure, Options, Turnstile, code_hash};
use plumblog_cli::state;
use plumblog_cli::text::{Code, Text};
use plumblog_i18n::key;
use plumblog_store::setup_hash_json;
use serde_json::{Value, json};
use std::process::Command;

pub(crate) const ALL: &[&str] = &[
    "Account | D1 | Edit",
    "Account | Workers Scripts | Edit",
    "Account | Workers R2 Storage | Edit",
    "Account | Turnstile | Edit",
    "Zone | Zone | Read",
];
const NOW: i64 = 1_790_000_000_000;
const ORIGIN: &str = "https://plumblog.steven.workers.dev";

pub(crate) fn site(perms: &[&'static str]) -> State {
    let mut st = State::default();
    st.accounts.push(("acc1".into(), "Steven's blog".into()));
    st.tokens.insert(GOOD.into(), Token::user(perms));
    st.subdomain = Some("steven".into());
    st.zones
        .push(json!({"id": "zone-1", "name": "example.com", "account": {"id": "acc1"}}));
    st
}

pub(crate) struct Ran {
    pub(crate) result: Result<(), SetupFailure>,
    pub(crate) out: String,
}

pub(crate) fn run_with(
    fake: &Fake,
    dir: &TempDir,
    opts: &Options,
    migs: &[Migration],
    text: &Text,
) -> Ran {
    let mut c = client(fake, GOOD);
    let cx = Context {
        dir: &dir.0,
        migrations: migs,
        now_ms: NOW,
        text,
    };
    let mut buf = Vec::new();
    let result = setup::run(&mut c, None, opts, &cx, &mut buf);
    Ran {
        result,
        out: String::from_utf8(buf).unwrap(),
    }
}

fn run(fake: &Fake, dir: &TempDir, opts: &Options) -> Ran {
    run_with(fake, dir, opts, &migrations::bundled(), &en())
}

pub(crate) fn ok(r: &Ran) {
    if let Err(e) = &r.result {
        panic!("{:?}: {}\n{}", e.code, e.message, r.out);
    }
}

/// The setup code printed in `out` (English).
fn code_in(out: &str) -> Option<String> {
    let prefix = en().t(key!("cli.setup.code"), &[("code", String::new())]);
    out.lines()
        .find_map(|l| l.strip_prefix(prefix.as_str()))
        .map(str::to_string)
}

fn writes(reqs: &[Recorded]) -> Vec<String> {
    reqs.iter()
        .filter(|r| r.method != "GET")
        .map(|r| format!("{} {}", r.method, r.path))
        .collect()
}

/// Requests that create or change something: every non-GET except D1 reads.
fn changes(reqs: &[Recorded]) -> Vec<String> {
    reqs.iter()
        .filter(|r| r.method != "GET")
        .filter(|r| {
            !r.path.ends_with("/query") || {
                let b = r.body.to_ascii_uppercase();
                ["INSERT", "UPDATE", "DELETE", "CREATE"]
                    .iter()
                    .any(|w| b.contains(w))
            }
        })
        .map(|r| format!("{} {} {}", r.method, r.path, r.body))
        .collect()
}

fn setting(fake: &Fake, key: &str) -> Option<String> {
    fake.sql(
        "db-1",
        &format!("SELECT value_json FROM settings WHERE key = '{key}'"),
    )
    .first()
    .and_then(|r| r["value_json"].as_str().map(str::to_string))
}

pub(crate) fn secret(fake: &Fake, name: &str) -> Option<String> {
    fake.state()
        .scripts
        .get("plumblog")?
        .secrets
        .get(name)
        .cloned()
}

fn add_owner(fake: &Fake) {
    fake.exec(
        "db-1",
        "INSERT INTO users (login, slug, display_name, role, password_hash, created_at, \
         updated_at) VALUES ('steven', 'steven', 'Steven', 'owner', 'x', 1, 1)",
    );
}

/// Nothing in `dir` but `.env` holds any of `values`.
fn assert_not_in_files(dir: &TempDir, values: &[&str]) {
    for e in std::fs::read_dir(&dir.0).unwrap().flatten() {
        if e.file_name() == ".env" {
            continue;
        }
        let text = std::fs::read_to_string(e.path()).unwrap_or_default();
        for v in values {
            assert!(
                !v.is_empty() && !text.contains(v),
                "{v} in {}",
                e.path().display()
            );
        }
    }
}

fn fresh() -> (Fake, TempDir, Ran) {
    let fake = Fake::start(site(ALL));
    let dir = TempDir::new();
    let r = run(&fake, &dir, &Options::default());
    ok(&r);
    (fake, dir, r)
}

#[test]
fn fresh_setup_creates_everything_with_the_expected_requests() {
    let (fake, _dir, r) = fresh();
    let reqs = fake.requests();
    let w: Vec<String> = writes(&reqs)
        .into_iter()
        .filter(|w| !w.ends_with("/query"))
        .collect();
    assert_eq!(
        w,
        [
            "POST /accounts/acc1/d1/database",
            "POST /accounts/acc1/r2/buckets",
            "PUT /accounts/acc1/workers/scripts/plumblog",
            "POST /accounts/acc1/workers/scripts/plumblog/subdomain",
            "PUT /accounts/acc1/workers/scripts/plumblog/secrets",
            "POST /accounts/acc1/challenges/widgets",
            "PUT /accounts/acc1/workers/scripts/plumblog/secrets",
        ]
    );
    let body = |path: &str| -> Value {
        let r = reqs
            .iter()
            .find(|r| r.path == path && r.method != "GET")
            .unwrap();
        serde_json::from_str(&r.body).unwrap()
    };
    assert_eq!(
        body("/accounts/acc1/d1/database"),
        json!({"name": "plumblog"})
    );
    assert_eq!(
        body("/accounts/acc1/r2/buckets"),
        json!({"name": "plumblog-media"})
    );
    assert_eq!(
        body("/accounts/acc1/challenges/widgets"),
        json!({"name": "plumblog-plumblog", "domains": ["plumblog.steven.workers.dev"],
               "mode": "managed"})
    );
    assert_eq!(
        body("/accounts/acc1/workers/scripts/plumblog/subdomain"),
        json!({"enabled": true})
    );

    // Migrations: wrangler's table, one row per file, in order; a bookmark was taken first.
    let names: Vec<String> = migrations::bundled().into_iter().map(|m| m.name).collect();
    let rows: Vec<String> = fake
        .sql("db-1", "SELECT name FROM d1_migrations ORDER BY id")
        .iter()
        .map(|r| r["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(rows, names);
    let cols: Vec<String> = fake
        .sql(
            "db-1",
            "SELECT name FROM pragma_table_info('d1_migrations') ORDER BY cid",
        )
        .iter()
        .map(|r| r["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(cols, ["id", "name", "applied_at"]);
    let bm_at = reqs
        .iter()
        .position(|r| r.path.ends_with("/bookmark"))
        .unwrap();
    let first_migration = reqs
        .iter()
        .position(|r| r.body.contains("CREATE TABLE site"))
        .unwrap();
    assert!(bm_at < first_migration);

    for line in [
        "D1 database plumblog: created",
        "R2 bucket plumblog-media: created",
        "Worker plumblog: created",
        "Worker secret WEBHOOK_KEY: set",
        "Turnstile widget plumblog-plumblog: created",
        "blog deploy",
    ] {
        assert!(r.out.contains(line), "{line}\n{}", r.out);
    }
}

#[test]
fn fresh_setup_writes_what_the_worker_reads_and_keeps_secrets_out_of_files() {
    let (fake, dir, r) = fresh();
    let names: Vec<String> = migrations::bundled().into_iter().map(|m| m.name).collect();
    // Settings exactly as the Worker reads them.
    assert_eq!(setting(&fake, "origin").unwrap(), format!("\"{ORIGIN}\""));
    let sitekey = fake.state().widgets[0].sitekey.clone();
    assert_eq!(
        setting(&fake, "turnstile_site_key").unwrap(),
        format!("\"{sitekey}\"")
    );
    let site_id = setting(&fake, "site_id").unwrap();
    let inner: String = serde_json::from_str(&site_id).unwrap();
    assert_eq!(inner.len(), 22, "{site_id}");
    assert!(
        inner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    );
    let code = code_in(&r.out).expect("a setup code");
    assert_eq!(
        setting(&fake, "setup_code_hash").unwrap(),
        setup_hash_json(&code_hash(&code))
    );
    assert_eq!(
        setting(&fake, "setup_code_expires_at").unwrap(),
        (NOW + 24 * 3600 * 1000).to_string()
    );
    // put_setting bumps the content version: origin and Turnstile key.
    let v = fake.sql("db-1", "SELECT content_version FROM site WHERE id = 1");
    assert_eq!(v[0]["content_version"], 3);
    // The Worker's create_first_owner guard accepts this code now, and not after expiry.
    let guard = |now: i64| {
        fake.sql(
            "db-1",
            &format!(
                "SELECT (NOT EXISTS (SELECT 1 FROM users) \
                 AND EXISTS (SELECT 1 FROM settings WHERE key = 'setup_code_hash' AND value_json = '{}') \
                 AND EXISTS (SELECT 1 FROM settings WHERE key = 'setup_code_expires_at' \
                   AND CAST(CAST(value_json AS INTEGER) AS TEXT) = value_json \
                   AND CAST(value_json AS INTEGER) > {now})) AS ok",
                setup_hash_json(&code_hash(&code))
            ),
        )[0]["ok"]
            .clone()
    };
    assert_eq!(guard(NOW + 1000), 1);
    assert_eq!(guard(NOW + 24 * 3600 * 1000), 0);

    // Secrets: WEBHOOK_KEY parses as the Worker reads it; TURNSTILE_SECRET is the widget's.
    let key = secret(&fake, "WEBHOOK_KEY").unwrap();
    assert!(
        plumblog_webhook::SealKey::parse(Some(&key)).is_ok(),
        "{key}"
    );
    let tsecret = fake.state().widgets[0].secret.clone();
    assert_eq!(secret(&fake, "TURNSTILE_SECRET").unwrap(), tsecret);
    assert!(fake.state().scripts["plumblog"].workers_dev);

    // The state file: names and ids, never a secret, the token or the code; mode 0600.
    let st = state::load(&dir.0).unwrap().unwrap();
    assert_eq!(st.account_id.as_deref(), Some("acc1"));
    assert_eq!(st.d1.unwrap().id, "db-1");
    assert_eq!(st.r2.unwrap().bucket, "plumblog-media");
    assert_eq!(st.origin.as_deref(), Some(ORIGIN));
    assert_eq!(st.turnstile_site_key.as_deref(), Some(sitekey.as_str()));
    assert_eq!(st.migrations.applied, names);
    assert_eq!(st.migrations.last_bookmark.as_deref(), Some("bm-db-1-1"));
    assert_not_in_files(&dir, &[GOOD, &key, &tsecret, &code]);
    for v in [&key, &tsecret] {
        assert!(!r.out.contains(v.as_str()), "secret printed: {}", r.out);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(state::path(&dir.0))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn rerun_is_a_noop_and_users_get_no_code() {
    let fake = Fake::start(site(ALL));
    let dir = TempDir::new();
    ok(&run(&fake, &dir, &Options::default()));
    add_owner(&fake);
    let key = secret(&fake, "WEBHOOK_KEY");
    let hash = setting(&fake, "setup_code_hash");
    let state_before = std::fs::read_to_string(state::path(&dir.0)).unwrap();
    let n = fake.requests().len();

    let r = run(&fake, &dir, &Options::default());
    ok(&r);
    let again = &fake.requests()[n..];
    assert_eq!(changes(again), Vec::<String>::new());
    assert_eq!(secret(&fake, "WEBHOOK_KEY"), key);
    assert_eq!(setting(&fake, "setup_code_hash"), hash);
    assert!(code_in(&r.out).is_none(), "{}", r.out);
    assert!(
        r.out
            .contains("The site already has an owner, so no setup code was created."),
        "{}",
        r.out
    );
    for line in [
        "D1 database plumblog: already there",
        "Worker plumblog: already there",
        "Worker secret WEBHOOK_KEY: already set",
        "Worker secret TURNSTILE_SECRET: already set",
        "Site settings: unchanged",
        "up to date",
    ] {
        assert!(r.out.contains(line), "{line}\n{}", r.out);
    }
    assert_eq!(
        std::fs::read_to_string(state::path(&dir.0)).unwrap(),
        state_before
    );
    let st = fake.state();
    assert_eq!((st.d1.len(), st.r2.len(), st.widgets.len()), (1, 1, 1));
    assert_eq!(st.scripts["plumblog"].uploads, 1);
}

#[test]
fn a_rerun_without_users_replaces_the_code() {
    let fake = Fake::start(site(ALL));
    let dir = TempDir::new();
    let first = code_in(&run(&fake, &dir, &Options::default()).out).unwrap();
    let r = run(&fake, &dir, &Options::default());
    let second = code_in(&r.out).unwrap();
    assert_ne!(first, second);
    assert_eq!(
        setting(&fake, "setup_code_hash").unwrap(),
        setup_hash_json(&code_hash(&second))
    );
}

/// A failure at each step stops setup; the next run completes with nothing duplicated, and
/// the secrets are never rotated. `DropAfter`: Cloudflare did it, the reply was lost.
#[test]
fn a_failure_at_any_step_then_a_rerun_completes_without_duplicates() {
    let cases: &[(&str, Failure)] = &[
        ("GET /user/tokens/verify", Failure::Status(400)),
        ("GET /zones", Failure::Status(400)),
        ("POST /accounts/acc1/d1/database", Failure::Status(400)),
        ("POST /accounts/acc1/d1/database", Failure::DropAfter),
        ("POST /accounts/acc1/r2/buckets", Failure::Status(400)),
        ("POST /accounts/acc1/r2/buckets", Failure::DropAfter),
        ("/time_travel/bookmark", Failure::Status(400)),
        ("/query", Failure::Status(400)),
        (
            "PUT /accounts/acc1/workers/scripts/plumblog",
            Failure::Status(400),
        ),
        (
            "PUT /accounts/acc1/workers/scripts/plumblog",
            Failure::DropAfter,
        ),
        (
            "POST /accounts/acc1/workers/scripts/plumblog/subdomain",
            Failure::Status(400),
        ),
        (
            "PUT /accounts/acc1/workers/scripts/plumblog/secrets",
            Failure::Status(400),
        ),
        (
            "PUT /accounts/acc1/workers/scripts/plumblog/secrets",
            Failure::DropAfter,
        ),
        (
            "POST /accounts/acc1/challenges/widgets",
            Failure::Status(400),
        ),
        ("POST /accounts/acc1/challenges/widgets", Failure::DropAfter),
        ("PUT /accounts/acc1/workers/domains", Failure::Status(400)),
        ("PUT /accounts/acc1/workers/domains", Failure::DropAfter),
    ];
    let opts = Options {
        domain: Some("blog.example.com".into()),
        ..Options::default()
    };
    for (pattern, failure) in cases {
        let fake = Fake::start(site(ALL));
        let dir = TempDir::new();
        fake.fail_on(pattern, *failure);
        let first = run(&fake, &dir, &opts);
        // A lost reply to a create (POST) is recovered in the same run, by listing again;
        // every other injected failure stops the run.
        let recovered = matches!(failure, Failure::DropAfter) && pattern.starts_with("POST");
        assert_eq!(
            first.result.is_ok(),
            recovered,
            "{pattern} {failure:?}: {:?}",
            first.result
        );
        let key = secret(&fake, "WEBHOOK_KEY");
        let second = run(&fake, &dir, &opts);
        ok(&second);
        let ctx = format!("{pattern} {failure:?}");
        let st = fake.state();
        assert_eq!(st.d1.len(), 1, "{ctx}");
        assert_eq!(st.r2.len(), 1, "{ctx}");
        assert_eq!(st.widgets.len(), 1, "{ctx}");
        assert_eq!(st.domains.len(), 1, "{ctx}");
        assert_eq!(st.scripts.len(), 1, "{ctx}");
        assert_eq!(st.scripts["plumblog"].uploads, 1, "{ctx}");
        if key.is_some() {
            assert_eq!(
                st.scripts["plumblog"].secrets.get("WEBHOOK_KEY"),
                key.as_ref(),
                "{ctx}"
            );
        }
        assert_eq!(
            st.scripts["plumblog"].secrets["TURNSTILE_SECRET"], st.widgets[0].secret,
            "{ctx}"
        );
        drop(st);
        let rows = fake.sql(
            "db-1",
            "SELECT COUNT(*) AS n, COUNT(DISTINCT name) AS d FROM d1_migrations",
        );
        let n = migrations::bundled().len();
        assert_eq!(
            (rows[0]["n"].clone(), rows[0]["d"].clone()),
            (json!(n), json!(n)),
            "{ctx}"
        );
        assert_eq!(
            setting(&fake, "origin").unwrap(),
            "\"https://blog.example.com\"",
            "{ctx}"
        );
        assert!(code_in(&second.out).is_some(), "{ctx}");
    }
}

#[test]
fn no_turnstile_needs_confirmation_and_then_warns() {
    let fake = Fake::start(site(ALL));
    let dir = TempDir::new();
    let refused = run(
        &fake,
        &dir,
        &Options {
            turnstile: Turnstile::OffUnconfirmed,
            ..Options::default()
        },
    );
    let e = refused.result.unwrap_err();
    assert_eq!(e.code, Code::NoTurnstileUnconfirmed);
    assert!(e.message.contains("--yes-i-understand"), "{}", e.message);
    assert!(fake.requests().is_empty(), "nothing is called");

    let r = run(
        &fake,
        &dir,
        &Options {
            turnstile: Turnstile::Off,
            ..Options::default()
        },
    );
    ok(&r);
    assert!(r.out.contains("Warning: Turnstile is off."), "{}", r.out);
    assert!(r.out.contains("15-minute") && r.out.contains("comments stay off"));
    assert!(
        fake.requests()
            .iter()
            .all(|q| !q.path.contains("/challenges/"))
    );
    assert_eq!(secret(&fake, "TURNSTILE_SECRET"), None);
    assert_eq!(setting(&fake, "turnstile_site_key"), None);
    assert!(setting(&fake, "origin").is_some());
}

#[test]
fn a_domain_without_a_zone_stops_before_changing_anything() {
    let fake = Fake::start(site(ALL));
    let dir = TempDir::new();
    let r = run(
        &fake,
        &dir,
        &Options {
            domain: Some("Blog.Unknown.ORG".into()),
            ..Options::default()
        },
    );
    let e = r.result.unwrap_err();
    assert_eq!(e.code, Code::ZoneNotFound);
    assert!(
        e.message.contains("blog.unknown.org") && e.message.contains("unknown.org"),
        "{}",
        e.message
    );
    assert_eq!(writes(&fake.requests()), Vec::<String>::new());
    let zone_queries: Vec<String> = fake
        .requests()
        .iter()
        .filter(|q| q.path == "/zones")
        .map(|q| q.query.clone())
        .collect();
    assert_eq!(
        zone_queries,
        [
            "name=blog.unknown.org&account.id=acc1",
            "name=unknown.org&account.id=acc1"
        ]
    );
    // A bad domain is refused before any call.
    let fake = Fake::start(site(ALL));
    let r = run(
        &fake,
        &dir,
        &Options {
            domain: Some("https://blog.example.com/".into()),
            ..Options::default()
        },
    );
    assert_eq!(r.result.unwrap_err().code, Code::DomainInvalid);
    assert!(fake.requests().is_empty());
}

#[test]
fn a_domain_on_another_worker_is_not_taken_over() {
    let mut st = site(ALL);
    st.domains.push(
        json!({"id": "d1", "hostname": "blog.example.com", "service": "other",
                           "zone_id": "zone-1"}),
    );
    let fake = Fake::start(st);
    let dir = TempDir::new();
    let r = run(
        &fake,
        &dir,
        &Options {
            domain: Some("blog.example.com".into()),
            ..Options::default()
        },
    );
    let e = r.result.unwrap_err();
    assert_eq!(e.code, Code::DomainTaken);
    assert!(e.message.contains("other"), "{}", e.message);
    assert_eq!(fake.state().domains[0]["service"], "other");
}

#[test]
fn a_missing_permission_mid_setup_names_it() {
    let perms: Vec<&'static str> = ALL
        .iter()
        .copied()
        .filter(|p| !p.contains("Turnstile"))
        .collect();
    let fake = Fake::start(site(&perms));
    let dir = TempDir::new();
    let r = run(&fake, &dir, &Options::default());
    let e = r.result.unwrap_err();
    assert_eq!(e.code, Code::PermissionMissing);
    assert!(
        e.message.contains("\"Account | Turnstile | Edit\""),
        "{}",
        e.message
    );
    // Earlier steps are done and recorded; no settings were written yet.
    let st = state::load(&dir.0).unwrap().unwrap();
    assert!(st.d1.is_some() && st.r2.is_some() && st.turnstile_site_key.is_none());
    assert_eq!(setting(&fake, "origin"), None);
}

#[test]
fn a_migration_failing_halfway_reports_it_and_writes_no_settings() {
    let mut migs = migrations::bundled();
    migs.push(Migration {
        name: "0099_bad.sql".into(),
        sql: "CREATE TABLE half_done (x INTEGER);\nINSERT INTO no_such_table VALUES (1);".into(),
    });

    // A new database: the bookmark is the clean state before 0001.
    let fake = Fake::start(site(ALL));
    let dir = TempDir::new();
    let r = run_with(&fake, &dir, &Options::default(), &migs, &en());
    let e = r.result.unwrap_err();
    assert_eq!(e.code, Code::MigrationFailed);
    for part in [
        "0099_bad.sql",
        "no such table",
        "bm-db-1-1",
        "`blog rollback --db bm-db-1-1 --yes`",
    ] {
        assert!(e.message.contains(part), "{part}: {}", e.message);
    }
    // Not atomic: the first statement stayed. The failed one is not recorded.
    assert_eq!(fake.sql("db-1", "SELECT * FROM half_done").len(), 0);
    let recorded = fake.sql("db-1", "SELECT name FROM d1_migrations ORDER BY id");
    assert_eq!(recorded.len(), migs.len() - 1);
    assert_eq!(fake.sql("db-1", "SELECT * FROM settings").len(), 0);
    let st = state::load(&dir.0).unwrap().unwrap();
    assert_eq!(st.migrations.last_bookmark.as_deref(), Some("bm-db-1-1"));
    assert_eq!(st.migrations.applied.len(), migs.len() - 1);
    assert!(
        fake.state().scripts.is_empty(),
        "no step after the migration ran"
    );

    // A database with tables and settings: a new bookmark, and nothing written after.
    let fake = Fake::start(site(ALL));
    let dir = TempDir::new();
    ok(&run(&fake, &dir, &Options::default()));
    let before = fake.sql(
        "db-1",
        "SELECT key, value_json, updated_at FROM settings ORDER BY key",
    );
    let n = fake.requests().len();
    let r = run_with(&fake, &dir, &Options::default(), &migs, &Text::new("vi"));
    let e = r.result.unwrap_err();
    assert_eq!(e.code, Code::MigrationFailed);
    assert!(e.message.contains("0099_bad.sql") && e.message.contains("bm-db-1-2"));
    assert!(
        e.message.contains("blog rollback --db bm-db-1-2 --yes"),
        "{}",
        e.message
    );
    let after = &fake.requests()[n..];
    let failed_at = after
        .iter()
        .position(|q| q.body.contains("no_such_table"))
        .unwrap();
    assert_eq!(
        after.len(),
        failed_at + 1,
        "no request after the failed migration"
    );
    assert_eq!(
        fake.sql(
            "db-1",
            "SELECT key, value_json, updated_at FROM settings ORDER BY key"
        ),
        before
    );
}

#[test]
fn the_database_is_found_on_a_later_page() {
    let mut st = site(ALL);
    for i in 0..150 {
        st.d1
            .push(json!({"uuid": format!("other-{i}"), "name": format!("other-{i}")}));
    }
    st.d1.push(json!({"uuid": "db-1", "name": "plumblog"}));
    st.sqlite.insert(
        "db-1".into(),
        rusqlite::Connection::open_in_memory().unwrap(),
    );
    let fake = Fake::start(st);
    let dir = TempDir::new();
    ok(&run(&fake, &dir, &Options::default()));
    assert_eq!(fake.state().d1.len(), 151);
    let pages: Vec<String> = fake
        .requests()
        .iter()
        .filter(|q| q.path == "/accounts/acc1/d1/database" && q.method == "GET")
        .map(|q| q.query.clone())
        .collect();
    assert_eq!(pages, ["per_page=100&page=1", "per_page=100&page=2"]);
    assert_eq!(state::load(&dir.0).unwrap().unwrap().d1.unwrap().id, "db-1");
}

#[test]
fn a_folder_belongs_to_one_site() {
    let fake = Fake::start(site(ALL));
    let dir = TempDir::new();
    ok(&run(&fake, &dir, &Options::default()));
    let n = fake.requests().len();
    let r = run(
        &fake,
        &dir,
        &Options {
            name: Some("other".into()),
            ..Options::default()
        },
    );
    let e = r.result.unwrap_err();
    assert_eq!(e.code, Code::StateMismatch);
    assert!(e.message.contains("--name") && e.message.contains("plumblog"));
    assert_eq!(writes(&fake.requests()[n..]), Vec::<String>::new());
    let bad = run(
        &fake,
        &dir,
        &Options {
            name: Some("Bad_Name".into()),
            ..Options::default()
        },
    );
    assert_eq!(bad.result.unwrap_err().code, Code::NameInvalid);
}

fn blog_setup(dir: &TempDir, base: &str, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_blog"))
        .args(args)
        .current_dir(&dir.0)
        .env_remove("CLOUDFLARE_API_TOKEN")
        .env_remove("CLOUDFLARE_ACCOUNT_ID")
        .env("PLUMBLOG_CF_API_BASE", base)
        .output()
        .unwrap()
}

/// The binary end to end, in Vietnamese: the code is printed once, nothing secret is
/// written; `--no-turnstile` alone is refused with its code before any call.
#[test]
fn binary_sets_up_in_vietnamese_and_refuses_no_turnstile_alone() {
    let fake = Fake::start(site(ALL));
    let dir = TempDir::new();
    dir.write_env(GOOD);
    let out = blog_setup(
        &dir,
        &fake.base,
        &["--lang", "vi", "setup", "--no-turnstile"],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains("[setup.no_turnstile_unconfirmed]"),
        "{stderr}"
    );
    assert!(fake.requests().is_empty());

    let out = blog_setup(&dir, &fake.base, &["--lang", "vi", "setup"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    if std::env::var_os("PLUMBLOG_SHOW_SETUP").is_some() {
        eprintln!("{stdout}");
    }
    let vi = Text::new("vi");
    let code_prefix = vi.t(key!("cli.setup.code"), &[("code", String::new())]);
    let done = vi.t(key!("cli.setup.done"), &[("origin", ORIGIN.to_string())]);
    assert!(stdout.contains(&done), "{stdout}");
    let code = stdout
        .lines()
        .find_map(|l| l.strip_prefix(code_prefix.as_str()))
        .map_or_else(|| panic!("no code in {stdout}"), str::to_string);
    assert_eq!(
        setting(&fake, "setup_code_hash").unwrap(),
        setup_hash_json(&code_hash(&code))
    );
    let key = secret(&fake, "WEBHOOK_KEY").unwrap();
    let tsecret = secret(&fake, "TURNSTILE_SECRET").unwrap();
    assert_not_in_files(&dir, &[GOOD, &key, &tsecret, &code]);
    for v in [GOOD, key.as_str(), tsecret.as_str()] {
        assert!(!stdout.contains(v) && !String::from_utf8_lossy(&out.stderr).contains(v));
    }
}

#[test]
fn a_lost_state_file_does_not_move_the_site() {
    let fake = Fake::start(site(ALL));
    let dir = TempDir::new();
    let with_domain = Options {
        domain: Some("blog.example.com".into()),
        ..Options::default()
    };
    ok(&run(&fake, &dir, &with_domain));
    std::fs::remove_file(state::path(&dir.0)).unwrap();
    let n = fake.requests().len();
    let r = run(&fake, &dir, &Options::default());
    let e = r.result.unwrap_err();
    assert_eq!(e.code, Code::OriginChangeUnconfirmed);
    assert!(
        e.message.contains("https://blog.example.com") && e.message.contains(ORIGIN),
        "{}",
        e.message
    );
    assert_eq!(changes(&fake.requests()[n..]), Vec::<String>::new());
    assert_eq!(
        setting(&fake, "origin").unwrap(),
        "\"https://blog.example.com\""
    );
    // Explicitly: the same domain resumes; --change-origin really moves the site.
    ok(&run(&fake, &dir, &with_domain));
    std::fs::remove_file(state::path(&dir.0)).unwrap();
    let moved = run(
        &fake,
        &dir,
        &Options {
            change_origin: true,
            ..Options::default()
        },
    );
    ok(&moved);
    assert_eq!(setting(&fake, "origin").unwrap(), format!("\"{ORIGIN}\""));
}

#[test]
fn after_a_failed_migration_nothing_migrates_until_restored() {
    let mut migs = migrations::bundled();
    let last = migs.last_mut().unwrap();
    last.sql.push_str("\nINSERT INTO no_such_table VALUES (1);");
    let failing = last.name.clone();
    let fake = Fake::start(site(ALL));
    let dir = TempDir::new();
    let r = run_with(&fake, &dir, &Options::default(), &migs, &en());
    let e = r.result.unwrap_err();
    assert_eq!(e.code, Code::MigrationFailed);
    assert!(e.message.contains("change nothing else"), "{}", e.message);
    let st = state::load(&dir.0).unwrap().unwrap();
    let failed = st.migrations.failed.clone().unwrap();
    assert_eq!(
        (failed.name.as_str(), failed.bookmark.as_str()),
        (failing.as_str(), "bm-db-1-1")
    );

    let n = fake.requests().len();
    let again = run(&fake, &dir, &Options::default());
    let e = again.result.unwrap_err();
    assert_eq!(e.code, Code::MigrationRestorePending);
    for part in [
        failing.as_str(),
        "bm-db-1-1",
        "blog rollback --db bm-db-1-1 --yes",
        "--clear-failed-migration",
    ] {
        assert!(e.message.contains(part), "{part}: {}", e.message);
    }
    assert_eq!(
        fake.state().bookmarks.len(),
        1,
        "no new bookmark of the half-migrated database"
    );
    assert_eq!(changes(&fake.requests()[n..]), Vec::<String>::new());
    let st = state::load(&dir.0).unwrap().unwrap();
    assert_eq!(st.migrations.last_bookmark.as_deref(), Some("bm-db-1-1"));
    assert_eq!(st.migrations.failed, Some(failed));
}

#[test]
fn a_database_blog_did_not_make_is_left_alone() {
    let mut st = site(ALL);
    st.d1.push(json!({"uuid": "db-1", "name": "plumblog"}));
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE TABLE orders (id INTEGER PRIMARY KEY);")
        .unwrap();
    st.sqlite.insert("db-1".into(), conn);
    let fake = Fake::start(st);
    let dir = TempDir::new();
    let r = run(&fake, &dir, &Options::default());
    let e = r.result.unwrap_err();
    assert_eq!(e.code, Code::DatabaseForeign);
    assert!(e.message.contains("orders"), "{}", e.message);
    assert_eq!(changes(&fake.requests()), Vec::<String>::new());
    let tables = fake.sql(
        "db-1",
        "SELECT name FROM sqlite_master WHERE type = 'table'",
    );
    assert_eq!(tables.len(), 1);
    assert!(fake.state().r2.is_empty());
}

#[test]
fn only_script_not_found_uploads_the_placeholder() {
    let fake = Fake::start(site(ALL));
    let dir = TempDir::new();
    fake.fail_on(
        "GET /accounts/acc1/workers/scripts/plumblog/secrets",
        Failure::Status(404),
    );
    let r = run(&fake, &dir, &Options::default());
    let e = r.result.unwrap_err();
    assert_eq!(e.code, Code::HttpFailed, "{}", e.message);
    assert!(
        fake.requests()
            .iter()
            .all(|q| !(q.method == "PUT" && q.path == "/accounts/acc1/workers/scripts/plumblog"))
    );
    assert!(fake.state().scripts.is_empty());
}

#[test]
fn two_widgets_with_the_site_name_change_nothing() {
    let mut st = site(ALL);
    for n in 1..=2 {
        st.widgets.push(crate::fake_cf::Widget {
            sitekey: format!("0x4AAAAAAAdup{n}"),
            secret: format!("dup-secret-{n}"),
            name: "plumblog-plumblog".into(),
            domains: vec!["plumblog.steven.workers.dev".into()],
            mode: "managed".into(),
        });
    }
    let fake = Fake::start(st);
    let dir = TempDir::new();
    let e = run(&fake, &dir, &Options::default()).result.unwrap_err();
    assert_eq!(e.code, Code::TurnstileAmbiguous);
    assert!(
        e.message.contains("0x4AAAAAAAdup1") && e.message.contains("0x4AAAAAAAdup2"),
        "{}",
        e.message
    );
    assert_eq!(fake.state().widgets.len(), 2);
    assert_eq!(secret(&fake, "TURNSTILE_SECRET"), None);
    assert!(
        fake.requests()
            .iter()
            .all(|q| !q.path.ends_with("/challenges/widgets/0x4AAAAAAAdup1"))
    );
}

#[test]
fn a_domain_on_page_two_attached_elsewhere_is_found() {
    let mut st = site(ALL);
    for i in 0..120 {
        st.domains.push(
            json!({"id": format!("d{i}"), "hostname": format!("x{i}.example.com"),
                               "service": "other", "zone_id": "zone-1"}),
        );
    }
    st.domains.push(
        json!({"id": "dz", "hostname": "blog.example.com", "service": "other",
                           "zone_id": "zone-1"}),
    );
    let fake = Fake::start(st);
    let dir = TempDir::new();
    let r = run(
        &fake,
        &dir,
        &Options {
            domain: Some("blog.example.com".into()),
            ..Options::default()
        },
    );
    assert_eq!(r.result.unwrap_err().code, Code::DomainTaken);
    let pages = fake
        .requests()
        .iter()
        .filter(|q| q.method == "GET" && q.path == "/accounts/acc1/workers/domains")
        .count();
    assert_eq!(pages, 2);
    assert!(
        fake.requests()
            .iter()
            .all(|q| q.method != "PUT" || !q.path.ends_with("/domains"))
    );
}

#[test]
fn an_owner_created_during_setup_gets_no_code() {
    let mut st = site(ALL);
    st.after_query.push((
        "SELECT 1 AS x FROM users LIMIT 1".into(),
        "INSERT INTO users (login, slug, display_name, role, password_hash, created_at, \
         updated_at) VALUES ('racer', 'racer', 'Racer', 'owner', 'x', 1, 1)"
            .into(),
    ));
    let fake = Fake::start(st);
    let dir = TempDir::new();
    let r = run(&fake, &dir, &Options::default());
    ok(&r);
    assert!(code_in(&r.out).is_none(), "{}", r.out);
    assert!(
        r.out.contains(&en().t(key!("cli.setup.owner_exists"), &[])),
        "{}",
        r.out
    );
    assert_eq!(
        fake.sql(
            "db-1",
            "SELECT key FROM settings WHERE key LIKE 'setup_code%'"
        )
        .len(),
        0
    );
}
