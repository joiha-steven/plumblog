//! `blog token`: API tokens for whoever holds the Cloudflare token, so an agent gets a `plb_`
//! token without the browser. Why it works this way: docs/ARCHITECTURE.md, Users and auth.
//!
//! The token text is printed once, on stdout, and kept nowhere: not in the state file, the
//! system log or an error. Only its SHA-256 is stored, made by the Worker's own functions.

use crate::admin;
use crate::cloudflare::Client;
use crate::d1::Database;
use crate::deploy::{self, Lock, request_id};
use crate::setup::{Context, Failure, Run, printable};
use crate::text::Code;
use plumblog_auth::webauthn::{Es256Key, PublicKey, Rs256Key};
use plumblog_auth::{Crypto, api_token};
use plumblog_i18n::{Key, key};
use plumblog_model::{Error, ErrorCode, Role, TokenScope};
use plumblog_store::OWNER_RESET_EXPIRES_KEY;
use plumblog_store::OWNER_RESET_USER_KEY;
use plumblog_syslog::{Event, EventKind};
use serde_json::{Map, Value};
use std::future::ready;
use std::io::Write;

const DAY_MS: i64 = 86_400_000;

/// `blog token create`.
#[derive(Clone, Debug)]
pub struct Create {
    pub name: String,
    /// `read`, `writer` or `owner`.
    pub scope: String,
    /// The user the token belongs to; default the only active owner.
    pub login: Option<String>,
    /// `--expires` as typed: days until it stops working; `None` never.
    pub expires: Option<String>,
}

/// What `blog token` was asked to do.
#[derive(Clone, Debug)]
pub enum Action {
    Create(Create),
    List { login: Option<String> },
    Revoke { id: i64, yes: bool },
}

fn fail(run: &Run, code: Code, k: Key, args: &[(&str, String)]) -> Failure {
    Failure::new(code, run.cx.text, k, args)
}

fn internal(run: &Run, detail: &str) -> Failure {
    fail(
        run,
        Code::Internal,
        key!("cli.error.internal"),
        &[("detail", detail.to_string())],
    )
}

/// The OS random source and the pure SHA-256, so [`api_token::new_token`] makes the token
/// exactly as the Worker does. The rest of [`Crypto`] is never called here.
struct OsCrypto;

fn unused() -> Error {
    Error::new(ErrorCode::Internal, "not used by blog token")
}

type Res<T> = plumblog_model::Result<T>;

impl Crypto for OsCrypto {
    fn pbkdf2_sha256(&self, _: &[u8], _: &[u8], _: u32) -> impl Future<Output = Res<[u8; 32]>> {
        ready(Err(unused()))
    }
    fn random_bytes(&self, buf: &mut [u8]) -> Res<()> {
        getrandom::getrandom(buf)
            .map_err(|e| Error::new(ErrorCode::Internal, format!("random source: {e}")))
    }
    fn sha256(&self, data: &[u8]) -> impl Future<Output = Res<[u8; 32]>> {
        ready(Ok(plumblog_auth::sha256::sha256(data)))
    }
    fn verify_es256(
        &self,
        _: &Es256Key,
        _: &[u8],
        _: &[u8; 64],
    ) -> impl Future<Output = Res<bool>> {
        ready(Err(unused()))
    }
    fn verify_rs256(&self, _: &Rs256Key, _: &[u8], _: &[u8]) -> impl Future<Output = Res<bool>> {
        ready(Err(unused()))
    }
    fn verify_ed25519(
        &self,
        _: &[u8; 32],
        _: &[u8],
        _: &[u8; 64],
    ) -> impl Future<Output = Res<bool>> {
        ready(Err(unused()))
    }
    fn validate_public_key(&self, _: &PublicKey) -> impl Future<Output = Res<()>> {
        ready(Err(unused()))
    }
}

/// [`OsCrypto`] never waits, so one poll finishes its futures.
fn finish<T>(f: impl Future<Output = Res<T>>) -> Res<T> {
    let mut f = std::pin::pin!(f);
    match f
        .as_mut()
        .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
    {
        std::task::Poll::Ready(r) => r,
        std::task::Poll::Pending => Err(Error::new(ErrorCode::Internal, "crypto future pending")),
    }
}

/// The `api_tokens.token_hash` of `token`, as the Worker computes it when the token is used.
///
/// # Errors
/// Never in practice (the pure SHA-256 cannot fail).
pub fn hash(token: &api_token::ApiTokenSecret) -> plumblog_model::Result<String> {
    finish(api_token::hash_token(&OsCrypto, token))
}

/// The values of a token insert, as D1 parameters (text).
pub struct NewRow<'a> {
    pub user: &'a str,
    /// The role read before the write: the insert lands only if the user still has it.
    pub role: &'a str,
    pub name: &'a str,
    pub hash: &'a str,
    pub scope: &'a str,
    pub now: &'a str,
    /// Empty for no expiry.
    pub expires: &'a str,
    pub log: &'a [String; 4],
}

/// The `auth.token_created` log row: `at_ms, request_id, kind, detail` (never the token).
#[must_use]
pub fn created_log(user: &str, name: &str, scope: &str, now_ms: i64) -> [String; 4] {
    log_row(
        EventKind::TokenCreated,
        "token-create",
        user,
        name,
        scope,
        now_ms,
    )
}

fn log_row(
    kind: EventKind,
    what: &str,
    user: &str,
    name: &str,
    scope: &str,
    now_ms: i64,
) -> [String; 4] {
    // The Worker's fields (subject = name, scope); no actor, so the user and `via` say whose
    // token and who did it, as `blog admin reset` logs.
    let event = Event::new(
        kind,
        request_id(what, now_ms),
        u64::try_from(now_ms).unwrap_or(0),
    )
    .subject(name)
    .field("scope", scope)
    .field("user", user)
    .field("via", "cli");
    [
        event.at_ms().to_string(),
        event.request_id().as_str().to_string(),
        event.kind().as_str().to_string(),
        event.detail(),
    ]
}

/// The one D1 request of `blog token create`, as `(sql, params)`. The insert lands only if,
/// at that moment, the user is still active with the role the scope was capped against and
/// has no owner reset waiting (a reset that landed after the reads would otherwise leave a
/// fresh token behind); `RETURNING id` is empty when it did not land. The log row lands only
/// with the token. Public so tools/worker-test replays this exact SQL (crates/cli/tests/token
/// writes it to a fixture).
#[must_use]
pub fn create_statements(r: &NewRow) -> Vec<(&'static str, Vec<String>)> {
    let s = |v: &[&str]| v.iter().map(|x| (*x).to_string()).collect::<Vec<_>>();
    vec![
        (
            "INSERT INTO api_tokens (user_id, name, token_hash, scope, created_at, expires_at, \
             revoked_at) SELECT ?1, ?2, ?3, ?4, ?5, NULLIF(?6, ''), NULL \
             WHERE EXISTS (SELECT 1 FROM users WHERE id = ?1 AND role = ?7 AND disabled_at IS NULL) \
             AND NOT EXISTS (SELECT 1 FROM settings u, settings e WHERE u.key = ?8 \
             AND u.value_json = ?1 AND e.key = ?9 AND CAST(e.value_json AS INTEGER) > ?5) \
             RETURNING id",
            s(&[
                r.user,
                r.name,
                r.hash,
                r.scope,
                r.now,
                r.expires,
                r.role,
                OWNER_RESET_USER_KEY,
                OWNER_RESET_EXPIRES_KEY,
            ]),
        ),
        (
            "INSERT INTO system_log (at_ms, request_id, kind, actor_id, error_code, detail) \
             SELECT ?1, ?2, ?3, NULL, NULL, ?4 \
             WHERE EXISTS (SELECT 1 FROM api_tokens WHERE token_hash = ?5)",
            [r.log.to_vec(), s(&[r.hash])].concat(),
        ),
    ]
}

fn batch(
    run: &Run,
    db: &Database,
    stmts: &[(&'static str, Vec<String>)],
) -> Result<Vec<crate::d1::StatementResult>, Failure> {
    let stmts: Vec<(&str, Vec<&str>)> = stmts
        .iter()
        .map(|(sql, v)| (*sql, v.iter().map(String::as_str).collect()))
        .collect();
    let stmts: Vec<(&str, &[&str])> = stmts.iter().map(|(sql, v)| (*sql, &v[..])).collect();
    db.batch(&stmts).map_err(|e| run.api(&e))
}

/// An active user, as read before anything changes.
struct User {
    id: i64,
    login: String,
    role: String,
}

/// The token's user: the one `--login` names (any active user; `users.login` compares ASCII
/// case-insensitively), else the only active owner.
fn user(run: &Run, db: &Database, login: Option<&str>) -> Result<User, Failure> {
    if let Some(l) = login {
        let rows = db
            .rows(
                "SELECT id, login, role FROM users WHERE login = ? AND disabled_at IS NULL",
                &[l],
            )
            .map_err(|e| run.api(&e))?;
        let found = rows.first().and_then(|r| {
            Some(User {
                id: r.get("id")?.as_i64()?,
                login: r.get("login")?.as_str()?.to_string(),
                role: r.get("role")?.as_str()?.to_string(),
            })
        });
        return found.ok_or_else(|| {
            fail(
                run,
                Code::TokenNoUser,
                key!("cli.error.token_no_user"),
                &[("login", printable(l))],
            )
        });
    }
    let all = admin::owners(run, db)?;
    match &all[..] {
        [] => Err(fail(
            run,
            Code::TokenNoOwner,
            key!("cli.error.token_no_owner"),
            &[],
        )),
        [o] => Ok(User {
            id: o.id,
            login: o.login.clone(),
            role: o.role.clone(),
        }),
        _ => Err(fail(
            run,
            Code::TokenAmbiguous,
            key!("cli.error.token_ambiguous"),
            &[(
                "list",
                all.iter()
                    .map(|o| printable(&o.login))
                    .collect::<Vec<_>>()
                    .join(", "),
            )],
        )),
    }
}

/// Longest `--expires`, in days (100 years): past it, "never" says it better.
pub const MAX_EXPIRES_DAYS: i64 = 36_500;

/// What `create` was asked, checked before the lock and any network call, so a typo costs
/// nothing: the scope and the expiry time (`None` never).
#[derive(Clone, Copy)]
struct Checked {
    scope: TokenScope,
    expires: Option<i64>,
}

fn check(c: &Create, cx: &Context) -> Result<Checked, Failure> {
    let fail = |code, k, args: &[(&str, String)]| Failure::new(code, cx.text, k, args);
    api_token::check_name(&c.name).map_err(|e| {
        fail(
            Code::TokenNameInvalid,
            key!("cli.error.token_name_invalid"),
            &[("detail", e.detail)],
        )
    })?;
    let scope = TokenScope::parse(&c.scope).ok_or_else(|| {
        fail(
            Code::TokenScopeUnknown,
            key!("cli.error.token_scope_unknown"),
            &[
                ("scope", printable(&c.scope)),
                (
                    "list",
                    TokenScope::ALL
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", "),
                ),
            ],
        )
    })?;
    // Parsed here, not by clap, so every bad value (negative, huge, not a number) gets the
    // translated message.
    let expires = match c.expires.as_deref() {
        None => None,
        Some(v) => {
            let days = (!v.is_empty() && v.len() <= 6 && v.bytes().all(|b| b.is_ascii_digit()))
                .then(|| v.parse::<i64>().ok())
                .flatten()
                .filter(|d| (1..=MAX_EXPIRES_DAYS).contains(d))
                .ok_or_else(|| {
                    fail(
                        Code::TokenExpiresInvalid,
                        key!("cli.error.token_expires_invalid"),
                        &[
                            ("value", printable(v)),
                            ("max", MAX_EXPIRES_DAYS.to_string()),
                        ],
                    )
                })?;
            Some(cx.now_ms + days * DAY_MS)
        }
    };
    Ok(Checked { scope, expires })
}

fn create(run: &mut Run, db: &Database, c: &Create, checked: &Checked) -> Result<(), Failure> {
    let Checked { scope, expires } = *checked;
    let u = user(run, db, c.login.as_deref())?;
    let login = printable(&u.login);
    let role =
        Role::parse(&u.role).ok_or_else(|| internal(run, &format!("users.role {:?}", u.role)))?;
    // The Worker caps the scope at use time too; refusing here keeps the row honest.
    if scope > TokenScope::from(role) {
        return Err(fail(
            run,
            Code::TokenScopeTooHigh,
            key!("cli.error.token_scope_too_high"),
            &[
                ("login", login),
                ("role", role.as_str().to_string()),
                ("scope", scope.as_str().to_string()),
            ],
        ));
    }

    let (secret, hash) =
        finish(api_token::new_token(&OsCrypto)).map_err(|e| internal(run, &e.detail))?;
    let id = u.id.to_string();
    let now = run.cx.now_ms.to_string();
    let log = created_log(&id, &c.name, scope.as_str(), run.cx.now_ms);
    let stmts = create_statements(&NewRow {
        user: &id,
        role: role.as_str(),
        name: &c.name,
        hash: &hash,
        scope: scope.as_str(),
        now: &now,
        expires: &expires.map(|e| e.to_string()).unwrap_or_default(),
        log: &log,
    });
    let results = batch(run, db, &stmts)?;
    let Some(made) = results
        .first()
        .and_then(|r| r.results.first())
        .and_then(|r| r.get("id"))
        .and_then(Value::as_i64)
    else {
        return Err(fail(
            run,
            Code::TokenUserChanged,
            key!("cli.error.token_user_changed"),
            &[("login", login)],
        ));
    };
    run.line(
        key!("cli.token.created"),
        &[
            ("id", made.to_string()),
            ("name", printable(&c.name)),
            ("scope", scope.as_str().to_string()),
            ("login", login),
            ("expires", expiry(run, expires)),
        ],
    );
    // Shown once; never written to the state file or a log.
    run.line(
        key!("cli.token.secret"),
        &[("token", secret.as_str().to_string())],
    );
    run.line(key!("cli.token.keep"), &[]);
    Ok(())
}

fn expiry(run: &Run, at: Option<i64>) -> String {
    at.map_or_else(
        || run.t(key!("cli.token.never"), &[]),
        |at| deploy::utc(at).0,
    )
}

const LIST_SQL: &str = "SELECT t.id, t.user_id, t.name, t.scope, u.login, t.created_at, t.expires_at, \
                        t.revoked_at FROM api_tokens t JOIN users u ON u.id = t.user_id";

fn int(r: &Map<String, Value>, k: &str) -> Option<i64> {
    r.get(k).and_then(Value::as_i64)
}

fn text<'a>(r: &'a Map<String, Value>, k: &str) -> &'a str {
    r.get(k).and_then(Value::as_str).unwrap_or_default()
}

fn list(run: &mut Run, db: &Database, login: Option<&str>) -> Result<(), Failure> {
    // Never the hash: it is not even read.
    let rows = match login {
        Some(l) => db.rows(&format!("{LIST_SQL} WHERE u.login = ? ORDER BY t.id"), &[l]),
        None => db.rows(&format!("{LIST_SQL} ORDER BY t.id"), &[]),
    }
    .map_err(|e| run.api(&e))?;
    if rows.is_empty() {
        run.line(key!("cli.token.list_none"), &[]);
        return Ok(());
    }
    for r in &rows {
        let revoked = int(r, "revoked_at").map_or_else(
            || run.t(key!("cli.token.list_active"), &[]),
            |at| {
                run.t(
                    key!("cli.token.list_revoked"),
                    &[("date", deploy::utc(at).0)],
                )
            },
        );
        let line = run.t(
            key!("cli.token.list_item"),
            &[
                ("id", int(r, "id").unwrap_or_default().to_string()),
                ("name", printable(text(r, "name"))),
                ("scope", printable(text(r, "scope"))),
                ("login", printable(text(r, "login"))),
                (
                    "created",
                    deploy::utc(int(r, "created_at").unwrap_or_default()).0,
                ),
                ("expires", expiry(run, int(r, "expires_at"))),
                ("revoked", revoked),
            ],
        );
        let _ = writeln!(run.out, "{line}");
    }
    Ok(())
}

/// The one D1 request of `blog token revoke --yes`: the revoke, and its log row only if this
/// request is the one that revoked it.
#[must_use]
pub fn revoke_statements(
    id: &str,
    now: &str,
    log: &[String; 4],
) -> Vec<(&'static str, Vec<String>)> {
    vec![
        (
            "UPDATE api_tokens SET revoked_at = ?1 WHERE id = ?2 AND revoked_at IS NULL",
            vec![now.to_string(), id.to_string()],
        ),
        (
            "INSERT INTO system_log (at_ms, request_id, kind, actor_id, error_code, detail) \
             SELECT ?1, ?2, ?3, NULL, NULL, ?4 \
             WHERE EXISTS (SELECT 1 FROM api_tokens WHERE id = ?5 AND revoked_at = ?1)",
            [log.to_vec(), vec![id.to_string()]].concat(),
        ),
    ]
}

fn revoke(run: &mut Run, db: &Database, id: i64, yes: bool) -> Result<(), Failure> {
    let ids = id.to_string();
    let row = db
        .rows(&format!("{LIST_SQL} WHERE t.id = ?"), &[ids.as_str()])
        .map_err(|e| run.api(&e))?
        .into_iter()
        .next()
        .ok_or_else(|| {
            fail(
                run,
                Code::TokenNotFound,
                key!("cli.error.token_not_found"),
                &[("id", ids.clone())],
            )
        })?;
    let args = [
        ("id", ids.clone()),
        ("name", printable(text(&row, "name"))),
        ("scope", printable(text(&row, "scope"))),
        ("login", printable(text(&row, "login"))),
    ];
    if let Some(at) = int(&row, "revoked_at") {
        run.line(
            key!("cli.token.already_revoked"),
            &[("id", ids), ("date", deploy::utc(at).0)],
        );
        return Ok(());
    }
    run.line(key!("cli.token.plan_revoke"), &args);
    if !yes {
        return Err(fail(
            run,
            Code::TokenUnconfirmed,
            key!("cli.error.token_unconfirmed"),
            &[],
        ));
    }
    let now = run.cx.now_ms.to_string();
    let user = int(&row, "user_id").unwrap_or_default().to_string();
    let log = log_row(
        EventKind::TokenRevoked,
        "token-revoke",
        &user,
        text(&row, "name"),
        text(&row, "scope"),
        run.cx.now_ms,
    );
    let results = batch(run, db, &revoke_statements(&ids, &now, &log))?;
    if results
        .first()
        .map_or(0, crate::d1::StatementResult::changes)
        == 0
    {
        // Revoked by someone else between the read and the write: the end state is the same.
        run.line(
            key!("cli.token.already_revoked"),
            &[
                ("id", ids),
                ("date", run.t(key!("cli.token.just_now"), &[])),
            ],
        );
        return Ok(());
    }
    run.line(key!("cli.token.revoked"), &args);
    Ok(())
}

/// `blog token create | list | revoke`.
///
/// # Errors
/// [`Failure`] with a stable code. Every refusal changes nothing.
pub fn run(
    client: &mut Client,
    account_id: Option<&str>,
    action: &Action,
    cx: &Context,
    out: &mut dyn Write,
) -> Result<(), Failure> {
    let checked = match action {
        Action::Create(c) => Some(check(c, cx)?),
        _ => None,
    };
    // Writes hold the folder lock, as `blog admin reset` does; a list only reads.
    let _lock = match action {
        Action::List { .. } => None,
        _ => Some(Lock::take(cx)?),
    };
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
    admin::check_ours(&run, &site.worker, &db)?;
    match action {
        Action::Create(c) => match &checked {
            Some(k) => create(&mut run, &db, c, k),
            None => Err(internal(&run, "create without its checks")),
        },
        Action::List { login } => list(&mut run, &db, login.as_deref()),
        Action::Revoke { id, yes } => revoke(&mut run, &db, *id, *yes),
    }
}
