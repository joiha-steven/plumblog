//! `blog token` against the fake API with the real migrations in SQLite: a token is made the
//! way the Worker makes it, shown once, and every refusal changes nothing.

use super::{GOOD, TempDir, client};
use crate::fake_cf::Fake;
use crate::setup::{ALL, ok as setup_ok, run_with as setup_with, site};
use plumblog_auth::api_token::ApiTokenSecret;
use plumblog_cli::migrations;
use plumblog_cli::setup::{Context, Failure, Options as SetupOptions};
use plumblog_cli::state;
use plumblog_cli::text::{Code, Text};
use plumblog_cli::token::{self, Action, Create, NewRow};
use serde_json::Value;

const NOW: i64 = 1_790_000_000_000;
const DB: &str = "db-1";
const DAY: i64 = 86_400_000;

struct Ran {
    result: Result<(), Failure>,
    out: String,
}

fn en() -> Text {
    Text::new("en")
}

/// A set-up site with an owner `steven` (id 1) and a writer `wanda` (id 2), each with one
/// token (hashes `t1`, `t2`).
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
         updated_at) VALUES (1, 'Steven', 'steven', 'Steven', 'owner', 'pw', 1, 1), \
         (2, 'wanda', 'wanda', 'Wanda', 'writer', 'pw', 1, 1);
         INSERT INTO api_tokens (user_id, name, token_hash, scope, created_at) VALUES \
         (1, 'mine', 't1', 'owner', 1), (2, 'hers', 't2', 'writer', 1);",
    );
    (fake, dir)
}

fn run(fake: &Fake, dir: &TempDir, a: &Action) -> Ran {
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
    let result = token::run(&mut c, None, a, &cx, &mut buf);
    Ran {
        result,
        out: String::from_utf8(buf).unwrap(),
    }
}

fn create(name: &str) -> Create {
    Create {
        name: name.into(),
        scope: "writer".into(),
        login: None,
        expires_days: None,
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

/// Every row a token command could touch, to compare before and after a refusal.
fn snapshot(fake: &Fake) -> String {
    let mut out = String::new();
    for sql in [
        "SELECT * FROM api_tokens ORDER BY id",
        "SELECT kind, detail FROM system_log ORDER BY id",
    ] {
        out.push_str(&serde_json::to_string(&fake.sql(DB, sql)).unwrap());
        out.push('\n');
    }
    out
}

/// The token from the line that shows it.
fn printed_token(out: &str) -> String {
    let line = out
        .lines()
        .find(|l| l.starts_with("Token (shown only now"))
        .unwrap_or_else(|| panic!("no token line in:\n{out}"));
    line.rsplit(": ").next().unwrap().to_string()
}

fn tokens(fake: &Fake) -> Vec<serde_json::Map<String, Value>> {
    fake.sql(DB, "SELECT * FROM api_tokens ORDER BY id")
}

#[test]
fn create_prints_a_token_that_authenticates_and_is_kept_nowhere() {
    let (fake, dir) = set_up();
    let r = run(&fake, &dir, &Action::Create(create("agent")));
    ok(&r);
    let printed = printed_token(&r.out);
    assert_eq!(r.out.matches(&printed).count(), 1, "{}", r.out);

    // The Worker's own parse and hash: what it does with `Authorization: Bearer <token>`.
    let secret = ApiTokenSecret::from_authorization(&format!("Bearer {printed}"))
        .unwrap_or_else(|| panic!("not a token the Worker accepts: {printed}"));
    let hash = token::hash(&secret).unwrap();
    let independent =
        plumblog_auth::base64url::hex(&plumblog_auth::sha256::sha256(printed.as_bytes()));
    assert_eq!(hash, independent);
    let rows = tokens(&fake);
    let row = rows.last().unwrap();
    assert_eq!(row["token_hash"], hash.as_str());
    assert_eq!(row["user_id"], 1);
    assert_eq!(row["name"], "agent");
    assert_eq!(row["scope"], "writer");
    assert_eq!(row["created_at"], NOW);
    assert_eq!(row["expires_at"], Value::Null);
    assert_eq!(row["revoked_at"], Value::Null);
    // The lookup the Worker makes for a Bearer token (crates/worker d1/auth.rs find_token).
    let found = fake.sql(
        DB,
        &format!(
            "SELECT t.id FROM api_tokens t JOIN users u ON u.id = t.user_id WHERE \
             t.token_hash = '{hash}' AND t.revoked_at IS NULL AND (t.expires_at IS NULL OR \
             t.expires_at > {NOW}) AND u.disabled_at IS NULL"
        ),
    );
    assert_eq!(found.len(), 1);

    // Logged as the Worker logs it, without the token.
    let log = fake.sql(
        DB,
        "SELECT kind, actor_id, detail FROM system_log WHERE kind = 'auth.token_created'",
    );
    assert_eq!(log.len(), 1);
    let detail = log[0]["detail"].as_str().unwrap();
    for want in ["agent", "scope\twriter", "user\t1", "via\tcli"] {
        assert!(detail.contains(want), "{detail}");
    }
    assert!(!snapshot(&fake).contains(&printed), "stored nowhere");
    let file = std::fs::read_to_string(state::path(&dir.0)).unwrap();
    assert!(!file.contains(&printed), "not in the state file");
    for entry in std::fs::read_dir(&dir.0).unwrap() {
        let p = entry.unwrap().path();
        if let Ok(text) = std::fs::read_to_string(&p) {
            assert!(!text.contains(&printed), "{}", p.display());
        }
    }
    for req in fake.requests() {
        assert!(!req.body.contains(&printed), "sent to Cloudflare");
    }
}

#[test]
fn an_expiry_is_stored() {
    let (fake, dir) = set_up();
    let c = Create {
        expires_days: Some(30),
        ..create("short")
    };
    ok(&run(&fake, &dir, &Action::Create(c)));
    assert_eq!(tokens(&fake).last().unwrap()["expires_at"], NOW + 30 * DAY);

    let before = snapshot(&fake);
    let c = Create {
        expires_days: Some(0),
        ..create("zero")
    };
    assert_eq!(
        code(&run(&fake, &dir, &Action::Create(c))),
        Code::TokenExpiresInvalid
    );
    assert_eq!(snapshot(&fake), before);
}

#[test]
fn the_scope_is_capped_by_the_role() {
    let (fake, dir) = set_up();
    let before = snapshot(&fake);
    let c = Create {
        scope: "owner".into(),
        login: Some("WANDA".into()),
        ..create("too much")
    };
    let r = run(&fake, &dir, &Action::Create(c));
    assert_eq!(code(&r), Code::TokenScopeTooHigh);
    let msg = r.result.unwrap_err().message;
    assert!(msg.contains("wanda") && msg.contains("owner"), "{msg}");
    for scope in ["admin", ""] {
        let c = Create {
            scope: scope.into(),
            ..create("x")
        };
        assert_eq!(
            code(&run(&fake, &dir, &Action::Create(c))),
            Code::TokenScopeUnknown
        );
    }
    for name in ["", "  ", "a\u{7}b"] {
        assert_eq!(
            code(&run(&fake, &dir, &Action::Create(create(name)))),
            Code::TokenNameInvalid
        );
    }
    assert_eq!(snapshot(&fake), before);

    // Within the role: a writer gets writer and read tokens, the owner an owner token.
    for (login, scope) in [("wanda", "writer"), ("wanda", "read"), ("Steven", "owner")] {
        let c = Create {
            scope: scope.into(),
            login: Some(login.into()),
            ..create("ok")
        };
        ok(&run(&fake, &dir, &Action::Create(c)));
        assert_eq!(tokens(&fake).last().unwrap()["scope"], scope);
    }
}

#[test]
fn several_owners_need_login_and_unknown_users_are_refused() {
    let (fake, dir) = set_up();
    fake.exec(
        DB,
        "INSERT INTO users (id, login, slug, display_name, role, password_hash, created_at, \
         updated_at, disabled_at) VALUES (3, 'olga', 'olga', 'Olga', 'owner', 'pw', 1, 1, NULL), \
         (4, 'gone', 'gone', 'Gone', 'writer', 'pw', 1, 1, 5)",
    );
    let before = snapshot(&fake);
    let r = run(&fake, &dir, &Action::Create(create("agent")));
    assert_eq!(code(&r), Code::TokenAmbiguous);
    let msg = r.result.unwrap_err().message;
    assert!(msg.contains("Steven") && msg.contains("olga"), "{msg}");
    for login in ["nobody", "gone"] {
        let c = Create {
            login: Some(login.into()),
            ..create("agent")
        };
        assert_eq!(
            code(&run(&fake, &dir, &Action::Create(c))),
            Code::TokenNoUser
        );
    }
    assert_eq!(snapshot(&fake), before);

    let c = Create {
        login: Some("OLGA".into()),
        ..create("agent")
    };
    ok(&run(&fake, &dir, &Action::Create(c)));
    assert_eq!(tokens(&fake).last().unwrap()["user_id"], 3);
}

#[test]
fn foreign_and_unmarked_sites_are_refused() {
    let (fake, dir) = set_up();
    let before = snapshot(&fake);
    let all = [
        Action::Create(create("agent")),
        Action::List { login: None },
        Action::Revoke { id: 1, yes: true },
    ];

    // The Worker carries another folder's marker.
    let mine = state::load(&dir.0).unwrap().unwrap().script_marker;
    state::update(&dir.0, &[], |s| {
        s.script_marker = Some("someone-else".into());
    })
    .unwrap();
    for a in &all {
        let r = run(&fake, &dir, a);
        assert_eq!(code(&r), Code::AdminResetForeign);
        assert!(r.out.is_empty(), "{}", r.out);
    }
    state::update(&dir.0, &[], |s| s.script_marker = mine).unwrap();

    // The database carries another site id.
    fake.exec(
        DB,
        "UPDATE settings SET value_json = '\"other\"' WHERE key = 'site_id'",
    );
    for a in &all {
        assert_eq!(code(&run(&fake, &dir, a)), Code::AdminResetForeign);
    }
    assert_eq!(snapshot(&fake), before);
}

#[test]
fn a_worker_without_the_marker_binding_is_refused() {
    let (fake, dir) = set_up();
    {
        let mut st = fake.state();
        for v in &mut st.versions {
            if let Some(b) = v.metadata["bindings"].as_array_mut() {
                b.retain(|b| b["name"] != "PLUMBLOG_SITE_ID");
            }
        }
    }
    let before = snapshot(&fake);
    assert_eq!(
        code(&run(&fake, &dir, &Action::Create(create("agent")))),
        Code::AdminResetUnmarked
    );
    assert_eq!(snapshot(&fake), before);
}

#[test]
fn writes_hold_the_folder_lock() {
    let (fake, dir) = set_up();
    let lock = dir.0.join(plumblog_cli::deploy::LOCK_FILE);
    std::fs::write(&lock, "1").unwrap();
    let before = snapshot(&fake);
    for a in [
        Action::Create(create("agent")),
        Action::Revoke { id: 1, yes: true },
    ] {
        assert_eq!(code(&run(&fake, &dir, &a)), Code::DeployLocked);
    }
    assert_eq!(snapshot(&fake), before);
    ok(&run(&fake, &dir, &Action::List { login: None }));
    std::fs::remove_file(&lock).unwrap();
}

#[test]
fn list_shows_every_token_but_never_the_hash() {
    let (fake, dir) = set_up();
    ok(&run(&fake, &dir, &Action::Create(create("agent"))));
    fake.exec(
        DB,
        "UPDATE api_tokens SET revoked_at = 5 WHERE token_hash = 't2'",
    );
    let hashes: Vec<String> = tokens(&fake)
        .iter()
        .map(|r| r["token_hash"].as_str().unwrap().to_string())
        .collect();
    let r = run(&fake, &dir, &Action::List { login: None });
    ok(&r);
    let lines: Vec<&str> = r.out.lines().collect();
    assert_eq!(lines.len(), 3, "{}", r.out);
    assert!(
        lines[0].starts_with("1  mine  owner  Steven  created "),
        "{}",
        r.out
    );
    assert!(lines[0].ends_with("expires never  active"), "{}", r.out);
    assert!(lines[1].starts_with("2  hers  writer  wanda"), "{}", r.out);
    assert!(lines[1].contains("revoked 1970-01-01"), "{}", r.out);
    assert!(
        lines[2].starts_with("3  agent  writer  Steven"),
        "{}",
        r.out
    );
    for h in &hashes {
        assert!(!r.out.contains(h.as_str()), "hash {h} in:\n{}", r.out);
    }
    let req = fake.requests();
    let last = req.last().unwrap();
    assert!(!last.body.contains("token_hash"), "{}", last.body);

    let r = run(
        &fake,
        &dir,
        &Action::List {
            login: Some("WANDA".into()),
        },
    );
    ok(&r);
    assert_eq!(r.out.lines().count(), 1, "{}", r.out);
    let r = run(
        &fake,
        &dir,
        &Action::List {
            login: Some("nobody".into()),
        },
    );
    ok(&r);
    assert_eq!(r.out.trim(), "No API token.");
}

#[test]
fn revoke_needs_yes_then_revokes_and_logs() {
    let (fake, dir) = set_up();
    let before = snapshot(&fake);
    let r = run(&fake, &dir, &Action::Revoke { id: 2, yes: false });
    assert_eq!(code(&r), Code::TokenUnconfirmed);
    assert!(
        r.out
            .contains("Token to revoke: 2 \"hers\" (writer) of wanda."),
        "{}",
        r.out
    );
    assert_eq!(snapshot(&fake), before);

    ok(&run(&fake, &dir, &Action::Revoke { id: 2, yes: true }));
    let rows = tokens(&fake);
    assert_eq!(rows[1]["revoked_at"], NOW);
    assert_eq!(rows[0]["revoked_at"], Value::Null, "only that token");
    let log = fake.sql(
        DB,
        "SELECT detail FROM system_log WHERE kind = 'auth.token_revoked'",
    );
    assert_eq!(log.len(), 1);
    let detail = log[0]["detail"].as_str().unwrap();
    for want in ["hers", "scope\twriter", "user\t2", "via\tcli"] {
        assert!(detail.contains(want), "{detail}");
    }

    // Again, and a missing id: a clear message, nothing changes.
    let before = snapshot(&fake);
    let r = run(&fake, &dir, &Action::Revoke { id: 2, yes: true });
    ok(&r);
    assert!(r.out.contains("already revoked"), "{}", r.out);
    assert_eq!(
        code(&run(&fake, &dir, &Action::Revoke { id: 99, yes: true })),
        Code::TokenNotFound
    );
    assert_eq!(snapshot(&fake), before);
}

/// Runs the create SQL directly, as if the user changed between the reads and the write.
fn insert_now(fake: &Fake, user: &str, role: &str, hash: &str) -> usize {
    let log = token::created_log(user, "late", "writer", NOW);
    let stmts = token::create_statements(&NewRow {
        user,
        role,
        name: "late",
        hash,
        scope: "writer",
        now: &NOW.to_string(),
        expires: "",
        log: &log,
    });
    let st = fake.state();
    let conn = st.sqlite.get(DB).unwrap();
    let mut made = 0;
    for (i, (sql, params)) in stmts.iter().enumerate() {
        let mut s = conn.prepare(sql).unwrap();
        let mut rows = s.query(rusqlite::params_from_iter(params.iter())).unwrap();
        while rows.next().unwrap().is_some() {
            if i == 0 {
                made += 1;
            }
        }
    }
    made
}

#[test]
fn the_guarded_insert_creates_nothing_for_a_changed_user() {
    let (fake, dir) = set_up();
    let before = snapshot(&fake);
    // Disabled meanwhile.
    fake.exec(DB, "UPDATE users SET disabled_at = 7 WHERE id = 2");
    assert_eq!(insert_now(&fake, "2", "writer", "h-disabled"), 0);
    // Demoted meanwhile: the scope was capped against the old role.
    fake.exec(DB, "UPDATE users SET role = 'writer' WHERE id = 1");
    assert_eq!(insert_now(&fake, "1", "owner", "h-demoted"), 0);
    fake.exec(DB, "UPDATE users SET role = 'owner' WHERE id = 1");
    // `blog admin reset` landed meanwhile: its code is waiting.
    fake.exec(
        DB,
        &format!(
            "INSERT INTO settings (key, value_json, updated_at) VALUES \
             ('owner_reset_user', '1', 1), ('owner_reset_expires_at', '{}', 1)",
            NOW + 1000
        ),
    );
    assert_eq!(insert_now(&fake, "1", "owner", "h-reset"), 0);
    assert_eq!(snapshot(&fake), before, "no token, no log row");
    let r = run(&fake, &dir, &Action::Create(create("agent")));
    assert_eq!(code(&r), Code::TokenUserChanged);
    assert_eq!(snapshot(&fake), before);

    // An expired reset no longer blocks; neither does an active user with that role.
    fake.exec(
        DB,
        &format!(
            "UPDATE settings SET value_json = '{}' WHERE key = 'owner_reset_expires_at'",
            NOW - 1
        ),
    );
    assert_eq!(insert_now(&fake, "1", "owner", "h-ok"), 1);
    ok(&run(&fake, &dir, &Action::Create(create("agent"))));
}

/// tools/worker-test replays the create SQL from this file. `$user`, `$hash`, `$now` stand
/// for the values; `BLESS=1` rewrites it.
#[test]
fn the_worker_test_fixture_is_the_create_sql() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/worker-test/fixtures/token-create.json");
    let log = token::created_log("$user", "cli-agent", "writer", 0);
    let log = [
        "$now".to_string(),
        log[1].replace("-0", "-$now"),
        log[2].clone(),
        log[3].clone(),
    ];
    let stmts = token::create_statements(&NewRow {
        user: "$user",
        role: "writer",
        name: "cli-agent",
        hash: "$hash",
        scope: "writer",
        now: "$now",
        expires: "",
        log: &log,
    });
    let want = serde_json::to_string_pretty(&Value::Array(
        stmts
            .into_iter()
            .map(|(sql, params)| serde_json::json!({"sql": sql, "params": params}))
            .collect(),
    ))
    .unwrap()
        + "\n";
    if std::env::var_os("BLESS").is_some() {
        std::fs::write(&path, &want).unwrap();
    }
    let got = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        got == want,
        "{} is stale: run `BLESS=1 cargo test -p plumblog-cli --test cloudflare api_tokens::` after changing the create SQL",
        path.display()
    );
}
