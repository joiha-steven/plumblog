//! `blog admin reset`: whoever holds the Cloudflare token gets owner access back (no email
//! recovery). Why it works this way: docs/ARCHITECTURE.md, Users and auth.
//!
//! Every check reads only; the one write is a single D1 request ([`reset_statements`]).

use crate::cloudflare::Client;
use crate::d1::{self, Database};
use crate::deploy::{self, Lock, request_id};
use crate::migrations;
use crate::setup::{self, Context, Failure, MARKER_BINDING, Run, code_hash, printable};
use crate::text::Code;
use plumblog_i18n::{Key, key};
use plumblog_store::{
    OWNER_RESET_AT_KEY, OWNER_RESET_EXPIRES_KEY, OWNER_RESET_HASH_KEY, OWNER_RESET_KEEP_TOKENS_KEY,
    OWNER_RESET_USER_KEY, SITE_ID_KEY, setup_hash_json,
};
use plumblog_syslog::{Event, EventKind};
use serde_json::Value;
use std::io::Write;

/// How long a reset code works (ms): long enough to open the site, short enough that a code
/// left in a scrollback is soon worthless.
pub const RESET_CODE_TTL_MS: i64 = 60 * 60 * 1000;
/// The page that takes the code.
pub const RESET_PATH: &str = "/admin/reset";

/// What the owner asked for.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// The owner to reset; needed when the site has several.
    pub login: Option<String>,
    /// Without it: print what would happen, change nothing.
    pub yes: bool,
    /// Leave that owner's API tokens working.
    pub keep_tokens: bool,
}

/// An active owner, as read before anything changes.
#[derive(Clone)]
pub(crate) struct Owner {
    pub(crate) id: i64,
    pub(crate) login: String,
    pub(crate) role: String,
    created_at: i64,
}

fn fail(run: &Run, code: Code, k: Key, args: &[(&str, String)]) -> Failure {
    Failure::new(code, run.cx.text, k, args)
}

/// The Worker serving this folder's name carries this folder's marker, and the database
/// carries the site id `blog setup` recorded. Anything else is someone else's site. `blog
/// token` makes the same check.
pub(crate) fn check_ours(run: &Run, worker: &str, db: &Database) -> Result<(), Failure> {
    let bindings = setup::script_bindings(run.client, worker).map_err(|e| run.api(&e))?;
    let marker = bindings
        .iter()
        .find(|b| {
            b.get("type").and_then(Value::as_str) == Some("plain_text")
                && b.get("name").and_then(Value::as_str) == Some(MARKER_BINDING)
        })
        .and_then(|b| b.get("text").and_then(Value::as_str))
        .map(str::to_string);
    let marker_ok = marker.is_some() && marker == run.state.script_marker;
    if marker.is_none() {
        // An older setup, or a rollback to a version from before the marker: `blog setup`
        // records one in state, and only `blog deploy` puts it on the Worker.
        return Err(fail(
            run,
            Code::AdminResetUnmarked,
            key!("cli.error.admin_reset_unmarked"),
            &[
                ("worker", worker.to_string()),
                ("binding", MARKER_BINDING.to_string()),
            ],
        ));
    }
    migrations::check_ours(db).map_err(|e| setup::migration_error(run, e))?;
    let live: Option<String> = d1::settings(db, &[SITE_ID_KEY])
        .map_err(|e| run.api(&e))?
        .into_iter()
        .next()
        .and_then(|(_, v)| serde_json::from_str(&v).ok());
    let id_ok = live.is_some() && live == run.state.site_id;
    if marker_ok && id_ok {
        return Ok(());
    }
    Err(fail(
        run,
        Code::AdminResetForeign,
        key!("cli.error.admin_reset_foreign"),
        &[
            ("worker", worker.to_string()),
            ("binding", MARKER_BINDING.to_string()),
        ],
    ))
}

pub(crate) fn owners(run: &Run, db: &Database) -> Result<Vec<Owner>, Failure> {
    let rows = db
        .rows(
            "SELECT id, login, role, created_at FROM users WHERE role = 'owner' AND disabled_at IS NULL \
             ORDER BY id",
            &[],
        )
        .map_err(|e| run.api(&e))?;
    Ok(rows
        .iter()
        .filter_map(|r| {
            Some(Owner {
                id: r.get("id")?.as_i64()?,
                login: r.get("login")?.as_str()?.to_string(),
                role: r.get("role")?.as_str()?.to_string(),
                created_at: r.get("created_at")?.as_i64()?,
            })
        })
        .collect())
}

/// The owner to reset: the only one, or the one `--login` names (ASCII case-insensitive, as
/// the `users.login` column compares).
fn pick(run: &Run, all: &[Owner], login: Option<&str>) -> Result<Owner, Failure> {
    let list = || {
        all.iter()
            .map(|o| printable(&o.login))
            .collect::<Vec<_>>()
            .join(", ")
    };
    if all.is_empty() {
        return Err(fail(
            run,
            Code::AdminResetNoOwner,
            key!("cli.error.admin_reset_no_owner"),
            &[],
        ));
    }
    match login {
        Some(l) => {
            let names = list();
            all.iter()
                .find(|o| o.login.eq_ignore_ascii_case(l))
                .cloned()
                .ok_or_else(|| {
                    fail(
                        run,
                        Code::AdminResetNotOwner,
                        key!("cli.error.admin_reset_not_owner"),
                        &[("login", printable(l)), ("list", names)],
                    )
                })
        }
        None if all.len() > 1 => Err(fail(
            run,
            Code::AdminResetAmbiguous,
            key!("cli.error.admin_reset_ambiguous"),
            &[("list", list())],
        )),
        None => Ok(all[0].clone()),
    }
}

/// `(sessions, passkeys, active tokens, unused invites)` of the owner, for the plan.
fn counts(run: &Run, db: &Database, id: &str, now: &str) -> Result<[i64; 4], Failure> {
    let rows = db
        .rows(
            "SELECT (SELECT count(*) FROM sessions WHERE user_id = ?1) AS s, \
             (SELECT count(*) FROM passkeys WHERE user_id = ?1) AS p, \
             (SELECT count(*) FROM api_tokens WHERE user_id = ?1 AND revoked_at IS NULL \
              AND (expires_at IS NULL OR expires_at > ?2)) AS t, \
             (SELECT count(*) FROM invites WHERE created_by = ?1 AND used_at IS NULL) AS i",
            &[id, now],
        )
        .map_err(|e| run.api(&e))?;
    let n = |k: &str| {
        rows.first()
            .and_then(|r| r.get(k))
            .and_then(Value::as_i64)
            .unwrap_or(0)
    };
    Ok([n("s"), n("p"), n("t"), n("i")])
}

/// What `--yes` will do, printed before anything changes.
fn plan(run: &mut Run, opts: &Options, login: &str, origin: &str, counted: [i64; 4]) {
    let [sessions, passkeys, tokens, invites] = counted;
    run.line(
        key!("cli.admin.plan_owner"),
        &[("login", login.to_string()), ("site", origin.to_string())],
    );
    run.line(
        key!("cli.admin.plan_revoke"),
        &[
            ("sessions", sessions.to_string()),
            ("passkeys", passkeys.to_string()),
        ],
    );
    if opts.keep_tokens {
        run.line(
            key!("cli.admin.plan_keep_tokens"),
            &[("tokens", tokens.to_string())],
        );
    } else {
        run.line(
            key!("cli.admin.plan_tokens"),
            &[("tokens", tokens.to_string())],
        );
    }
    run.line(
        key!("cli.admin.plan_invites"),
        &[("invites", invites.to_string())],
    );
    run.line(
        key!("cli.admin.plan_code"),
        &[("minutes", (RESET_CODE_TTL_MS / 60_000).to_string())],
    );
}

/// The other active owners, so one an attacker added stands out. Printed in the plan and
/// again after the reset, which leaves them as they are.
fn others(run: &mut Run, all: &[Owner], picked: &Owner) {
    let others: Vec<&Owner> = all.iter().filter(|o| o.id != picked.id).collect();
    if others.is_empty() {
        run.line(key!("cli.admin.others_none"), &[]);
        return;
    }
    run.line(key!("cli.admin.others_head"), &[]);
    for o in others {
        run.line(
            key!("cli.admin.others_item"),
            &[
                ("login", printable(&o.login)),
                ("role", o.role.clone()),
                ("created", deploy::utc(o.created_at).0),
            ],
        );
    }
}

/// One system log row, as crates/worker d1/log.rs writes them (no actor: the CLI did it).
const LOG_SQL: &str = "INSERT INTO system_log (at_ms, request_id, kind, actor_id, error_code, \
                       detail) VALUES (?, ?, ?, NULL, NULL, ?)";

/// The values of the reset write, as D1 parameters (text).
pub struct Reset<'a> {
    pub user: &'a str,
    pub now: &'a str,
    pub hash_json: &'a str,
    pub expires: &'a str,
    pub keep_tokens: bool,
    pub log: &'a [String; 4],
}

/// The `auth.owner_reset` log row: `at_ms, request_id, kind, detail` (never the code).
#[must_use]
pub fn reset_log(user: &str, keep_tokens: bool, now_ms: i64) -> [String; 4] {
    let revoked = if keep_tokens {
        "password,passkeys,sessions,invites"
    } else {
        "password,passkeys,sessions,invites,tokens"
    };
    let event = Event::new(
        EventKind::OwnerReset,
        request_id("admin-reset", now_ms),
        u64::try_from(now_ms).unwrap_or(0),
    )
    .subject("owner reset")
    .field("user", user)
    .field("revoked", revoked)
    .field("via", "cli");
    [
        event.at_ms().to_string(),
        event.request_id().as_str().to_string(),
        event.kind().as_str().to_string(),
        event.detail(),
    ]
}

/// The one D1 request of a reset, in order, as `(sql, params)`. Ordered so any prefix leaves
/// the account more locked, never more open (the REST query is not documented atomic). A
/// Worker request that authenticated before it creates no credential after it (store
/// `Proof`); one that wrote between its statements is undone by the redeem, which removes
/// every passkey, session and (per `owner_reset_keep_tokens`) token of the owner, using the
/// reset time and choice stored here. Public so tools/worker-test replays this exact SQL
/// (crates/cli/tests/admin writes it to a fixture).
#[must_use]
pub fn reset_statements(r: &Reset) -> Vec<(&'static str, Vec<String>)> {
    let s = |v: &[&str]| v.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
    // NULL never verifies: sign-in answers it like a wrong password, at the same cost
    // (api/auth.rs `password_matches`), where an unparseable hash would be a 500.
    let mut stmts = vec![
        (
            "UPDATE users SET password_hash = NULL, updated_at = ? WHERE id = ?",
            s(&[r.now, r.user]),
        ),
        ("DELETE FROM passkeys WHERE user_id = ?", s(&[r.user])),
        ("DELETE FROM sessions WHERE user_id = ?", s(&[r.user])),
        // Every unused invite they made, of any role and however old: one an attacker made
        // before the reset would otherwise still let someone in.
        (
            "DELETE FROM invites WHERE created_by = ? AND used_at IS NULL",
            s(&[r.user]),
        ),
    ];
    if !r.keep_tokens {
        stmts.push((
            "UPDATE api_tokens SET revoked_at = ? WHERE user_id = ? AND revoked_at IS NULL",
            s(&[r.now, r.user]),
        ));
    }
    // Hash first: a request cut short leaves a code nobody was shown, and the previous code
    // stops working at once.
    let keep = if r.keep_tokens { "true" } else { "false" };
    for (k, v) in [
        (OWNER_RESET_HASH_KEY, r.hash_json),
        (OWNER_RESET_USER_KEY, r.user),
        (OWNER_RESET_EXPIRES_KEY, r.expires),
        (OWNER_RESET_AT_KEY, r.now),
        (OWNER_RESET_KEEP_TOKENS_KEY, keep),
    ] {
        stmts.push((d1::PUT_OWNER_RESET, s(&[k, v, r.now])));
    }
    stmts.push((LOG_SQL, r.log.to_vec()));
    stmts
}

/// `blog admin reset`.
///
/// # Errors
/// [`Failure`] with a stable code; `admin.reset_unconfirmed` (no `--yes`) after the plan.
pub fn run(
    client: &mut Client,
    account_id: Option<&str>,
    opts: &Options,
    cx: &Context,
    out: &mut dyn Write,
) -> Result<(), Failure> {
    let _lock = Lock::take(cx)?;
    let (st, site) = deploy::start(client, account_id, cx)?;
    let client: &Client = client;
    let mut run = Run {
        client,
        cx,
        out,
        state: st,
        secrets: Vec::new(),
    };
    let db = Database::new(client, site.db_id.clone());

    // Read only: is this our site, and which owner.
    check_ours(&run, &site.worker, &db)?;
    let all = owners(&run, &db)?;
    let owner = pick(&run, &all, opts.login.as_deref())?;
    let id = owner.id.to_string();
    let now = cx.now_ms.to_string();
    let counted = counts(&run, &db, &id, &now)?;
    let login = printable(&owner.login);
    plan(&mut run, opts, &login, &site.origin, counted);
    others(&mut run, &all, &owner);
    if !opts.yes {
        return Err(fail(
            &run,
            Code::AdminResetUnconfirmed,
            key!("cli.error.admin_reset_unconfirmed"),
            &[],
        ));
    }

    // The one write.
    let code = run.random(setup::new_setup_code())?;
    let hash = setup_hash_json(&code_hash(&code));
    let expires = (cx.now_ms + RESET_CODE_TTL_MS).to_string();
    let log = reset_log(&id, opts.keep_tokens, cx.now_ms);
    let stmts = reset_statements(&Reset {
        user: &id,
        now: &now,
        hash_json: &hash,
        expires: &expires,
        keep_tokens: opts.keep_tokens,
        log: &log,
    });
    let stmts: Vec<(&str, Vec<&str>)> = stmts
        .iter()
        .map(|(sql, v)| (*sql, v.iter().map(String::as_str).collect()))
        .collect();
    let stmts: Vec<(&str, &[&str])> = stmts.iter().map(|(sql, v)| (*sql, &v[..])).collect();
    db.batch(&stmts).map_err(|e| run.api(&e))?;

    // Shown once; never written to the state file or a log.
    run.line(key!("cli.admin.done"), &[("login", login)]);
    if opts.keep_tokens {
        run.line(key!("cli.admin.tokens_kept"), &[]);
    }
    run.line(
        key!("cli.admin.code"),
        &[
            ("code", code),
            ("minutes", (RESET_CODE_TTL_MS / 60_000).to_string()),
        ],
    );
    run.line(
        key!("cli.admin.code_next"),
        &[("url", format!("{}{RESET_PATH}", site.origin))],
    );
    others(&mut run, &all, &owner);
    Ok(())
}
