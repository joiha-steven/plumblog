//! `blog setup`: creates or finds everything a blog needs on Cloudflare. Idempotent: every
//! step looks up by name before creating, and `site.local.toml` is written after each step,
//! so a re-run after any failure resumes and never duplicates (docs/ARCHITECTURE.md,
//! Site-specific state).

use crate::cloudflare::{ApiError, ApiToken, Call, Client, Endpoint, TokenError, token};
use crate::d1::{self, Database};
use crate::migrations::{self, Migration};
use crate::state::{self, D1State, FailedMigration, R2State, SiteState, StateError};
use crate::text::{Code, Text};
use plumblog_i18n::{Key, key};
use plumblog_store::{ORIGIN_KEY, SITE_ID_KEY, SITE_LANG_KEY, TURNSTILE_SITE_KEY};
use serde::Deserialize;
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::io::Write;
use std::path::Path;

/// Worker name when neither `--name` nor `site.local.toml` gives one.
pub const DEFAULT_NAME: &str = "plumblog";
/// How to restore the database to a bookmark (`blog rollback --db`).
pub const ROLLBACK_COMMAND: &str = "blog rollback --db {bookmark} --yes";
/// Longest worker name: `<name>-media` must stay a valid R2 bucket name (63).
const MAX_NAME: usize = 50;
/// Setup code lifetime, as the Worker checks it (`setup_code_expires_at`, ms).
const SETUP_CODE_TTL_MS: i64 = 24 * 60 * 60 * 1000;
/// Same size as `SITE_ID_BYTES` in crates/worker/src/api/auth.rs.
const SITE_ID_BYTES: usize = 16;
/// `WEBHOOK_KEY`: base64 of this many random bytes (`plumblog_webhook::KEY_LEN`).
const WEBHOOK_KEY_BYTES: usize = 32;
pub const WEBHOOK_KEY: &str = "WEBHOOK_KEY";
pub const TURNSTILE_SECRET: &str = "TURNSTILE_SECRET";
/// Cloudflare's "script not found" (wrangler: `WORKER_NOT_FOUND_ERR_CODE`).
const SCRIPT_NOT_FOUND: i64 = 10007;
/// Readable setup code alphabet: no 0/O, 1/I. 32 letters, so one byte & 31 is uniform.
const CODE_ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const CODE_GROUPS: usize = 4;
const CODE_GROUP_LEN: usize = 5;
/// Placeholder Worker until `blog deploy`: 503, no body, so secrets can attach to a script.
const PLACEHOLDER_JS: &str =
    "export default { fetch() { return new Response(null, { status: 503 }); } };\n";
/// Plain-text binding that marks a script as this site's (value: `script_marker` in state).
/// A binding, not a script tag: the upload metadata docs and wrangler 4.160.0 send no tags,
/// and a binding is kept on every version because `blog deploy` sends it again.
pub const MARKER_BINDING: &str = "PLUMBLOG_SITE_ID";
/// Random bytes of a new marker.
const MARKER_BYTES: usize = 16;

/// Turnstile, as asked on the command line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Turnstile {
    #[default]
    On,
    /// `--no-turnstile` alone: refused.
    OffUnconfirmed,
    /// `--no-turnstile --yes-i-understand`.
    Off,
}

impl Turnstile {
    #[must_use]
    pub fn from_flags(no_turnstile: bool, yes_i_understand: bool) -> Self {
        match (no_turnstile, yes_i_understand) {
            (false, _) => Self::On,
            (true, false) => Self::OffUnconfirmed,
            (true, true) => Self::Off,
        }
    }
}

/// What the owner asked for on the command line.
#[derive(Clone, Debug, Default)]
pub struct Options {
    pub name: Option<String>,
    pub domain: Option<String>,
    pub turnstile: Turnstile,
    /// Allows replacing an origin already stored in the database with a computed one.
    pub change_origin: bool,
    /// Forgets a recorded failed migration (after restoring by hand) so migrations run again.
    pub clear_failed_migration: bool,
}

/// Everything a run needs besides the API.
pub struct Context<'a> {
    /// Where `site.local.toml` lives.
    pub dir: &'a Path,
    pub migrations: &'a [Migration],
    /// Unix time in milliseconds.
    pub now_ms: i64,
    pub text: &'a Text,
}

/// Why setup stopped: a stable code and the message in the owner's language.
#[derive(Debug)]
pub struct Failure {
    pub code: Code,
    pub message: String,
}

impl Failure {
    pub(crate) fn api(e: &ApiError, text: &Text) -> Self {
        Self {
            code: e.code(),
            message: e.message(text),
        }
    }

    pub(crate) fn token(e: &TokenError, text: &Text) -> Self {
        Self {
            code: e.code(),
            message: e.message(text),
        }
    }

    pub(crate) fn new(code: Code, text: &Text, key: Key, args: &[(&str, String)]) -> Self {
        Self {
            code,
            message: text.t(key, args),
        }
    }
}

/// Text from Cloudflare, safe on a terminal.
pub(crate) fn printable(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// 1-50 of `a-z 0-9 -`, not starting or ending with `-`: valid as a Worker, D1, R2 and
/// workers.dev label.
#[must_use]
pub fn valid_name(name: &str) -> bool {
    (1..=MAX_NAME).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
        && !name.ends_with('-')
}

/// A host name as the `origin` setting needs it: lowercase ASCII labels, at least two, no
/// scheme, port, path or trailing dot. `None` if it is not one.
#[must_use]
pub fn normalize_domain(input: &str) -> Option<String> {
    let host = input.trim().to_ascii_lowercase();
    let labels: Vec<&str> = host.split('.').collect();
    let ok = host.len() <= 253
        && labels.len() >= 2
        && labels.iter().all(|l| {
            (1..=63).contains(&l.len())
                && l.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && !l.starts_with('-')
                && !l.ends_with('-')
        });
    ok.then_some(host)
}

/// Standard base64 with padding, the form `WEBHOOK_KEY` is read in (plumblog-webhook b64).
pub(crate) fn base64_std(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(A[((n >> (18 - 6 * i)) & 0x3f) as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// A new `WEBHOOK_KEY` value.
///
/// # Errors
/// The OS random source failed.
pub fn new_webhook_key() -> Result<String, getrandom::Error> {
    let mut b = [0u8; WEBHOOK_KEY_BYTES];
    getrandom::getrandom(&mut b)?;
    Ok(base64_std(&b))
}

/// A new setup code: 20 letters and digits in groups of 5 (100 random bits).
///
/// # Errors
/// The OS random source failed.
pub fn new_setup_code() -> Result<String, getrandom::Error> {
    let mut b = [0u8; CODE_GROUPS * CODE_GROUP_LEN];
    getrandom::getrandom(&mut b)?;
    let groups: Vec<String> = b
        .chunks(CODE_GROUP_LEN)
        .map(|g| {
            g.iter()
                .map(|x| char::from(CODE_ALPHABET[usize::from(x & 31)]))
                .collect()
        })
        .collect();
    Ok(groups.join("-"))
}

/// What the Worker stores for a code: lowercase hex SHA-256 (api/auth.rs `code_hash`).
#[must_use]
pub fn code_hash(code: &str) -> String {
    plumblog_auth::base64url::hex(&plumblog_auth::sha256::sha256(code.as_bytes()))
}

/// A new site id `value_json`, as api/auth.rs `site_id` makes it: a JSON string of
/// base64url (no padding) of 16 random bytes.
///
/// # Errors
/// The OS random source failed.
pub fn new_site_id_json() -> Result<String, getrandom::Error> {
    let mut b = [0u8; SITE_ID_BYTES];
    getrandom::getrandom(&mut b)?;
    Ok(json_string(&plumblog_auth::base64url::encode(&b)))
}

/// A new script marker: 32 lowercase hex characters.
///
/// # Errors
/// The OS random source failed.
pub fn new_marker() -> Result<String, getrandom::Error> {
    let mut b = [0u8; MARKER_BYTES];
    getrandom::getrandom(&mut b)?;
    Ok(plumblog_auth::base64url::hex(&b))
}

fn json_string(s: &str) -> String {
    Value::String(s.to_string()).to_string()
}

#[derive(Deserialize)]
struct NamedD1 {
    uuid: String,
    name: String,
}

#[derive(Deserialize)]
struct NamedBucket {
    name: String,
}

#[derive(Deserialize)]
struct Zone {
    id: String,
    name: String,
}

#[derive(Deserialize)]
struct SecretName {
    name: String,
}

#[derive(Deserialize)]
struct WidgetItem {
    sitekey: String,
    name: String,
    #[serde(default)]
    domains: Vec<String>,
}

#[derive(Deserialize)]
struct WidgetSecret {
    sitekey: String,
    secret: String,
}

#[derive(Deserialize)]
struct WorkerDomain {
    hostname: String,
    service: String,
}

/// One run: the API, the owner's language, the output and the state file.
pub(crate) struct Run<'a> {
    pub(crate) client: &'a Client,
    pub(crate) cx: &'a Context<'a>,
    pub(crate) out: &'a mut dyn Write,
    pub(crate) state: SiteState,
    /// Secrets of this run, kept out of the state file.
    pub(crate) secrets: Vec<ApiToken>,
}

impl Run<'_> {
    pub(crate) fn t(&self, key: Key, args: &[(&str, String)]) -> String {
        self.cx.text.t(key, args)
    }

    pub(crate) fn line(&mut self, key: Key, args: &[(&str, String)]) {
        let l = self.t(key, args);
        // A closed stdout must not stop setup halfway; the state file still records it all.
        let _ = writeln!(self.out, "{l}");
    }

    fn status(&self, created: bool) -> String {
        self.t(
            if created {
                key!("cli.setup.status_created")
            } else {
                key!("cli.setup.status_reused")
            },
            &[],
        )
    }

    pub(crate) fn api(&self, e: &ApiError) -> Failure {
        Failure::api(e, self.cx.text)
    }

    /// Applies `change` to `site.local.toml` as it is on disk now (read-modify-write, under
    /// the folder lock) and keeps what was saved as this run's view. Never writes back a
    /// whole copy read earlier.
    pub(crate) fn update(&mut self, change: impl FnOnce(&mut SiteState)) -> Result<(), Failure> {
        let secrets: Vec<&ApiToken> = self.secrets.iter().collect();
        let saved = state::update(self.cx.dir, &secrets, change)
            .map_err(|e| state_failure(&e, self.cx.text))?;
        self.state = saved;
        Ok(())
    }

    pub(crate) fn random<T>(&self, r: Result<T, getrandom::Error>) -> Result<T, Failure> {
        r.map_err(|e| {
            Failure::new(
                Code::Internal,
                self.cx.text,
                key!("cli.error.internal"),
                &[("detail", format!("random source: {e}"))],
            )
        })
    }

    /// Lists, finds by name, else creates; if creating fails, lists again, since a lost
    /// reply or a concurrent run may have created it (names are unique per account).
    fn find_or_create<T>(
        &self,
        find: impl Fn(&Client) -> Result<Option<T>, ApiError>,
        create: impl Fn(&Client) -> Result<T, ApiError>,
    ) -> Result<(T, bool), Failure> {
        if let Some(found) = find(self.client).map_err(|e| self.api(&e))? {
            return Ok((found, false));
        }
        match create(self.client) {
            Ok(made) => Ok((made, true)),
            Err(e) => match find(self.client) {
                Ok(Some(found)) => Ok((found, false)),
                _ => Err(self.api(&e)),
            },
        }
    }
}

pub(crate) fn state_failure(e: &StateError, text: &Text) -> Failure {
    match e {
        StateError::Unreadable(path, reason) => Failure::new(
            Code::StateUnreadable,
            text,
            key!("cli.error.state_unreadable"),
            &[("path", path.clone()), ("reason", reason.clone())],
        ),
        StateError::Unwritable(path, reason) => Failure::new(
            Code::StateUnwritable,
            text,
            key!("cli.error.state_unwritable"),
            &[("path", path.clone()), ("reason", reason.clone())],
        ),
    }
}

/// Checks the options before anything is called.
///
/// # Errors
/// `--no-turnstile` without `--yes-i-understand`, a bad name or domain.
pub fn check_options(opts: &Options, text: &Text) -> Result<(), Failure> {
    if opts.turnstile == Turnstile::OffUnconfirmed {
        return Err(Failure::new(
            Code::NoTurnstileUnconfirmed,
            text,
            key!("cli.error.no_turnstile_unconfirmed"),
            &[],
        ));
    }
    if let Some(n) = &opts.name
        && !valid_name(n)
    {
        return Err(Failure::new(
            Code::NameInvalid,
            text,
            key!("cli.error.name_invalid"),
            &[("name", printable(n)), ("max", MAX_NAME.to_string())],
        ));
    }
    if let Some(d) = &opts.domain
        && normalize_domain(d).is_none()
    {
        return Err(Failure::new(
            Code::DomainInvalid,
            text,
            key!("cli.error.domain_invalid"),
            &[("domain", printable(d))],
        ));
    }
    Ok(())
}

/// A value given on the command line must match the one saved for this folder.
pub(crate) fn same_or_mismatch(
    saved: Option<&String>,
    given: Option<&String>,
    flag: &str,
    cx: &Context,
) -> Result<(), Failure> {
    match (saved, given) {
        (Some(s), Some(g)) if s != g => Err(Failure::new(
            Code::StateMismatch,
            cx.text,
            key!("cli.error.state_mismatch"),
            &[
                ("path", state::path(cx.dir).display().to_string()),
                ("flag", flag.to_string()),
                ("saved", s.clone()),
                ("given", g.clone()),
            ],
        )),
        _ => Ok(()),
    }
}

/// `blog setup`. Writes progress to `out` as each step finishes.
///
/// # Errors
/// [`Failure`] with a stable code; what finished before it is in `site.local.toml`.
pub fn run(
    client: &mut Client,
    account_id: Option<&str>,
    opts: &Options,
    cx: &Context,
    out: &mut dyn Write,
) -> Result<(), Failure> {
    let text = cx.text;
    check_options(opts, text)?;
    // The same lock as deploy and rollback: setup also migrates and writes the state.
    let _lock = crate::deploy::Lock::take(cx)?;
    let domain = opts.domain.as_deref().and_then(normalize_domain);
    let saved = state::load(cx.dir).map_err(|e| state_failure(&e, text))?;

    // 1. Token and account.
    let verified = token::verify(client, account_id).map_err(|e| Failure::token(&e, text))?;
    let account = verified.account.id.clone();
    let st = saved.unwrap_or_default();
    same_or_mismatch(
        st.account_id.as_ref(),
        Some(&account),
        "CLOUDFLARE_ACCOUNT_ID",
        cx,
    )?;
    same_or_mismatch(st.worker.as_ref(), opts.name.as_ref(), "--name", cx)?;
    same_or_mismatch(st.domain.as_ref(), domain.as_ref(), "--domain", cx)?;
    let name = opts
        .name
        .clone()
        .or_else(|| st.worker.clone())
        .unwrap_or_else(|| DEFAULT_NAME.to_string());
    let domain = domain.or_else(|| st.domain.clone());
    // Older setups made the script without a marker; a recorded placeholder or deploy of this
    // very name says it is ours.
    let recorded = st.worker.as_deref() == Some(name.as_str())
        && (st.placeholder_version.is_some() || !st.deploys.is_empty());

    let client: &Client = client;
    let mut run = Run {
        client,
        cx,
        out,
        state: st,
        secrets: Vec::new(),
    };
    let account_name = printable(&verified.account.name);
    run.line(
        key!("cli.setup.account"),
        &[("name", account_name), ("id", verified.account.id.clone())],
    );

    // 8 (looked up first). The origin, and with --domain its zone, before anything changes.
    let subdomain = workers_subdomain(client).map_err(|e| run.api(&e))?;
    let (origin, zone_id) = resolve_origin(&run, &name, domain.as_deref(), subdomain.as_deref())?;
    let host = origin.trim_start_matches("https://").to_string();
    // 5 (looked up first). A script of that name must be ours before anything is created.
    let script = existing_script(&run, &name, recorded)?;
    run.update(|s| {
        s.account_id = Some(account);
        s.worker = Some(name.clone());
        s.domain.clone_from(&domain);
    })?;

    // 2. D1; an existing one must be ours and keep its origin unless told otherwise.
    let db = database(&mut run, &name)?;
    let database = Database::new(client, db);
    // An origin given explicitly (--domain) or confirmed (--change-origin) may replace the
    // stored one; a computed one (lost state, fresh folder) must not silently move the site.
    let may_change = opts.domain.is_some() || opts.change_origin;
    check_existing(&run, &database, &origin, may_change)?;
    run.update(|s| s.origin = Some(origin.clone()))?;
    run.line(key!("cli.setup.origin"), &[("origin", origin.clone())]);

    // 3. R2, 4. migrations.
    bucket(&mut run, &name)?;
    apply_migrations(&mut run, &database, opts.clear_failed_migration)?;

    // 5. Placeholder Worker and its workers.dev address.
    let secrets = worker(&mut run, &name, script, subdomain.is_some(), &host)?;

    // 6. WEBHOOK_KEY: created once, never rotated (rotating loses every webhook secret).
    if secrets.iter().any(|s| s == WEBHOOK_KEY) {
        secret_line(&mut run, WEBHOOK_KEY, false);
    } else {
        let value = run.random(new_webhook_key())?;
        put_secret(&mut run, &name, WEBHOOK_KEY, &value)?;
        secret_line(&mut run, WEBHOOK_KEY, true);
    }

    // 7. Turnstile.
    if opts.turnstile == Turnstile::On {
        turnstile(&mut run, &name, &host, &secrets)?;
    } else {
        turnstile_off(&mut run, &name, &database, &secrets)?;
    }

    // 8. Custom domain, its zone already found.
    if let (Some(h), Some(zone)) = (&domain, &zone_id) {
        attach_domain(&mut run, &name, h, zone)?;
    }

    // 9. Settings, then 10. the setup code.
    write_settings(&mut run, &database, &origin)?;
    setup_code(&mut run, &database, &origin)?;

    // 11.
    run.line(key!("cli.setup.done"), &[("origin", origin)]);
    Ok(())
}

/// The site's origin and, with a custom domain, its zone id. Only reads.
fn resolve_origin(
    run: &Run,
    name: &str,
    domain: Option<&str>,
    subdomain: Option<&str>,
) -> Result<(String, Option<String>), Failure> {
    let text = run.cx.text;
    if let Some(host) = domain {
        let Some(zone) = find_zone(run.client, host).map_err(|e| run.api(&e))? else {
            let tried = zone_candidates(host).join(", ");
            return Err(Failure::new(
                Code::ZoneNotFound,
                text,
                key!("cli.error.zone_not_found"),
                &[("domain", host.to_string()), ("tried", tried)],
            ));
        };
        return Ok((format!("https://{host}"), Some(zone)));
    }
    let Some(sub) = subdomain else {
        return Err(Failure::new(
            Code::WorkersDevMissing,
            text,
            key!("cli.error.workers_dev_missing"),
            &[],
        ));
    };
    Ok((format!("https://{name}.{sub}.workers.dev"), None))
}

/// D1 database `<name>`; returns its id.
fn database(run: &mut Run, name: &str) -> Result<String, Failure> {
    let (db, created) = run.find_or_create(
        |c| {
            let all: Vec<NamedD1> = c.call_all_as(&Call::new(Endpoint::D1List), 100, None)?;
            Ok(all.into_iter().find(|d| d.name == name).map(|d| d.uuid))
        },
        |c| {
            let made: NamedD1 =
                c.call_as(&Call::new(Endpoint::D1Create).json(json!({"name": name})))?;
            Ok(made.uuid)
        },
    )?;
    run.update(|s| {
        s.d1 = Some(D1State {
            name: name.to_string(),
            id: db.clone(),
        });
    })?;
    let s = run.status(created);
    run.line(
        key!("cli.setup.d1"),
        &[("name", name.to_string()), ("status", s)],
    );
    Ok(db)
}

/// Refuses a database `blog` did not make, and an origin change nobody asked for. Reads only.
fn check_existing(run: &Run, db: &Database, origin: &str, may_change: bool) -> Result<(), Failure> {
    let text = run.cx.text;
    let tables = match migrations::check_ours(db) {
        Ok(()) => migrations::user_tables(db).map_err(|e| run.api(&e))?,
        Err(e) => return Err(migration_error(run, e)),
    };
    if may_change || !tables.iter().any(|t| t == "settings") {
        return Ok(());
    }
    let stored = d1::settings(db, &[ORIGIN_KEY]).map_err(|e| run.api(&e))?;
    let Some((_, raw)) = stored.into_iter().next() else {
        return Ok(());
    };
    let old: String = serde_json::from_str(&raw).unwrap_or(raw);
    if old == origin {
        return Ok(());
    }
    Err(Failure::new(
        Code::OriginChangeUnconfirmed,
        text,
        key!("cli.error.origin_change_unconfirmed"),
        &[("old", printable(&old)), ("new", origin.to_string())],
    ))
}

/// A refusal from the migrations module that is not a failed migration.
pub(crate) fn migration_error(run: &Run, e: migrations::Error) -> Failure {
    match e {
        migrations::Error::Foreign(tables) => Failure::new(
            Code::DatabaseForeign,
            run.cx.text,
            key!("cli.error.database_foreign"),
            &[
                (
                    "name",
                    run.state
                        .d1
                        .as_ref()
                        .map(|d| d.name.clone())
                        .unwrap_or_default(),
                ),
                ("tables", printable(&tables.join(", "))),
            ],
        ),
        migrations::Error::Before(e) => run.api(&e),
        migrations::Error::Failed { error, .. } => run.api(&error),
    }
}

/// R2 bucket `<name>-media`.
fn bucket(run: &mut Run, name: &str) -> Result<(), Failure> {
    let bucket = format!("{name}-media");
    let ((), created) = run.find_or_create(
        |c| {
            let all: Vec<NamedBucket> =
                c.call_all_as(&Call::new(Endpoint::R2BucketList), 1000, Some("buckets"))?;
            Ok(all.into_iter().find(|b| b.name == bucket).map(drop))
        },
        |c| {
            c.call(&Call::new(Endpoint::R2BucketCreate).json(json!({"name": bucket})))
                .map(drop)
        },
    )?;
    run.update(|s| {
        s.r2 = Some(R2State {
            bucket: bucket.clone(),
        });
    })?;
    let s = run.status(created);
    run.line(key!("cli.setup.r2"), &[("name", bucket), ("status", s)]);
    Ok(())
}

/// A script that already has the site's name: its secret names, its marker and whether it
/// has the `DB` binding (`None`: no such script).
pub(crate) struct Existing {
    secrets: Vec<String>,
    marker: Option<String>,
    deployed: bool,
}

/// Refuses a script of that name that `blog` did not make for this site: no marker, and no
/// placeholder or deploy recorded for it in this folder; or another site's marker. Reads only.
fn existing_script(run: &Run, name: &str, recorded: bool) -> Result<Option<Existing>, Failure> {
    let client = run.client;
    let Some(secrets) = script_secrets(client, name).map_err(|e| run.api(&e))? else {
        return Ok(None);
    };
    let bindings = script_bindings(client, name).map_err(|e| run.api(&e))?;
    let find = |kind: &str, n: &str| {
        bindings
            .iter()
            .find(|b| str_of(b, "type") == Some(kind) && str_of(b, "name") == Some(n))
    };
    let marker = find("plain_text", MARKER_BINDING)
        .and_then(|b| str_of(b, "text"))
        .map(str::to_string);
    let ours = match (&marker, &run.state.script_marker) {
        (Some(m), Some(mine)) => m == mine,
        // Made by `blog` for a site of this name; this folder lost or never had the state.
        (Some(_), None) => true,
        (None, _) => recorded,
    };
    if !ours {
        return Err(Failure::new(
            Code::ScriptForeign,
            run.cx.text,
            key!("cli.error.script_foreign"),
            &[
                ("name", name.to_string()),
                ("binding", MARKER_BINDING.to_string()),
            ],
        ));
    }
    Ok(Some(Existing {
        secrets,
        marker,
        deployed: find("d1", "DB").is_some(),
    }))
}

fn str_of<'v>(v: &'v Value, key: &str) -> Option<&'v str> {
    v.get(key).and_then(Value::as_str)
}

/// The script (a placeholder if new) and its workers.dev address; returns its secret names.
fn worker(
    run: &mut Run,
    name: &str,
    existing: Option<Existing>,
    workers_dev: bool,
    host: &str,
) -> Result<Vec<String>, Failure> {
    let client = run.client;
    let secrets = if let Some(found) = existing {
        run.line(
            key!("cli.setup.worker_reused"),
            &[("name", name.to_string())],
        );
        if let Some(m) = found.marker {
            if run.state.script_marker.as_ref() != Some(&m) {
                run.update(|s| s.script_marker = Some(m))?;
            }
        } else {
            // An older setup: ours by record. The binding comes with the next `blog deploy`
            // (a script's bindings change only with a new version of its code).
            let m = match run.state.script_marker.clone() {
                Some(m) => m,
                None => run.random(new_marker())?,
            };
            run.update(|s| s.script_marker = Some(m))?;
            run.line(
                key!("cli.setup.marker_pending"),
                &[("binding", MARKER_BINDING.to_string())],
            );
        }
        // Never deployed by `blog` (no record, no `DB` binding): what serves is the
        // placeholder, e.g. after its upload's reply was lost.
        if run.state.placeholder_version.is_none()
            && run.state.deploys.is_empty()
            && !found.deployed
        {
            let v = crate::deploy::active_version(client, name).map_err(|e| run.api(&e))?;
            run.update(|s| s.placeholder_version = Some(v))?;
        }
        found.secrets
    } else {
        // Saved before the upload: a re-run after a lost reply recognises the script.
        let marker = match run.state.script_marker.clone() {
            Some(m) => m,
            None => run.random(new_marker())?,
        };
        run.update(|s| s.script_marker = Some(marker.clone()))?;
        upload_placeholder(client, name, &marker).map_err(|e| run.api(&e))?;
        // `blog rollback` must never land on the placeholder.
        let placeholder = crate::deploy::active_version(client, name).map_err(|e| run.api(&e))?;
        run.update(|s| s.placeholder_version = Some(placeholder))?;
        run.line(
            key!("cli.setup.worker_created"),
            &[("name", name.to_string())],
        );
        Vec::new()
    };
    if !workers_dev {
        run.line(
            key!("cli.setup.workers_dev_skipped"),
            &[("host", host.to_string())],
        );
        return Ok(secrets);
    }
    let on = workers_dev_enabled(client, name).map_err(|e| run.api(&e))?;
    if !on {
        client
            .call(
                &Call::new(Endpoint::ScriptSubdomainSet)
                    .param("script_name", name.to_string())
                    .json(json!({"enabled": true})),
            )
            .map_err(|e| run.api(&e))?;
    }
    let s = run.t(
        if on {
            key!("cli.setup.status_already_on")
        } else {
            key!("cli.setup.status_turned_on")
        },
        &[],
    );
    run.line(key!("cli.setup.workers_dev"), &[("status", s)]);
    Ok(secrets)
}

/// The account's workers.dev subdomain; `None` if it has none yet.
/// <https://developers.cloudflare.com/api/resources/workers/subresources/subdomains/methods/get/>
pub(crate) fn workers_subdomain(client: &Client) -> Result<Option<String>, ApiError> {
    match client.call(&Call::new(Endpoint::SubdomainGet)) {
        Ok(v) => Ok(v
            .get("subdomain")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_ascii_lowercase)
            .filter(|s| valid_label(s))),
        // Not documented: an account that never opened Workers answers 404 / 10007, as a
        // missing script does.
        Err(e) if e.status == Some(404) || e.has_code(SCRIPT_NOT_FOUND) => Ok(None),
        Err(e) => Err(e),
    }
}

fn valid_label(s: &str) -> bool {
    normalize_domain(&format!("{s}.x")).is_some() && !s.contains('.')
}

/// The domain itself, then each parent with at least two labels (as wrangler's
/// `resolveDomain` tries them).
fn zone_candidates(host: &str) -> Vec<String> {
    let labels: Vec<&str> = host.split('.').collect();
    (0..labels.len().saturating_sub(1))
        .map(|i| labels[i..].join("."))
        .collect()
}

/// The zone holding `host` in this account. `GET /zones?name=&account.id=`, as wrangler's
/// `getZoneIdByDomain` calls it. <https://developers.cloudflare.com/api/resources/zones/methods/list/>
fn find_zone(client: &Client, host: &str) -> Result<Option<String>, ApiError> {
    let account = client.account_id().unwrap_or_default().to_string();
    for cand in zone_candidates(host) {
        let zones: Vec<Zone> = client.call_as(
            &Call::new(Endpoint::ZonesList)
                .query("name", cand.clone())
                .query("account.id", account.clone()),
        )?;
        if let Some(z) = zones.into_iter().find(|z| z.name == cand) {
            return Ok(Some(z.id));
        }
    }
    Ok(None)
}

/// Secret names of the script, or `None` if the script does not exist yet.
fn script_secrets(client: &Client, name: &str) -> Result<Option<Vec<String>>, ApiError> {
    let call = Call::new(Endpoint::SecretList).param("script_name", name.to_string());
    match client.call_as::<Vec<SecretName>>(&call) {
        Ok(list) => Ok(Some(list.into_iter().map(|s| s.name).collect())),
        // Only Cloudflare's own "script not found": any other error (a bare 404 included)
        // may mean the script exists, and a placeholder must never replace a real Worker.
        Err(e) if e.has_code(SCRIPT_NOT_FOUND) => Ok(None),
        Err(e) => Err(e),
    }
}

/// Bindings of the deployed script, as wrangler 4.160.0 reads them (`getSettings`:
/// `GET .../scripts/{name}/settings`, `result.bindings[]`; a plain-text binding carries
/// `text`, as `convertWorkerMetadataBindingsToFlatBindings` reads it).
/// <https://developers.cloudflare.com/api/resources/workers/subresources/scripts/subresources/settings/methods/get/>
pub(crate) fn script_bindings(client: &Client, name: &str) -> Result<Vec<Value>, ApiError> {
    let v = client
        .call(&Call::new(Endpoint::ScriptSettingsGet).param("script_name", name.to_string()))?;
    Ok(v.get("bindings")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// First upload of the script: multipart with a `metadata` part and one ES module part.
/// Part names and the module type `application/javascript+module` as wrangler 4.160.0's
/// `createWorkerUploadForm` sends them; the marker as its plain-text binding
/// (`{name, type: "plain_text", text}`).
/// <https://developers.cloudflare.com/workers/configuration/multipart-upload-metadata/>
fn upload_placeholder(client: &Client, name: &str, marker: &str) -> Result<(), ApiError> {
    let mut rnd = [0u8; 16];
    // A fixed boundary would also do: neither part can contain it.
    let _ = getrandom::getrandom(&mut rnd);
    let boundary = format!("plumblog{}", plumblog_auth::base64url::hex(&rnd));
    let metadata = json!({
        "main_module": "worker.js",
        "compatibility_date": crate::deploy::COMPATIBILITY_DATE,
        "bindings": [{"type": "plain_text", "name": MARKER_BINDING, "text": marker}],
    });
    let mut body = String::new();
    for (part, file, ctype, content) in [
        ("metadata", None, "application/json", metadata.to_string()),
        (
            "worker.js",
            Some("worker.js"),
            "application/javascript+module",
            PLACEHOLDER_JS.to_string(),
        ),
    ] {
        let _ = write!(
            body,
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{part}\""
        );
        if let Some(f) = file {
            let _ = write!(body, "; filename=\"{f}\"");
        }
        let _ = write!(body, "\r\nContent-Type: {ctype}\r\n\r\n{content}\r\n");
    }
    let _ = write!(body, "--{boundary}--\r\n");
    client
        .call(
            &Call::new(Endpoint::ScriptUpload)
                .param("script_name", name.to_string())
                .raw(
                    format!("multipart/form-data; boundary={boundary}"),
                    body.into_bytes(),
                ),
        )
        .map(drop)
}

fn workers_dev_enabled(client: &Client, name: &str) -> Result<bool, ApiError> {
    let v = client
        .call(&Call::new(Endpoint::ScriptSubdomainGet).param("script_name", name.to_string()))?;
    Ok(v.get("enabled").and_then(Value::as_bool).unwrap_or(false))
}

fn secret_line(run: &mut Run, name: &str, set: bool) {
    let s = run.t(
        if set {
            key!("cli.setup.status_set")
        } else {
            key!("cli.setup.status_already_set")
        },
        &[],
    );
    run.line(
        key!("cli.setup.secret"),
        &[("name", name.to_string()), ("status", s)],
    );
}

/// `PUT .../secrets {name, text, type: "secret_text"}`; the value is hidden in every error
/// and kept out of the state file.
fn put_secret(run: &mut Run, script: &str, name: &str, value: &str) -> Result<(), Failure> {
    let hidden = || {
        ApiToken::new(value).map_err(|_| {
            Failure::new(
                Code::Internal,
                run.cx.text,
                key!("cli.error.internal"),
                &[("detail", format!("secret {name} is not printable ASCII"))],
            )
        })
    };
    let call = Call::new(Endpoint::SecretPut)
        .param("script_name", script.to_string())
        .json(json!({"name": name, "text": value, "type": "secret_text"}))
        .secret(hidden()?);
    let kept = hidden()?;
    run.secrets.push(kept);
    run.client.call(&call).map(drop).map_err(|e| run.api(&e))
}

pub(crate) fn apply_migrations(
    run: &mut Run,
    db: &Database,
    clear_failed: bool,
) -> Result<(), Failure> {
    apply_migrations_undoing_to(run, db, clear_failed, None)
}

/// [`apply_migrations`], but a failure names `undo` instead of the bookmark taken just before
/// the migrations: `blog restore` passes its own pre-import bookmark, since the one taken
/// after its import holds the old backup, and restoring that would lose newer data again.
pub(crate) fn apply_migrations_undoing_to(
    run: &mut Run,
    db: &Database,
    clear_failed: bool,
    undo: Option<&str>,
) -> Result<(), Failure> {
    // A recorded failure means the database may be half migrated: a new bookmark now would
    // capture that state and replace the clean one, so nothing runs until it is restored.
    if let Some(f) = run.state.migrations.failed.clone() {
        if !clear_failed {
            let restore = ROLLBACK_COMMAND.replace("{bookmark}", &f.bookmark);
            return Err(Failure::new(
                Code::MigrationRestorePending,
                run.cx.text,
                key!("cli.error.migration_restore_pending"),
                &[
                    ("name", f.name),
                    ("bookmark", f.bookmark),
                    ("restore", restore),
                    ("path", state::path(run.cx.dir).display().to_string()),
                ],
            ));
        }
        run.update(|s| s.migrations.failed = None)?;
    }
    match migrations::apply(db, run.client, run.cx.migrations) {
        Ok(report) => {
            run.update(|s| {
                s.migrations.applied.clone_from(&report.recorded);
                if report.bookmark.is_some() {
                    s.migrations.last_bookmark.clone_from(&report.bookmark);
                }
            })?;
            if let Some(b) = &report.bookmark {
                run.line(key!("cli.setup.bookmark"), &[("bookmark", b.clone())]);
                run.line(
                    key!("cli.setup.migrations_applied"),
                    &[("list", report.applied.join(", "))],
                );
            } else {
                let latest = report.recorded.last().cloned().unwrap_or_default();
                run.line(key!("cli.setup.migrations_current"), &[("name", latest)]);
            }
            Ok(())
        }
        Err(e @ (migrations::Error::Before(_) | migrations::Error::Foreign(_))) => {
            Err(migration_error(run, e))
        }
        Err(migrations::Error::Failed {
            name,
            error,
            bookmark,
            applied,
            sql_succeeded,
        }) => {
            // Record what did apply and the bookmark; no step after this runs, so no
            // settings are written on a database in an unknown state.
            let taken = bookmark;
            let bookmark = undo.map_or_else(|| taken.clone(), str::to_string);
            run.update(|s| {
                for a in applied {
                    if !s.migrations.applied.contains(&a) {
                        s.migrations.applied.push(a);
                    }
                }
                s.migrations.last_bookmark = Some(taken.clone());
                s.migrations.failed = Some(FailedMigration {
                    name: name.clone(),
                    bookmark: bookmark.clone(),
                });
            })?;
            let restore = ROLLBACK_COMMAND.replace("{bookmark}", &bookmark);
            // Its SQL ran but `d1_migrations` does not say so: a re-run would apply it
            // again, so the code differs and the restore is the only way on.
            let (code, k) = if sql_succeeded {
                (
                    Code::MigrationUnrecorded,
                    key!("cli.error.migration_unrecorded"),
                )
            } else {
                (Code::MigrationFailed, key!("cli.error.migration_failed"))
            };
            Err(Failure::new(
                code,
                run.cx.text,
                k,
                &[
                    ("name", name),
                    ("error", error.message(run.cx.text)),
                    ("bookmark", bookmark),
                    ("restore", restore),
                ],
            ))
        }
    }
}

fn turnstile(run: &mut Run, worker: &str, host: &str, secrets: &[String]) -> Result<(), Failure> {
    let widget_name = format!("plumblog-{worker}");
    let all: Vec<WidgetItem> = run
        .client
        .call_all_as(&Call::new(Endpoint::TurnstileList), 50, None)
        .map_err(|e| run.api(&e))?;
    let same: Vec<String> = all
        .into_iter()
        .filter(|w| w.name == widget_name)
        .map(|w| printable(&w.sitekey))
        .collect();
    if same.len() > 1 {
        // Which one the site uses cannot be guessed; the owner deletes the extra ones.
        return Err(Failure::new(
            Code::TurnstileAmbiguous,
            run.cx.text,
            key!("cli.error.turnstile_ambiguous"),
            &[("name", widget_name), ("sitekeys", same.join(", "))],
        ));
    }
    let find = |c: &Client| -> Result<Option<WidgetItem>, ApiError> {
        let all: Vec<WidgetItem> = c.call_all_as(&Call::new(Endpoint::TurnstileList), 50, None)?;
        Ok(all.into_iter().find(|w| w.name == widget_name))
    };
    // Created: the reply carries the secret. Found: read it from GET widget.
    let (widget, created) = run.find_or_create(
        |c| {
            find(c)?
                .map(|w| {
                    let got: WidgetSecret = c.call_as(
                        &Call::new(Endpoint::TurnstileGet).param("sitekey", w.sitekey.clone()),
                    )?;
                    Ok((got, w.domains))
                })
                .transpose()
        },
        |c| {
            let made: WidgetSecret = c.call_as(
                &Call::new(Endpoint::TurnstileCreate)
                    .json(json!({"name": widget_name, "domains": [host], "mode": "managed"})),
            )?;
            Ok((made, vec![host.to_string()]))
        },
    )?;
    let (w, domains) = widget;
    let s = run.status(created);
    run.line(
        key!("cli.setup.turnstile"),
        &[("name", widget_name.clone()), ("status", s)],
    );
    if !domains.iter().any(|d| d == host) {
        run.line(
            key!("cli.setup.turnstile_host_missing"),
            &[("name", widget_name), ("host", host.to_string())],
        );
    }
    // A newly created widget has a new secret, so an existing TURNSTILE_SECRET is stale.
    let has = secrets.iter().any(|s| s == TURNSTILE_SECRET);
    if has && !created {
        secret_line(run, TURNSTILE_SECRET, false);
    } else {
        put_secret(run, worker, TURNSTILE_SECRET, &w.secret)?;
        secret_line(run, TURNSTILE_SECRET, true);
    }
    run.update(|s| s.turnstile_site_key = Some(w.sitekey))
}

/// `--no-turnstile --yes-i-understand` on a site that had Turnstile: the setting goes (with
/// the Worker's own delete SQL and its content-version bump), then `TURNSTILE_SECRET`, then
/// the site key in state. The widget stays, named, for the owner to delete or reuse.
fn turnstile_off(
    run: &mut Run,
    worker: &str,
    db: &Database,
    secrets: &[String],
) -> Result<(), Failure> {
    let stored = d1::settings(db, &[TURNSTILE_SITE_KEY]).map_err(|e| run.api(&e))?;
    let was_on = !stored.is_empty()
        || run.state.turnstile_site_key.is_some()
        || secrets.iter().any(|s| s == TURNSTILE_SECRET);
    if !stored.is_empty() {
        d1::delete_setting(db, TURNSTILE_SITE_KEY).map_err(|e| run.api(&e))?;
    }
    if secrets.iter().any(|s| s == TURNSTILE_SECRET) {
        run.client
            .call(
                &Call::new(Endpoint::SecretDelete)
                    .param("script_name", worker.to_string())
                    .param("secret_name", TURNSTILE_SECRET.to_string()),
            )
            .map_err(|e| run.api(&e))?;
    }
    if run.state.turnstile_site_key.is_some() {
        run.update(|s| s.turnstile_site_key = None)?;
    }
    if was_on {
        run.line(
            key!("cli.setup.turnstile_removed"),
            &[
                ("name", format!("plumblog-{worker}")),
                ("secret", TURNSTILE_SECRET.to_string()),
            ],
        );
    }
    run.line(key!("cli.setup.turnstile_off"), &[]);
    Ok(())
}

/// `PUT /workers/domains {hostname, service, zone_id, environment}`; body as the Cloudflare
/// SDK wrangler 4.160.0 bundles (`client.workers.domains.update`) shows it.
fn attach_domain(run: &mut Run, worker: &str, host: &str, zone: &str) -> Result<(), Failure> {
    // Every page, filtered here: a server that ignored a hostname filter must not make an
    // attached domain look free.
    let list: Vec<WorkerDomain> = run
        .client
        .call_all_as(&Call::new(Endpoint::DomainList), 100, None)
        .map_err(|e| run.api(&e))?;
    let attached = match list.into_iter().find(|d| d.hostname == host) {
        Some(d) if d.service == worker => false,
        Some(d) => {
            return Err(Failure::new(
                Code::DomainTaken,
                run.cx.text,
                key!("cli.error.domain_taken"),
                &[
                    ("host", host.to_string()),
                    ("service", printable(&d.service)),
                ],
            ));
        }
        None => {
            run.client
                .call(&Call::new(Endpoint::DomainAttach).json(json!({
                    "hostname": host,
                    "service": worker,
                    "zone_id": zone,
                    "environment": "production",
                })))
                .map_err(|e| run.api(&e))?;
            true
        }
    };
    let s = run.t(
        if attached {
            key!("cli.setup.status_attached")
        } else {
            key!("cli.setup.status_reused")
        },
        &[],
    );
    run.line(
        key!("cli.setup.domain"),
        &[("host", host.to_string()), ("status", s)],
    );
    Ok(())
}

fn write_settings(run: &mut Run, db: &Database, origin: &str) -> Result<(), Failure> {
    let now = run.cx.now_ms;
    let current = d1::settings(
        db,
        &[ORIGIN_KEY, TURNSTILE_SITE_KEY, SITE_ID_KEY, SITE_LANG_KEY],
    )
    .map_err(|e| run.api(&e))?;
    let get = |k: &str| {
        current
            .iter()
            .find(|(key, _)| key == k)
            .map(|(_, v)| v.clone())
    };
    let mut written = Vec::new();
    let want_origin = json_string(origin);
    let old_origin = get(ORIGIN_KEY);
    if old_origin.as_deref() != Some(&want_origin) {
        d1::put_setting(db, ORIGIN_KEY, &want_origin, now).map_err(|e| run.api(&e))?;
        written.push(ORIGIN_KEY);
        if let Some(old) = old_origin {
            let old: String = serde_json::from_str(&old).unwrap_or(old);
            run.line(
                key!("cli.setup.origin_changed"),
                &[("old", printable(&old)), ("new", origin.to_string())],
            );
        }
    }
    if let Some(sitekey) = run.state.turnstile_site_key.clone() {
        let want = json_string(&sitekey);
        if get(TURNSTILE_SITE_KEY).as_deref() != Some(&want) {
            d1::put_setting(db, TURNSTILE_SITE_KEY, &want, now).map_err(|e| run.api(&e))?;
            written.push(TURNSTILE_SITE_KEY);
        }
    }
    // A first setup (no user yet) starts the site in the owner's language (flag, env, OS locale,
    // else English). Never on a site with users or over a stored value: `site_lang` decides post
    // URLs, so setting it later would move every `/<slug>` and break old links.
    if get(SITE_LANG_KEY).is_none() && !d1::has_users(db).map_err(|e| run.api(&e))? {
        let lang = json_string(run.cx.text.lang());
        d1::put_setting(db, SITE_LANG_KEY, &lang, now).map_err(|e| run.api(&e))?;
        written.push(SITE_LANG_KEY);
    }
    let site_id = if let Some(v) = get(SITE_ID_KEY) {
        v
    } else {
        let id = run.random(new_site_id_json())?;
        d1::ensure_site_id(db, &id, now).map_err(|e| run.api(&e))?;
        written.push(SITE_ID_KEY);
        // Read back: the Worker may have created one first, and that one wins.
        d1::settings(db, &[SITE_ID_KEY])
            .map_err(|e| run.api(&e))?
            .into_iter()
            .next()
            .map_or(id, |(_, v)| v)
    };
    let site_id = serde_json::from_str::<String>(&site_id).ok();
    if site_id.is_some() && run.state.site_id != site_id {
        run.update(|s| s.site_id = site_id)?;
    }
    let s = if written.is_empty() {
        run.t(key!("cli.setup.status_unchanged"), &[])
    } else {
        run.t(
            key!("cli.setup.status_written"),
            &[("keys", written.join(", "))],
        )
    };
    run.line(key!("cli.setup.settings"), &[("status", s)]);
    Ok(())
}

fn setup_code(run: &mut Run, db: &Database, origin: &str) -> Result<(), Failure> {
    if d1::has_users(db).map_err(|e| run.api(&e))? {
        run.line(key!("cli.setup.owner_exists"), &[]);
        return Ok(());
    }
    let code = run.random(new_setup_code())?;
    let now = run.cx.now_ms;
    let written = d1::put_setup_code(db, &code_hash(&code), now + SETUP_CODE_TTL_MS, now)
        .map_err(|e| run.api(&e))?;
    if written {
        run.line(key!("cli.setup.code"), &[("code", code)]);
        run.line(
            key!("cli.setup.code_next"),
            &[("origin", origin.to_string())],
        );
    } else {
        // An owner was created between the check and the write.
        run.line(key!("cli.setup.owner_exists"), &[]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_and_domains() {
        for ok in ["blog", "my-blog-2", "a"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", "-x", "x-", "My", "a_b", "a.b", &"x".repeat(51)] {
            assert!(!valid_name(bad), "{bad}");
        }
        assert_eq!(
            normalize_domain(" Blog.Example.COM ").as_deref(),
            Some("blog.example.com")
        );
        for bad in [
            "example",
            "https://a.b",
            "a.b/",
            "a.b:443",
            "a..b",
            "-a.b",
            "a.b.",
            "bl\u{f6}g.de",
        ] {
            assert_eq!(normalize_domain(bad), None, "{bad}");
        }
        assert_eq!(
            zone_candidates("a.blog.example.co"),
            ["a.blog.example.co", "blog.example.co", "example.co"]
        );
    }

    #[test]
    fn base64_matches_rfc4648_vectors() {
        for (i, o) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64_std(i.as_bytes()), o);
        }
        assert_eq!(base64_std(&[0xfb, 0xff]), "+/8=");
    }

    #[test]
    fn setup_code_is_readable_and_hashes_like_the_worker() {
        let code = new_setup_code().unwrap();
        assert_eq!(code.len(), 23);
        assert!(
            code.split('-')
                .all(|g| g.len() == 5 && g.bytes().all(|b| CODE_ALPHABET.contains(&b)))
        );
        assert_ne!(code, new_setup_code().unwrap());
        // SHA-256("abc"), FIPS 180-2.
        assert_eq!(
            code_hash("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        let id = new_site_id_json().unwrap();
        assert!(
            id.starts_with('"') && id.ends_with('"') && id.len() == 24,
            "{id}"
        );
    }
}
