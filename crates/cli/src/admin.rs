//! `blog admin reset`: whoever holds the Cloudflare token gets owner access back (no email
//! recovery). Why it works this way: docs/ARCHITECTURE.md, Users and auth.
//!
//! Every check reads only; the one write is a single D1 request whose statements are ordered
//! so that any prefix of it leaves the account more locked, never more open (the REST query
//! is not documented as atomic): credentials first, then sessions (a session can no longer
//! be minted), then tokens, then the new code, then the log row.

use crate::cloudflare::Client;
use crate::d1::{self, Database};
use crate::deploy::{self, Lock, at_ms, request_id};
use crate::migrations;
use crate::setup::{self, Context, Failure, MARKER_BINDING, Run, code_hash, printable};
use crate::text::Code;
use plumblog_i18n::{Key, key};
use plumblog_store::{
    OWNER_RESET_EXPIRES_KEY, OWNER_RESET_HASH_KEY, OWNER_RESET_USER_KEY, SITE_ID_KEY,
    setup_hash_json,
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
struct Owner {
    id: i64,
    login: String,
}

fn fail(run: &Run, code: Code, k: Key, args: &[(&str, String)]) -> Failure {
    Failure::new(code, run.cx.text, k, args)
}

/// The Worker serving this folder's name carries this folder's marker, and the database
/// carries the site id `blog setup` recorded. Anything else is someone else's site.
fn check_ours(run: &Run, worker: &str, db: &Database) -> Result<(), Failure> {
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

fn owners(run: &Run, db: &Database) -> Result<Vec<Owner>, Failure> {
    let rows = db
        .rows(
            "SELECT id, login FROM users WHERE role = 'owner' AND disabled_at IS NULL \
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
            })
        })
        .collect())
}

/// The owner to reset: the only one, or the one `--login` names (ASCII case-insensitive, as
/// the `users.login` column compares).
fn pick(run: &Run, all: Vec<Owner>, login: Option<&str>) -> Result<Owner, Failure> {
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
            all.into_iter()
                .find(|o| o.login.eq_ignore_ascii_case(l))
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
        None => Ok(all.into_iter().next().expect("one owner")),
    }
}

/// `(sessions, passkeys, active tokens)` of the owner, for the plan.
fn counts(run: &Run, db: &Database, id: &str, now: &str) -> Result<[i64; 3], Failure> {
    let rows = db
        .rows(
            "SELECT (SELECT count(*) FROM sessions WHERE user_id = ?1) AS s, \
             (SELECT count(*) FROM passkeys WHERE user_id = ?1) AS p, \
             (SELECT count(*) FROM api_tokens WHERE user_id = ?1 AND revoked_at IS NULL \
              AND (expires_at IS NULL OR expires_at > ?2)) AS t",
            &[id, now],
        )
        .map_err(|e| run.api(&e))?;
    let n = |k: &str| {
        rows.first()
            .and_then(|r| r.get(k))
            .and_then(Value::as_i64)
            .unwrap_or(0)
    };
    Ok([n("s"), n("p"), n("t")])
}

/// What `--yes` will do, printed before anything changes.
fn plan(run: &mut Run, opts: &Options, login: &str, origin: &str, counted: [i64; 3]) {
    let [sessions, passkeys, tokens] = counted;
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
        key!("cli.admin.plan_code"),
        &[("minutes", (RESET_CODE_TTL_MS / 60_000).to_string())],
    );
}

/// One system log row, as crates/worker d1/log.rs writes them (no actor: the CLI did it).
const LOG_SQL: &str = "INSERT INTO system_log (at_ms, request_id, kind, actor_id, error_code, \
                       detail) VALUES (?, ?, ?, NULL, NULL, ?)";

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
    let owner = pick(&run, owners(&run, &db)?, opts.login.as_deref())?;
    let id = owner.id.to_string();
    let now = cx.now_ms.to_string();
    let counted = counts(&run, &db, &id, &now)?;
    let login = printable(&owner.login);
    plan(&mut run, opts, &login, &site.origin, counted);
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
    let revoked = if opts.keep_tokens {
        "password,passkeys,sessions"
    } else {
        "password,passkeys,sessions,tokens"
    };
    let event = Event::new(
        EventKind::OwnerReset,
        request_id("admin-reset", cx.now_ms),
        at_ms(cx),
    )
    .subject("owner reset")
    .field("user", &id)
    .field("revoked", revoked)
    .field("via", "cli");
    let log = [
        event.at_ms().to_string(),
        event.request_id().as_str().to_string(),
        event.kind().as_str().to_string(),
        event.detail(),
    ];
    let log: Vec<&str> = log.iter().map(String::as_str).collect();
    // NULL never verifies: sign-in answers it like a wrong password, at the same cost
    // (api/auth.rs `password_matches`), where an unparseable hash would be a 500.
    let (n, u) = (now.as_str(), id.as_str());
    let at_user = [n, u];
    let user = [u];
    let put_hash = [OWNER_RESET_HASH_KEY, hash.as_str(), n];
    let put_user = [OWNER_RESET_USER_KEY, u, n];
    let put_expires = [OWNER_RESET_EXPIRES_KEY, expires.as_str(), n];
    let mut stmts: Vec<(&str, &[&str])> = vec![
        (
            "UPDATE users SET password_hash = NULL, updated_at = ? WHERE id = ?",
            &at_user,
        ),
        ("DELETE FROM passkeys WHERE user_id = ?", &user),
        ("DELETE FROM sessions WHERE user_id = ?", &user),
    ];
    if !opts.keep_tokens {
        stmts.push((
            "UPDATE api_tokens SET revoked_at = ? WHERE user_id = ? AND revoked_at IS NULL",
            &at_user,
        ));
    }
    // Hash first: a request cut short leaves a code nobody was shown, and the previous code
    // stops working at once.
    stmts.extend([
        (d1::PUT_OWNER_RESET, &put_hash[..]),
        (d1::PUT_OWNER_RESET, &put_user[..]),
        (d1::PUT_OWNER_RESET, &put_expires[..]),
        (LOG_SQL, &log[..]),
    ]);
    db.batch(&stmts).map_err(|e| run.api(&e))?;

    // Shown once; never written to the state file or a log.
    run.line(key!("cli.admin.done"), &[("login", login)]);
    if opts.keep_tokens {
        run.line(key!("cli.admin.tokens_kept"), &[]);
    }
    run.line(key!("cli.admin.code"), &[("code", code)]);
    run.line(
        key!("cli.admin.code_next"),
        &[("url", format!("{}{RESET_PATH}", site.origin))],
    );
    Ok(())
}
