//! `blog admin reset` against the fake API with the real migrations in SQLite: every refusal
//! changes nothing, a reset locks out only the named owner, and the code is shown once.

use super::{GOOD, TempDir, client};
use crate::fake_cf::Fake;
use crate::setup::{ALL, ok as setup_ok, run_with as setup_with, site};
use plumblog_cli::admin::{self, Options};
use plumblog_cli::migrations;
use plumblog_cli::setup::{Context, Failure, Options as SetupOptions, code_hash};
use plumblog_cli::state;
use plumblog_cli::text::{Code, Text};
use plumblog_store::setup_hash_json;
use serde_json::Value;

const NOW: i64 = 1_790_000_000_000;
const DB: &str = "db-1";
const HOUR: i64 = 3_600_000;

struct Ran {
    result: Result<(), Failure>,
    out: String,
}

fn en() -> Text {
    Text::new("en")
}

/// A set-up site with an owner `steven` (id 1: password, a session, a passkey, a token) and
/// a writer `wanda` (id 2) with the same.
fn set_up() -> (Fake, TempDir) {
    let fake = Fake::start(site(ALL));
    let dir = TempDir::new();
    setup_ok(&setup_with(
        &fake,
        &dir,
        &SetupOptions::default(),
        &migrations::bundled(),
        &en(),
    ));
    fake.exec(
        DB,
        "INSERT INTO users (id, login, slug, display_name, role, password_hash, created_at, \
         updated_at) VALUES (1, 'Steven', 'steven', 'Steven', 'owner', 'pw-old', 1, 1), \
         (2, 'wanda', 'wanda', 'Wanda', 'writer', 'pw-wanda', 1, 1);
         INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES \
         ('s1', 1, 1, 9999999999999), ('s2', 2, 1, 9999999999999);
         INSERT INTO passkeys (user_id, credential_id, public_key, created_at) VALUES \
         (1, 'c1', 'k', 1), (2, 'c2', 'k', 1);
         INSERT INTO api_tokens (user_id, name, token_hash, scope, created_at) VALUES \
         (1, 'mine', 't1', 'owner', 1), (2, 'hers', 't2', 'writer', 1);",
    );
    (fake, dir)
}

fn reset(fake: &Fake, dir: &TempDir, o: &Options) -> Ran {
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
    let result = admin::run(&mut c, None, o, &cx, &mut buf);
    Ran {
        result,
        out: String::from_utf8(buf).unwrap(),
    }
}

fn yes() -> Options {
    Options {
        yes: true,
        ..Options::default()
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

/// Every row a reset could touch, as text, to compare before and after a refusal.
fn snapshot(fake: &Fake) -> String {
    let mut out = String::new();
    for sql in [
        "SELECT id, login, password_hash, updated_at FROM users ORDER BY id",
        "SELECT token_hash, user_id FROM sessions ORDER BY token_hash",
        "SELECT credential_id, user_id FROM passkeys ORDER BY credential_id",
        "SELECT token_hash, revoked_at FROM api_tokens ORDER BY token_hash",
        "SELECT key, value_json FROM settings ORDER BY key",
        "SELECT kind, detail FROM system_log ORDER BY id",
    ] {
        out.push_str(&serde_json::to_string(&fake.sql(DB, sql)).unwrap());
        out.push('\n');
    }
    out
}

fn setting(fake: &Fake, key: &str) -> Option<String> {
    fake.sql(
        DB,
        &format!("SELECT value_json FROM settings WHERE key = '{key}'"),
    )
    .first()
    .and_then(|r| r["value_json"].as_str().map(str::to_string))
}

fn count(fake: &Fake, sql: &str) -> i64 {
    fake.sql(DB, sql)[0]["n"].as_i64().unwrap()
}

/// The code from the line that shows it.
fn printed_code(out: &str) -> String {
    let line = out
        .lines()
        .find(|l| l.starts_with("Reset code"))
        .unwrap_or_else(|| panic!("no code line in:\n{out}"));
    line.rsplit(": ").next().unwrap().to_string()
}

#[test]
fn without_yes_it_prints_the_plan_and_changes_nothing() {
    let (fake, dir) = set_up();
    let before = snapshot(&fake);
    let r = reset(&fake, &dir, &Options::default());
    assert_eq!(code(&r), Code::AdminResetUnconfirmed);
    assert!(r.out.contains("Owner to reset: Steven"), "{}", r.out);
    assert!(
        r.out
            .contains("1 passkeys are removed and 1 signed-in sessions end")
    );
    assert!(r.out.contains("Their 1 API tokens are revoked"));
    assert!(!r.out.contains("Reset code"), "{}", r.out);
    assert_eq!(snapshot(&fake), before);
}

#[test]
fn a_foreign_worker_or_database_is_refused() {
    let (fake, dir) = set_up();
    let before = snapshot(&fake);
    assert!(
        state::load(&dir.0).unwrap().unwrap().site_id.is_some(),
        "setup records the site id"
    );

    // The Worker carries another folder's marker.
    let mine = state::load(&dir.0).unwrap().unwrap().script_marker;
    state::update(&dir.0, &[], |s| {
        s.script_marker = Some("someone-else".into());
    })
    .unwrap();
    assert_eq!(code(&reset(&fake, &dir, &yes())), Code::AdminResetForeign);
    assert_eq!(snapshot(&fake), before);
    state::update(&dir.0, &[], |s| s.script_marker = mine).unwrap();

    // The database carries another site id.
    let real = setting(&fake, "site_id").unwrap();
    fake.exec(
        DB,
        "UPDATE settings SET value_json = '\"other\"' WHERE key = 'site_id'",
    );
    let before = snapshot(&fake);
    assert_eq!(code(&reset(&fake, &dir, &yes())), Code::AdminResetForeign);
    assert_eq!(snapshot(&fake), before);
    fake.exec(
        DB,
        &format!("UPDATE settings SET value_json = '{real}' WHERE key = 'site_id'"),
    );

    // A folder that never recorded the site id proves nothing.
    let recorded = state::load(&dir.0).unwrap().unwrap().site_id;
    state::update(&dir.0, &[], |s| s.site_id = None).unwrap();
    let before = snapshot(&fake);
    assert_eq!(code(&reset(&fake, &dir, &yes())), Code::AdminResetForeign);
    assert_eq!(snapshot(&fake), before);

    // With both marks back it is ours.
    state::update(&dir.0, &[], |s| s.site_id = recorded).unwrap();
    ok(&reset(&fake, &dir, &yes()));
}

#[test]
fn several_owners_need_login_and_a_non_owner_is_refused() {
    let (fake, dir) = set_up();
    fake.exec(
        DB,
        "INSERT INTO users (id, login, slug, display_name, role, password_hash, created_at, \
         updated_at) VALUES (3, 'olga', 'olga', 'Olga', 'owner', 'pw-olga', 1, 1)",
    );
    let before = snapshot(&fake);
    let r = reset(&fake, &dir, &yes());
    assert_eq!(code(&r), Code::AdminResetAmbiguous);
    let msg = r.result.unwrap_err().message;
    assert!(msg.contains("Steven") && msg.contains("olga"), "{msg}");
    for login in ["wanda", "nobody"] {
        let o = Options {
            login: Some(login.into()),
            ..yes()
        };
        assert_eq!(code(&reset(&fake, &dir, &o)), Code::AdminResetNotOwner);
    }
    assert_eq!(snapshot(&fake), before);

    // Named, case-insensitively: only that owner is reset.
    let o = Options {
        login: Some("OLGA".into()),
        ..yes()
    };
    ok(&reset(&fake, &dir, &o));
    let pw = fake.sql(DB, "SELECT id, password_hash FROM users ORDER BY id");
    assert_eq!(pw[0]["password_hash"], "pw-old");
    assert_eq!(pw[2]["password_hash"], Value::Null);
    assert_eq!(setting(&fake, "owner_reset_user").unwrap(), "3");
}

#[test]
fn reset_revokes_everything_of_that_owner_and_nobody_else() {
    let (fake, dir) = set_up();
    ok(&reset(&fake, &dir, &yes()));
    let users = fake.sql(DB, "SELECT password_hash FROM users ORDER BY id");
    assert_eq!(users[0]["password_hash"], Value::Null);
    assert_eq!(users[1]["password_hash"], "pw-wanda");
    for (table, extra) in [("sessions", ""), ("passkeys", "")] {
        let q = |u: i64| format!("SELECT count(*) AS n FROM {table} WHERE user_id = {u}{extra}");
        assert_eq!(count(&fake, &q(1)), 0, "{table} of the owner");
        assert_eq!(count(&fake, &q(2)), 1, "{table} of the writer");
    }
    let tokens = fake.sql(
        DB,
        "SELECT token_hash, revoked_at FROM api_tokens ORDER BY token_hash",
    );
    assert_eq!(tokens[0]["revoked_at"], NOW);
    assert_eq!(tokens[1]["revoked_at"], Value::Null);
}

#[test]
fn keep_tokens_leaves_tokens_and_says_so() {
    let (fake, dir) = set_up();
    let o = Options {
        keep_tokens: true,
        ..yes()
    };
    let r = reset(&fake, &dir, &o);
    ok(&r);
    assert!(r.out.contains("API tokens were kept"), "{}", r.out);
    assert_eq!(
        count(
            &fake,
            "SELECT count(*) AS n FROM api_tokens WHERE revoked_at IS NULL"
        ),
        2
    );
    assert_eq!(
        count(
            &fake,
            "SELECT count(*) AS n FROM sessions WHERE user_id = 1"
        ),
        0
    );
}

#[test]
fn the_code_is_stored_hashed_printed_once_and_never_saved() {
    let (fake, dir) = set_up();
    let r = reset(&fake, &dir, &yes());
    ok(&r);
    let code = printed_code(&r.out);
    assert_eq!(r.out.matches(&code).count(), 1, "{}", r.out);
    assert!(
        r.out
            .contains("https://plumblog.steven.workers.dev/admin/reset"),
        "{}",
        r.out
    );
    assert_eq!(
        setting(&fake, "owner_reset_hash").unwrap(),
        setup_hash_json(&code_hash(&code))
    );
    assert_eq!(setting(&fake, "owner_reset_user").unwrap(), "1");
    assert_eq!(
        setting(&fake, "owner_reset_expires_at").unwrap(),
        (NOW + HOUR).to_string()
    );
    let file = std::fs::read_to_string(state::path(&dir.0)).unwrap();
    assert!(!file.contains(&code));
    let dump = snapshot(&fake);
    assert!(!dump.contains(&code), "the code itself is stored nowhere");

    // Again: a new code replaces the old one.
    let again = printed_code(&reset(&fake, &dir, &yes()).out);
    assert_ne!(again, code);
    assert_eq!(
        setting(&fake, "owner_reset_hash").unwrap(),
        setup_hash_json(&code_hash(&again))
    );
}

#[test]
fn the_reset_is_logged_without_the_code() {
    let (fake, dir) = set_up();
    let r = reset(&fake, &dir, &yes());
    ok(&r);
    let code = printed_code(&r.out);
    let rows = fake.sql(
        DB,
        "SELECT kind, actor_id, detail FROM system_log WHERE kind = 'auth.owner_reset'",
    );
    assert_eq!(rows.len(), 1);
    let detail = rows[0]["detail"].as_str().unwrap();
    assert!(detail.contains("user\t1"), "{detail}");
    assert!(
        detail.contains("revoked\tpassword,passkeys,sessions,tokens"),
        "{detail}"
    );
    assert!(detail.contains("via\tcli"), "{detail}");
    assert!(!detail.contains(&code));
}

#[test]
fn the_folder_lock_is_held() {
    let (fake, dir) = set_up();
    let lock = dir.0.join(plumblog_cli::deploy::LOCK_FILE);
    std::fs::write(&lock, "1").unwrap();
    let before = snapshot(&fake);
    assert_eq!(code(&reset(&fake, &dir, &yes())), Code::DeployLocked);
    assert_eq!(snapshot(&fake), before);
    std::fs::remove_file(&lock).unwrap();
    ok(&reset(&fake, &dir, &yes()));
    assert!(!lock.exists(), "the lock goes when the run ends");
}

#[test]
fn a_worker_without_the_marker_binding_needs_setup_then_deploy() {
    let (fake, dir) = set_up();
    // An older setup, or a rollback to a version from before the marker: no binding at all.
    {
        let mut st = fake.state();
        for v in &mut st.versions {
            if let Some(b) = v.metadata["bindings"].as_array_mut() {
                b.retain(|b| b["name"] != "PLUMBLOG_SITE_ID");
            }
        }
    }
    let before = snapshot(&fake);
    let r = reset(&fake, &dir, &yes());
    assert_eq!(code(&r), Code::AdminResetUnmarked);
    let msg = &r.result.as_ref().unwrap_err().message;
    assert!(
        msg.contains("blog setup") && msg.contains("blog deploy"),
        "{msg}"
    );
    assert_eq!(snapshot(&fake), before);
}

/// tools/worker-test replays the reset's exact SQL from this file. `$user`, `$now`,
/// `$hash_json` and `$expires` stand for the values; `BLESS=1` rewrites it.
#[test]
fn the_worker_test_fixture_is_the_reset_sql() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/worker-test/fixtures/admin-reset.json");
    let batch = |keep_tokens: bool| {
        let log = admin::reset_log("$user", keep_tokens, 0);
        let log = [
            "$now".to_string(),
            log[1].replace("-0", "-$now"),
            log[2].clone(),
            log[3].clone(),
        ];
        let stmts = admin::reset_statements(&admin::Reset {
            user: "$user",
            now: "$now",
            hash_json: "$hash_json",
            expires: "$expires",
            keep_tokens,
            log: &log,
        });
        Value::Array(
            stmts
                .into_iter()
                .map(|(sql, params)| serde_json::json!({"sql": sql, "params": params}))
                .collect(),
        )
    };
    let want = serde_json::to_string_pretty(
        &serde_json::json!({"revoke_tokens": batch(false), "keep_tokens": batch(true)}),
    )
    .unwrap()
        + "\n";
    if std::env::var_os("BLESS").is_some() {
        std::fs::write(&path, &want).unwrap();
    }
    let got = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        got == want,
        "{} is stale: run `BLESS=1 cargo test -p plumblog-cli --test cloudflare admin::` after changing the reset SQL",
        path.display()
    );
}

#[test]
fn the_code_carries_the_reset_time_and_the_token_choice() {
    let (fake, dir) = set_up();
    ok(&reset(&fake, &dir, &yes()));
    assert_eq!(setting(&fake, "owner_reset_at"), Some(NOW.to_string()));
    assert_eq!(
        setting(&fake, "owner_reset_keep_tokens").as_deref(),
        Some("false")
    );
    let keep = Options {
        keep_tokens: true,
        ..yes()
    };
    ok(&reset(&fake, &dir, &keep));
    assert_eq!(
        setting(&fake, "owner_reset_keep_tokens").as_deref(),
        Some("true")
    );
}
