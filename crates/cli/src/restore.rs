//! `blog restore <file.sql>`: the database replaced by a dump (`blog backup`,
//! `blog deploy --export`, crates/cli/src/dump.rs), also one older than Time Travel's 7 days. The order and the
//! guards are in docs/ARCHITECTURE.md (Site-specific state); nothing changes before the
//! Time Travel bookmark is saved in `site.local.toml`.
//!
//! The import is D1's own, with the calls wrangler 4.160.0 makes for
//! `d1 execute --file --remote` (wrangler-dist/cli.js `executeRemotely`,
//! `uploadAndBeginIngestion`, `pollUntilComplete`, near line 240040): `init` with the file's
//! MD5, a `PUT` of the bytes to the presigned `upload_url` (no token), `ingest`, then `poll`
//! until `complete` or `error`.

use crate::backup::md5_hex;
use crate::bundle::Bundle;
use crate::cloudflare::client::base_override_ok;
use crate::cloudflare::{ApiError, Call, Client, Endpoint};
use crate::d1::{self, Database};
use crate::deploy::{
    self, Clock, Lock, Site, at_ms, log_events, plain_agent, request_id, start, str_at, utc,
};
use crate::migrations;
use crate::setup::{self, Context, Failure, Run, printable};
use crate::state::{RollbackRecord, push_capped};
use crate::text::Code;
use plumblog_i18n::{Key, key};
use plumblog_store::{OWNER_RESET_KEYS, SETUP_CODE_EXPIRES_KEY, SETUP_CODE_HASH_KEY, SITE_ID_KEY};
use plumblog_syslog::{Event, EventKind};
use serde_json::{Value, json};
use std::fmt::Write as _;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

/// How long the import may take before `blog` stops watching it (wrangler polls forever).
const IMPORT_BUDGET: Duration = Duration::from_mins(30);
const POLL_WAIT: Duration = Duration::from_secs(1);
const UPLOAD_TIMEOUT: Duration = Duration::from_mins(30);
/// The kind of the `rollbacks` record a restore writes.
const KIND: &str = "file";

/// What the owner asked for.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// The `.sql` file, relative to the site folder or absolute.
    pub file: PathBuf,
    /// Without it: check, print the plan, change nothing.
    pub yes: bool,
    /// As `blog deploy`: the code whose migrations the restored database must match.
    pub bundle: Option<PathBuf>,
    pub build: bool,
    pub release: Option<crate::release::Release>,
    /// Restore a backup whose `site_id` is not this site's.
    pub allow_other_site: bool,
}

// --- Reading the dump ---

/// Why a file is not a backup; each has a locale key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    NotText,
    Unterminated,
    NoMigrationsTable,
    NoSettingsTable,
    RowBeforeTable,
    RowUnreadable,
    /// No row counts at the end: a cut file, or not a plumblog dump.
    NoRowCounts,
}

impl Reason {
    fn key(self) -> Key {
        match self {
            Self::NotText => key!("cli.restore.reason_not_text"),
            Self::Unterminated => key!("cli.restore.reason_unterminated"),
            Self::NoMigrationsTable => key!("cli.restore.reason_no_migrations"),
            Self::NoSettingsTable => key!("cli.restore.reason_no_settings"),
            Self::RowBeforeTable => key!("cli.restore.reason_row_before_table"),
            Self::RowUnreadable => key!("cli.restore.reason_row_unreadable"),
            Self::NoRowCounts => key!("cli.restore.reason_no_counts"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tok<'a> {
    /// Keyword, bare name or number.
    Word(&'a str),
    /// Quoted name (`"x"`, `` `x` ``, `[x]`): the text inside and its quote.
    Name(&'a str, u8),
    /// String literal: the text inside, `''` not yet undone.
    Str(&'a str),
    Punct(u8),
}

/// SQLite's tokens, enough to find statements and literals; comments are skipped.
struct Lexer<'a> {
    s: &'a str,
    i: usize,
}

impl Lexer<'_> {
    /// Index just past the closing `q` of a run where `qq` stands for one `q`.
    fn quoted(&self, from: usize, q: u8) -> Option<usize> {
        let b = self.s.as_bytes();
        let mut j = from;
        while j < b.len() {
            if b[j] == q {
                if b.get(j + 1) == Some(&q) {
                    j += 2;
                    continue;
                }
                return Some(j + 1);
            }
            j += 1;
        }
        None
    }
}

impl<'a> Iterator for Lexer<'a> {
    type Item = Result<Tok<'a>, Reason>;

    fn next(&mut self) -> Option<Self::Item> {
        let b = self.s.as_bytes();
        loop {
            while self.i < b.len() && b[self.i].is_ascii_whitespace() {
                self.i += 1;
            }
            if b[self.i..].starts_with(b"--") {
                self.i = self.s[self.i..].find('\n').map_or(b.len(), |n| self.i + n);
            } else if b[self.i..].starts_with(b"/*") {
                self.i = self.s[self.i + 2..]
                    .find("*/")
                    .map_or(b.len(), |n| self.i + n + 4);
            } else {
                break;
            }
        }
        let start = self.i;
        let c = *b.get(start)?;
        let tok = match c {
            b'\'' | b'"' | b'`' => {
                let Some(end) = self.quoted(start + 1, c) else {
                    self.i = b.len();
                    return Some(Err(Reason::Unterminated));
                };
                self.i = end;
                let inner = &self.s[start + 1..end - 1];
                if c == b'\'' {
                    Tok::Str(inner)
                } else {
                    Tok::Name(inner, c)
                }
            }
            b'[' => {
                let Some(n) = self.s[start..].find(']') else {
                    self.i = b.len();
                    return Some(Err(Reason::Unterminated));
                };
                self.i = start + n + 1;
                Tok::Name(&self.s[start + 1..start + n], b']')
            }
            c if c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80 => {
                let mut j = start;
                while j < b.len() && {
                    let d = b[j];
                    d.is_ascii_alphanumeric() || d == b'_' || d == b'$' || d >= 0x80
                } {
                    j += 1;
                }
                self.i = j;
                Tok::Word(&self.s[start..j])
            }
            _ => {
                self.i = start + 1;
                Tok::Punct(c)
            }
        };
        Some(Ok(tok))
    }
}

fn is_word(t: Option<&Tok>, w: &str) -> bool {
    matches!(t, Some(Tok::Word(x)) if x.eq_ignore_ascii_case(w))
}

/// A name token's text, quotes undone.
fn name_of(t: &Tok) -> Option<String> {
    match *t {
        Tok::Word(w) => Some(w.to_string()),
        Tok::Name(n, b']') => Some(n.to_string()),
        Tok::Name(n, q) => {
            let q = char::from(q).to_string();
            Some(n.replace(&q.repeat(2), &q))
        }
        _ => None,
    }
}

/// `[schema.]name` at `i`: the name and the index after it.
fn table_at(toks: &[Tok], i: usize) -> Option<(String, usize)> {
    let first = name_of(toks.get(i)?)?;
    if toks.get(i + 1) == Some(&Tok::Punct(b'.')) {
        return Some((name_of(toks.get(i + 2)?)?, i + 3));
    }
    Some((first, i + 1))
}

/// The parenthesised list starting at `toks[i] == '('`: its comma-separated items (top level)
/// and the index after the `)`.
fn group<'t, 'a>(toks: &'t [Tok<'a>], i: usize) -> Option<(Vec<&'t [Tok<'a>]>, usize)> {
    if toks.get(i) != Some(&Tok::Punct(b'(')) {
        return None;
    }
    let mut depth = 0usize;
    let mut items = Vec::new();
    let mut from = i + 1;
    for (j, t) in toks.iter().enumerate().skip(i) {
        match t {
            Tok::Punct(b'(') => depth += 1,
            Tok::Punct(b')') => {
                depth -= 1;
                if depth == 0 {
                    items.push(&toks[from..j]);
                    return Some((items, j + 1));
                }
            }
            Tok::Punct(b',') if depth == 1 => {
                items.push(&toks[from..j]);
                from = j + 1;
            }
            _ => {}
        }
    }
    None
}

/// A value that is one plain string literal.
fn string_value(v: &[Tok]) -> Option<String> {
    match v {
        [Tok::Str(s)] => Some(s.replace("''", "'")),
        _ => None,
    }
}

/// Statements, split at `;` except inside a `CREATE TRIGGER ... END`.
fn statements(text: &str) -> Result<Vec<Vec<Tok<'_>>>, Reason> {
    let mut out = Vec::new();
    let mut cur: Vec<Tok> = Vec::new();
    for t in (Lexer { s: text, i: 0 }) {
        let t = t?;
        if t == Tok::Punct(b';') {
            let trigger = is_word(cur.first(), "CREATE")
                && (is_word(cur.get(1), "TRIGGER")
                    || (is_word(cur.get(1), "TEMP") || is_word(cur.get(1), "TEMPORARY"))
                        && is_word(cur.get(2), "TRIGGER"));
            if trigger && !is_word(cur.last(), "END") {
                cur.push(t);
                continue;
            }
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        cur.push(t);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}

/// What a dump says about the site it came from.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Dump {
    /// `value_json` of the `site_id` setting.
    pub site_id: Option<String>,
    /// Names inserted into `d1_migrations`, in file order.
    pub migrations: Vec<String>,
    /// `(table, rows)` the dump wrote, from its last lines.
    pub counts: Vec<(String, u64)>,
    /// AUTOINCREMENT marks at dump time (`-- sequence` lines).
    pub sequences: Vec<(String, i64)>,
}

/// Column names of a `CREATE TABLE ... (...)`, from the `(` at `i`.
fn columns(toks: &[Tok], i: usize) -> Vec<String> {
    let Some((items, _)) = group(toks, i) else {
        return Vec::new();
    };
    items
        .iter()
        .filter(|item| {
            !["CONSTRAINT", "PRIMARY", "UNIQUE", "CHECK", "FOREIGN"]
                .iter()
                .any(|k| is_word(item.first(), k))
        })
        .filter_map(|item| item.first().and_then(name_of))
        .collect()
}

/// Reads a dump (SQLite text): it must create `d1_migrations` and `settings`; their rows
/// give the migrations and the site id. Column positions come from the column list of each
/// `INSERT`, else from the dump's own `CREATE TABLE`. It must end with the row counts
/// [`crate::dump::write`] lists, which the restore checks after the import.
///
/// # Errors
/// The [`Reason`] it is not a backup.
pub fn parse(text: &str) -> Result<Dump, Reason> {
    let mut dump = Dump::default();
    let mut migrations_cols: Option<Vec<String>> = None;
    let mut settings_cols: Option<Vec<String>> = None;
    for st in statements(text)? {
        if is_word(st.first(), "CREATE") && is_word(st.get(1), "TABLE") {
            let mut i = 2;
            if is_word(st.get(2), "IF") && is_word(st.get(3), "NOT") && is_word(st.get(4), "EXISTS")
            {
                i = 5;
            }
            let Some((name, next)) = table_at(&st, i) else {
                continue;
            };
            if name.eq_ignore_ascii_case(migrations::TABLE) {
                migrations_cols = Some(columns(&st, next));
            } else if name.eq_ignore_ascii_case("settings") {
                settings_cols = Some(columns(&st, next));
            }
            continue;
        }
        if !(is_word(st.first(), "INSERT") || is_word(st.first(), "REPLACE")) {
            continue;
        }
        let Some(into) = st.iter().take(4).position(|t| is_word(Some(t), "INTO")) else {
            continue;
        };
        let Some((name, mut i)) = table_at(&st, into + 1) else {
            continue;
        };
        let is_migrations = name.eq_ignore_ascii_case(migrations::TABLE);
        if !is_migrations && !name.eq_ignore_ascii_case("settings") {
            continue;
        }
        let listed: Option<Vec<String>> = match group(&st, i) {
            Some((items, next)) => {
                i = next;
                Some(
                    items
                        .iter()
                        .map(|c| c.first().and_then(name_of).unwrap_or_default())
                        .collect(),
                )
            }
            None => None,
        };
        let cols = listed
            .or_else(|| {
                if is_migrations {
                    migrations_cols.clone()
                } else {
                    settings_cols.clone()
                }
            })
            .ok_or(Reason::RowBeforeTable)?;
        let pos = |c: &str| cols.iter().position(|x| x.eq_ignore_ascii_case(c));
        if !is_word(st.get(i), "VALUES") {
            return Err(Reason::RowUnreadable);
        }
        i += 1;
        while let Some((values, next)) = group(&st, i) {
            let at = |c: &str| {
                pos(c)
                    .and_then(|p| values.get(p))
                    .and_then(|v| string_value(v))
            };
            if is_migrations {
                dump.migrations
                    .push(at("name").ok_or(Reason::RowUnreadable)?);
            } else if at("key").ok_or(Reason::RowUnreadable)? == SITE_ID_KEY {
                dump.site_id = Some(at("value_json").ok_or(Reason::RowUnreadable)?);
            }
            i = next;
            if st.get(i) != Some(&Tok::Punct(b',')) {
                break;
            }
            i += 1;
        }
    }
    if migrations_cols.is_none() {
        return Err(Reason::NoMigrationsTable);
    }
    if settings_cols.is_none() {
        return Err(Reason::NoSettingsTable);
    }
    dump.counts = crate::dump::counts(text).ok_or(Reason::NoRowCounts)?;
    dump.sequences = crate::dump::sequence_marks(text);
    Ok(dump)
}

/// `2026-11-28T10:15:00Z` from a name starting `20261128T101500Z-`, as backups are named.
#[must_use]
pub fn date_from_name(name: &str) -> Option<String> {
    let s = name.get(..17)?;
    let b = s.as_bytes();
    let digits = |r: std::ops::Range<usize>| b[r].iter().all(u8::is_ascii_digit);
    (digits(0..8) && b[8] == b'T' && digits(9..15) && &s[15..17] == "Z-").then(|| {
        format!(
            "{}-{}-{}T{}:{}:{}Z",
            &s[0..4],
            &s[4..6],
            &s[6..8],
            &s[9..11],
            &s[11..13],
            &s[13..15]
        )
    })
}

/// A quoted SQL name.
/// Tables the Worker empties of expired or spent rows by itself, on its cron or on use
/// (`crates/worker/src/d1`: `auth.rs` sessions, `signin.rs` challenges and login failures,
/// `log.rs` retention, `webhooks.rs` delivered jobs). Fewer rows in them after an import is
/// the site at work, not a lost row: it only warns. Every other table keeps the hard check.
pub const SELF_PRUNING: &[&str] = &[
    "sessions",
    "auth_challenges",
    "login_failures",
    "system_log",
    "webhook_deliveries",
];

/// The command that finishes a restore whose credential reset failed (`blog restore
/// --reset-credentials --yes`, [`reset_credentials`]).
pub const RESET_COMMAND: &str = "blog restore --reset-credentials --yes";

/// What a restore makes unusable, per table: the backup holds every credential as it was
/// then, so one revoked or ended since (a token, a session, an invite, a code) would work
/// again. Tokens are revoked, not deleted, so the owner still sees them. Passwords, passkeys
/// and users stay: changing them would lock the owner out. `login_failures` stays too: it is
/// the rate limiter, and emptying it would only help someone guessing.
const CREDENTIAL_RESET: &[(&str, &str)] = &[
    ("sessions", "DELETE FROM sessions"),
    (
        "api_tokens",
        "UPDATE api_tokens SET revoked_at = ? WHERE revoked_at IS NULL",
    ),
    ("invites", "DELETE FROM invites WHERE used_at IS NULL"),
    // WebAuthn challenges: short-lived, but one taken since the backup would be usable again.
    ("auth_challenges", "DELETE FROM auth_challenges"),
    // The owner reset and setup codes ([`reset_settings`]).
    ("settings", ""),
];

/// The one-time codes kept in settings: every owner reset key and the setup code (which only
/// works while no user exists; `blog setup` makes a new one).
fn reset_settings() -> Vec<&'static str> {
    OWNER_RESET_KEYS
        .iter()
        .copied()
        .chain([SETUP_CODE_HASH_KEY, SETUP_CODE_EXPIRES_KEY])
        .collect()
}

/// The statements of the credential reset for the tables in `present`, in order, as
/// `(sql, params)`; `now` is the revocation time (ms). Public so tools/worker-test replays
/// this exact SQL (crates/cli/tests/restore writes it to a fixture).
#[must_use]
pub fn credential_reset_statements(present: &[&str], now: &str) -> Vec<(String, Vec<String>)> {
    let keys = reset_settings();
    CREDENTIAL_RESET
        .iter()
        .filter(|(t, _)| present.contains(t))
        .map(|(table, sql)| match *table {
            "api_tokens" => ((*sql).to_string(), vec![now.to_string()]),
            "settings" => (
                format!(
                    "DELETE FROM settings WHERE key IN ({})",
                    vec!["?"; keys.len()].join(", ")
                ),
                keys.iter().map(|k| (*k).to_string()).collect(),
            ),
            _ => ((*sql).to_string(), Vec::new()),
        })
        .collect()
}

/// Makes every credential from the backup unusable, in one D1 request after reading which
/// tables exist (an old backup whose migrations failed may lack one). Every statement only
/// removes or revokes, so running it twice changes nothing more.
fn reset_credentials_in(db: &Database, now_ms: i64) -> Result<(), ApiError> {
    // Constant table names, so quoting them as string literals is safe.
    let names: Vec<String> = CREDENTIAL_RESET
        .iter()
        .map(|(t, _)| format!("'{t}'"))
        .collect();
    let present: Vec<String> = db
        .rows(
            &format!(
                "SELECT name FROM sqlite_master WHERE type = 'table' AND name IN ({})",
                names.join(", ")
            ),
            &[],
        )?
        .iter()
        .filter_map(|r| r.get("name").and_then(Value::as_str).map(str::to_string))
        .collect();
    let present: Vec<&str> = present.iter().map(String::as_str).collect();
    let stmts = credential_reset_statements(&present, &now_ms.to_string());
    if stmts.is_empty() {
        return Ok(());
    }
    let params: Vec<Vec<&str>> = stmts
        .iter()
        .map(|(_, v)| v.iter().map(String::as_str).collect())
        .collect();
    let batch: Vec<(&str, &[&str])> = stmts
        .iter()
        .zip(&params)
        .map(|((sql, _), v)| (sql.as_str(), v.as_slice()))
        .collect();
    db.batch(&batch).map(drop)
}

/// What the owner must do after the reset; `date` is the backup's.
fn after_reset(run: &mut Run, date: &str) {
    run.line(key!("cli.restore.credentials_reset"), &[]);
    run.line(
        key!("cli.restore.credentials_next"),
        &[("date", date.to_string())],
    );
}

/// The backup's date, from its file name.
fn backup_date(run: &Run, file: &std::path::Path) -> String {
    date_from_name(
        &file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    )
    .unwrap_or_else(|| run.t(key!("cli.restore.date_unknown"), &[]))
}

/// How far the content version moves past the highest value read before the import: the
/// live site keeps bumping it during the upload and the import, and none of those values may
/// come back for pages cached under them.
const VERSION_MARGIN: i64 = 1000;

fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Statements that drop every object the owner's database has (never SQLite's `sqlite_*` or
/// D1's `_cf_*`): triggers, views, indexes, virtual tables (which take their shadow tables
/// with them, hence `IF EXISTS` after), then tables. Foreign keys are checked only at the end
/// of the import, after the dump has put the rows back. Also returns the live AUTOINCREMENT
/// high-water marks (dropping a table drops its `sqlite_sequence` row).
type Marks = Vec<(String, i64)>;

fn drop_sql(db: &Database) -> Result<(String, Marks), ApiError> {
    let rows = db.rows(
        "SELECT type, name, sql FROM sqlite_master WHERE name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
         AND name NOT LIKE '\\_cf\\_%' ESCAPE '\\' AND type IN ('table', 'view', 'trigger', \
         'index') AND sql IS NOT NULL ORDER BY name",
        &[],
    )?;
    let get = |r: &serde_json::Map<String, Value>, k: &str| {
        r.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let mut out = String::from("PRAGMA defer_foreign_keys=TRUE;\n");
    let virt = |r: &serde_json::Map<String, Value>| {
        get(r, "sql")
            .trim_start()
            .get(..20)
            .is_some_and(|p| p.eq_ignore_ascii_case("CREATE VIRTUAL TABLE"))
    };
    for (ty, verb, only_virtual) in [
        ("trigger", "TRIGGER", None),
        ("view", "VIEW", None),
        ("index", "INDEX", None),
        ("table", "TABLE", Some(true)),
        ("table", "TABLE", Some(false)),
    ] {
        for r in &rows {
            if get(r, "type") != ty || only_virtual.is_some_and(|v| v != virt(r)) {
                continue;
            }
            let _ = writeln!(out, "DROP {verb} IF EXISTS {};", ident(&get(r, "name")));
        }
    }
    Ok((out, crate::dump::sequences(db)?))
}

// --- The run ---

fn fail(run: &Run, code: Code, k: Key, args: &[(&str, String)]) -> Failure {
    Failure::new(code, run.cx.text, k, args)
}

/// Replaces this run's record (found by time and kind) in the file as it is now.
fn put_record(run: &mut Run, rec: &RollbackRecord) -> Result<(), Failure> {
    run.update(|s| {
        match s
            .rollbacks
            .iter_mut()
            .rev()
            .find(|r| r.at == rec.at && r.kind == KIND)
        {
            Some(mine) => *mine = rec.clone(),
            None => push_capped(&mut s.rollbacks, rec.clone()),
        }
    })
}

/// Cloudflare's words from an import reply: `error`, `errors` and `messages`, shown safely.
fn reply_words(run: &Run, r: &Value, hide: &str) -> String {
    let mut words = Vec::new();
    if let Some(e) = str_at(r, "error") {
        words.push(e.to_string());
    }
    for k in ["errors", "messages"] {
        for v in r.get(k).and_then(Value::as_array).into_iter().flatten() {
            let s = v
                .as_str()
                .map(str::to_string)
                .or_else(|| str_at(v, "message").map(str::to_string));
            if let Some(s) = s.filter(|s| !words.contains(s)) {
                words.push(s);
            }
        }
    }
    let text = words.join("; ");
    let text = if hide.is_empty() {
        text
    } else {
        text.replace(hide, "[hidden]")
    };
    printable(&run.client.scrub(&text, &[]))
}

/// Why the upload failed; each is a sentence in `locales/` (`cli.restore.reason_upload_*`).
enum UploadFailure {
    NotHttps,
    NoReply,
    Status(u16),
    /// The HTTP client's own message (never holding the URL): raw detail, not prose.
    Other(String),
}

impl UploadFailure {
    fn render(&self, run: &Run) -> String {
        match self {
            Self::NotHttps => run.t(key!("cli.restore.reason_upload_not_https"), &[]),
            Self::NoReply => run.t(key!("cli.restore.reason_upload_no_reply"), &[]),
            Self::Status(s) => run.t(
                key!("cli.restore.reason_upload_status"),
                &[("status", s.to_string())],
            ),
            Self::Other(s) => s.clone(),
        }
    }
}

/// `PUT`s the bytes to the presigned URL: no token or other credentials, HTTPS only (plain
/// HTTP only to a loopback test server), no redirects. Returns the reply's `ETag` without
/// quotes. The URL is a bearer link, so errors never include it.
fn upload(url: &str, body: &[u8], local: bool) -> Result<Option<String>, UploadFailure> {
    let loopback = base_override_ok(url.split(['?', '#']).next().unwrap_or(""));
    if !(url.starts_with("https://") || local && loopback) {
        return Err(UploadFailure::NotHttps);
    }
    let resp = plain_agent(local, UPLOAD_TIMEOUT, 0)
        .put(url)
        .send(body)
        .map_err(|e| {
            let s = e.to_string();
            if s.contains(url) {
                UploadFailure::NoReply
            } else {
                UploadFailure::Other(s)
            }
        })?;
    let status = resp.status().as_u16();
    if status != 200 {
        return Err(UploadFailure::Status(status));
    }
    Ok(resp
        .headers()
        .get("etag")
        .and_then(|v| v.to_str().ok())
        .map(|v| v.trim().trim_matches('"').to_string()))
}

/// How an import step ended badly; `outcome` goes to the record.
struct Stopped {
    outcome: &'static str,
    failure: Failure,
}

fn stopped(outcome: &'static str, failure: Failure) -> Stopped {
    Stopped { outcome, failure }
}

/// Upload and import `sql`; returns the final reply. Each failure says whether the database
/// may have changed.
fn import_call(site: &Site, body: Value) -> Call {
    Call::new(Endpoint::D1Import)
        .param("database_id", site.db_id.clone())
        .json(body)
}

/// The import may have run: the owner checks the site and has the undo command.
fn uncertain(run: &Run, undo: &str, detail: String) -> Stopped {
    stopped(
        "import_uncertain",
        fail(
            run,
            Code::RestoreImportUncertain,
            key!("cli.error.restore_import_uncertain"),
            &[("detail", detail), ("undo", undo.to_string())],
        ),
    )
}

fn import(
    run: &mut Run,
    site: &Site,
    clock: &dyn Clock,
    sql: &[u8],
    undo: &str,
) -> Result<Value, Stopped> {
    let etag = md5_hex(sql);
    let call = |body: Value| import_call(site, body);
    let init = match run
        .client
        .call(&call(json!({"action": "init", "etag": etag})))
    {
        Ok(r) => r,
        // Without an upload this init may itself have started an import of a file uploaded
        // before (wrangler: "File already uploaded. Processing.").
        Err(e) if e.code() == Code::NetworkWriteUncertain => {
            return Err(uncertain(run, undo, e.message(run.cx.text)));
        }
        Err(e) => return Err(stopped("failed", run.api(&e))),
    };
    let mut hide = String::new();
    let reply = if let Some(url) = str_at(&init, "upload_url") {
        hide = url.to_string();
        let filename = str_at(&init, "filename").unwrap_or_default().to_string();
        run.line(
            key!("cli.restore.uploading"),
            &[("bytes", sql.len().to_string())],
        );
        let got = upload(url, sql, run.client.is_local()).map_err(|failure| {
            let reason = failure.render(run);
            stopped(
                "upload_failed",
                fail(
                    run,
                    Code::RestoreUploadFailed,
                    key!("cli.error.restore_upload_failed"),
                    &[("reason", reason)],
                ),
            )
        })?;
        if !got
            .as_deref()
            .is_some_and(|g| g.eq_ignore_ascii_case(&etag))
        {
            return Err(stopped(
                "upload_failed",
                fail(
                    run,
                    Code::RestoreUploadMismatch,
                    key!("cli.error.restore_upload_mismatch"),
                    &[
                        ("got", printable(got.as_deref().unwrap_or("-"))),
                        ("want", etag.clone()),
                    ],
                ),
            ));
        }
        run.line(key!("cli.restore.importing"), &[]);
        // From here the import may be running: every failure is "may have been applied".
        run.client
            .call(&call(
                json!({"action": "ingest", "filename": filename, "etag": etag}),
            ))
            .map_err(|e| uncertain(run, undo, e.message(run.cx.text)))?
    } else {
        run.line(key!("cli.restore.importing"), &[]);
        init
    };
    poll(run, site, clock, reply, &hide, undo)
}

/// Polls until `complete` or `error`, within [`IMPORT_BUDGET`] (wrangler `pollUntilComplete`:
/// `{action: "poll", current_bookmark: at_bookmark}`). `hide`: the upload URL.
fn poll(
    run: &mut Run,
    site: &Site,
    clock: &dyn Clock,
    mut reply: Value,
    hide: &str,
    undo: &str,
) -> Result<Value, Stopped> {
    let started = clock.elapsed();
    loop {
        let words = || reply_words(run, &reply, hide);
        if reply.get("success").and_then(Value::as_bool) == Some(false)
            || str_at(&reply, "status") == Some("error")
        {
            return Err(stopped(
                "import_failed",
                fail(
                    run,
                    Code::RestoreImportFailed,
                    key!("cli.error.restore_import_failed"),
                    &[("detail", words()), ("undo", undo.to_string())],
                ),
            ));
        }
        if str_at(&reply, "status") == Some("complete") {
            return Ok(reply);
        }
        if clock.elapsed().saturating_sub(started) > IMPORT_BUDGET {
            let detail = run.t(
                key!("cli.restore.import_timeout"),
                &[("minutes", (IMPORT_BUDGET.as_secs() / 60).to_string())],
            );
            return Err(uncertain(run, undo, detail));
        }
        clock.sleep(POLL_WAIT);
        let at = str_at(&reply, "at_bookmark")
            .unwrap_or_default()
            .to_string();
        reply = run
            .client
            .call(&import_call(
                site,
                json!({"action": "poll", "current_bookmark": at}),
            ))
            .map_err(|e| uncertain(run, undo, e.message(run.cx.text)))?;
    }
}

/// Right after the import: every table holds at least the rows the dump wrote. Fewer (a
/// statement D1 skipped, a cut upload) is a mismatch, reported with the undo command; more
/// is the live site writing meanwhile (comments, sessions, the rate limiter, the log).
fn check_counts(
    run: &mut Run,
    db: &Database,
    counts: &[(String, u64)],
    undo: &str,
) -> Result<(), Stopped> {
    let mut pruned = Vec::new();
    for chunk in counts.chunks(50) {
        let cols: Vec<String> = chunk
            .iter()
            .enumerate()
            .map(|(i, (t, _))| format!("(SELECT count(*) FROM {}) AS c{i}", ident(t)))
            .collect();
        let row = db
            .rows(&format!("SELECT {}", cols.join(", ")), &[])
            .map_err(|e| uncertain(run, undo, e.to_string()))?;
        let row = row.first().cloned().unwrap_or_default();
        let mut wrong = Vec::new();
        for (i, (t, want)) in chunk.iter().enumerate() {
            let got = row.get(&format!("c{i}")).and_then(Value::as_u64);
            if got.is_some_and(|g| g >= *want) {
                continue;
            }
            let shown = format!(
                "{t} {want} -> {}",
                got.map_or_else(|| "?".into(), |g| g.to_string())
            );
            if got.is_some() && SELF_PRUNING.contains(&t.as_str()) {
                pruned.push(shown);
            } else {
                wrong.push(shown);
            }
        }
        if !wrong.is_empty() {
            return Err(stopped(
                "count_mismatch",
                fail(
                    run,
                    Code::RestoreCountMismatch,
                    key!("cli.error.restore_count_mismatch"),
                    &[
                        ("tables", printable(&wrong.join(", "))),
                        ("undo", undo.to_string()),
                    ],
                ),
            ));
        }
    }
    if !pruned.is_empty() {
        run.line(
            key!("cli.restore.counts_pruned"),
            &[("tables", printable(&pruned.join(", ")))],
        );
    }
    run.line(
        key!("cli.restore.counts_ok"),
        &[
            ("tables", counts.len().to_string()),
            ("rows", counts.iter().map(|c| c.1).sum::<u64>().to_string()),
        ],
    );
    Ok(())
}

/// After the import: the bundle's migrations the backup lacks, then the cache version, set
/// above `floor`.
fn after_import(
    run: &mut Run,
    db: &Database,
    reply: &Value,
    floor: i64,
    mark: &str,
) -> Result<(), Stopped> {
    let result = reply.get("result").cloned().unwrap_or_default();
    run.line(
        key!("cli.restore.imported"),
        &[
            (
                "queries",
                result
                    .get("num_queries")
                    .map_or_else(|| "?".into(), ToString::to_string),
            ),
            (
                "bookmark",
                printable(str_at(&result, "final_bookmark").unwrap_or("-")),
            ),
        ],
    );
    let as_failed = |f: Failure| stopped("migration_failed", f);
    // The file replaced a half-migrated database too, so a recorded failure no longer
    // describes it.
    let names = migrations::recorded(db).map_err(|e| as_failed(run.api(&e)))?;
    let failed = run.state.migrations.failed.clone();
    run.update(|s| {
        s.migrations.failed = None;
        s.migrations.applied = names;
    })
    .map_err(as_failed)?;
    if let Some(f) = failed {
        run.line(key!("cli.rollback.failed_cleared"), &[("name", f.name)]);
    }
    setup::apply_migrations_undoing_to(run, db, false, Some(mark)).map_err(as_failed)?;
    d1::bump_content_version_above(db, floor).map_err(|e| stopped("restored", run.api(&e)))
}

/// Step 2, read only: the database is ours, the backup is this site's (or `--allow-other-site`)
/// and not newer than the code. Returns the bundle's migrations the backup lacks and the
/// content version now.
fn check_live(
    run: &mut Run,
    db: &Database,
    dump: &Dump,
    b: &Bundle,
    opts: &Options,
    shown: &str,
) -> Result<(Vec<String>, Option<i64>), Failure> {
    migrations::check_ours(db).map_err(|e| setup::migration_error(run, e))?;
    let live_id = d1::settings(db, &[SITE_ID_KEY])
        .map_err(|e| run.api(&e))?
        .into_iter()
        .next()
        .map(|(_, v)| v);
    if dump.site_id.is_none() || dump.site_id != live_id {
        let args = [
            ("path", shown.to_string()),
            ("theirs", printable(dump.site_id.as_deref().unwrap_or("-"))),
            ("ours", printable(live_id.as_deref().unwrap_or("-"))),
        ];
        if !opts.allow_other_site {
            return Err(fail(
                run,
                Code::RestoreOtherSite,
                key!("cli.error.restore_other_site"),
                &args,
            ));
        }
        run.line(key!("cli.restore.other_site_allowed"), &args);
    }
    let unknown: Vec<String> = dump
        .migrations
        .iter()
        .filter(|m| !b.migrations.iter().any(|x| &x.name == *m))
        .map(|m| printable(m))
        .collect();
    if !unknown.is_empty() {
        return Err(fail(
            run,
            Code::RestoreBackupNewer,
            key!("cli.error.restore_backup_newer"),
            &[("path", shown.to_string()), ("list", unknown.join(", "))],
        ));
    }
    let pending: Vec<String> = b
        .migrations
        .iter()
        .filter(|m| !dump.migrations.contains(&m.name))
        .map(|m| m.name.clone())
        .collect();
    let version_before = d1::content_version(db).map_err(|e| run.api(&e))?;
    Ok((pending, version_before))
}

/// Step 3: what the restore will do.
fn plan(
    run: &mut Run,
    opts: &Options,
    shown: &str,
    bytes: usize,
    db_name: &str,
    pending: &[String],
) {
    let date = backup_date(run, &opts.file);
    run.line(
        key!("cli.restore.plan_file"),
        &[
            ("file", shown.to_string()),
            ("bytes", bytes.to_string()),
            ("date", date),
        ],
    );
    run.line(
        key!("cli.restore.plan_replace"),
        &[("database", db_name.to_string())],
    );
    if pending.is_empty() {
        run.line(key!("cli.restore.plan_no_pending"), &[]);
    } else {
        run.line(
            key!("cli.restore.plan_pending"),
            &[("list", pending.join(", "))],
        );
    }
}

/// Step 1: the file, read and checked locally.
fn read_file(cx: &Context, opts: &Options, shown: &str) -> Result<(Vec<u8>, Dump), Failure> {
    let bytes = std::fs::read(cx.dir.join(&opts.file)).map_err(|e| {
        Failure::new(
            Code::RestoreUnreadable,
            cx.text,
            key!("cli.error.restore_unreadable"),
            &[("path", shown.to_string()), ("reason", e.to_string())],
        )
    })?;
    let not_backup = |r: Reason| {
        Failure::new(
            Code::RestoreNotABackup,
            cx.text,
            key!("cli.error.restore_not_a_backup"),
            &[
                ("path", shown.to_string()),
                ("reason", cx.text.t(r.key(), &[])),
            ],
        )
    };
    let text = std::str::from_utf8(&bytes).map_err(|_| not_backup(Reason::NotText))?;
    let dump = parse(text).map_err(not_backup)?;
    Ok((bytes, dump))
}

/// Steps 5-8: the import, the row counts, migrations, the content version and the
/// credential reset.
fn replace(
    run: &mut Run,
    db: &Database,
    site: &Site,
    clock: &dyn Clock,
    (bytes, dump): (&[u8], &Dump),
    version_before: Option<i64>,
    (mark, undo, date): (&str, &str, &str),
) -> Result<(), Stopped> {
    let (drops, sequences) = drop_sql(db).map_err(|e| stopped("failed", run.api(&e)))?;
    // Read again just before the import: the site kept running since step 2.
    let now = d1::content_version(db).map_err(|e| stopped("failed", run.api(&e)))?;
    let floor = version_before
        .max(now)
        .unwrap_or(0)
        .saturating_add(VERSION_MARGIN);
    let mut sql = drops.into_bytes();
    sql.extend_from_slice(bytes);
    let reply = import(run, site, clock, &sql, undo)?;
    // Migrations and the version run even when rows are missing, so the database matches
    // the code and no stale page is served until the owner undoes the restore.
    let counted = check_counts(run, db, &dump.counts, undo);
    raise_sequences(run, db, &sequences, &dump.sequences);
    let after = after_import(run, db, &reply, floor, mark);
    // Whatever failed above, the backup's credentials are live now: always reset them, last,
    // so every table the migrations create exists.
    let reset = match reset_credentials_in(db, run.cx.now_ms) {
        Ok(()) => {
            after_reset(run, date);
            Ok(())
        }
        Err(e) => Err(stopped(
            "credentials_not_reset",
            fail(
                run,
                Code::RestoreCredentialsNotReset,
                key!("cli.error.restore_credentials_not_reset"),
                &[
                    ("detail", printable(&e.message(run.cx.text))),
                    ("command", RESET_COMMAND.to_string()),
                    ("undo", undo.to_string()),
                ],
            ),
        )),
    };
    // The first failure is the answer; the others are printed.
    let mut first = None;
    for r in [counted, after, reset] {
        let Err(s) = r else { continue };
        if first.is_none() {
            first = Some(s);
        } else {
            let f = &s.failure;
            let _ = writeln!(run.out, "{}", run.cx.text.error_line(f.code, &f.message));
        }
    }
    first.map_or(Ok(()), Err)
}

/// `blog restore --reset-credentials --yes`: only the last step of a restore, for one whose
/// reset failed (or whose import was uncertain but applied). Safe to repeat.
///
/// # Errors
/// [`Failure`] with a stable code; `restore.credentials_not_reset` when the write fails.
pub fn reset_credentials(
    client: &mut Client,
    account_id: Option<&str>,
    cx: &Context,
    out: &mut dyn Write,
) -> Result<(), Failure> {
    let _lock = Lock::take(cx)?;
    let (st, site) = start(client, account_id, cx)?;
    let client: &Client = client;
    let mut run = Run {
        client,
        cx,
        out,
        state: st,
        secrets: Vec::new(),
    };
    let db = Database::new(client, site.db_id.clone());
    migrations::check_ours(&db).map_err(|e| setup::migration_error(&run, e))?;
    let last = run
        .state
        .rollbacks
        .iter()
        .rev()
        .find(|r| r.kind == KIND)
        .cloned();
    let date = last.as_ref().map_or_else(
        || run.t(key!("cli.restore.date_unknown"), &[]),
        |r| backup_date(&run, std::path::Path::new(&r.to)),
    );
    reset_credentials_in(&db, cx.now_ms).map_err(|e| {
        fail(
            &run,
            Code::RestoreCredentialsNotReset,
            key!("cli.error.restore_reset_failed"),
            &[
                ("detail", printable(&e.message(cx.text))),
                ("command", RESET_COMMAND.to_string()),
            ],
        )
    })?;
    after_reset(&mut run, &date);
    if let Some(mut rec) = last.filter(|r| r.outcome.as_deref() == Some("credentials_not_reset")) {
        rec.outcome = Some("restored".into());
        put_record(&mut run, &rec)?;
    }
    Ok(())
}

/// Raises every AUTOINCREMENT mark to the higher of its live value before the import and the
/// dump's, so ids handed out since the backup are never reused. Outside the import, because
/// D1 may refuse writes to `sqlite_sequence`: a refusal only warns (printed and logged), the
/// restored data stands.
fn raise_sequences(run: &mut Run, db: &Database, live: &[(String, i64)], dumped: &[(String, i64)]) {
    let mut marks: std::collections::BTreeMap<&str, i64> = std::collections::BTreeMap::new();
    for (t, seq) in live.iter().chain(dumped) {
        let m = marks.entry(t.as_str()).or_insert(*seq);
        *m = (*m).max(*seq);
    }
    if marks.is_empty() {
        return;
    }
    let sql: Vec<String> = marks
        .iter()
        .flat_map(|(t, seq)| crate::dump::raise_sequence(t, *seq))
        .collect();
    let stmts: Vec<(&str, &[&str])> = sql.iter().map(|s| (s.as_str(), &[][..])).collect();
    let Err(e) = db.batch(&stmts) else {
        return;
    };
    let detail = printable(&e.message(run.cx.text));
    let warning = run.t(
        key!("cli.restore.sequence_warning"),
        &[("detail", detail.clone())],
    );
    let _ = writeln!(run.out, "{warning}");
    let event = Event::new(
        EventKind::Migration,
        request_id("restore", run.cx.now_ms),
        at_ms(run.cx),
    )
    .subject(&format!(
        "restore: AUTOINCREMENT marks not raised, ids may be reused: {detail}"
    ));
    log_events(run, db, &[event]);
}

/// `blog restore`. Progress goes to `out` as each step finishes.
///
/// # Errors
/// [`Failure`] with a stable code; `restore.unconfirmed` (no `--yes`) after the plan.
pub fn run(
    client: &mut Client,
    account_id: Option<&str>,
    opts: &Options,
    cx: &Context,
    clock: &dyn Clock,
    out: &mut dyn Write,
) -> Result<(), Failure> {
    let deploy_opts = deploy::Options {
        bundle: opts.bundle.clone(),
        build: opts.build,
        release: opts.release.clone(),
        ..deploy::Options::default()
    };
    let _lock = Lock::take(cx)?;
    // 1. The file, locally.
    let shown = opts.file.display().to_string();
    let (bytes, dump) = read_file(cx, opts, &shown)?;
    deploy::check_source(&deploy_opts, cx.text)?;

    // 2. The live site and the code, read only.
    let (st, site) = start(client, account_id, cx)?;
    let client: &Client = client;
    let mut first = Run {
        client,
        cx,
        out,
        state: st,
        secrets: Vec::new(),
    };
    let (_, b) = deploy::bundle(&mut first, &deploy_opts)?;
    let bcx = Context {
        dir: cx.dir,
        migrations: &b.migrations,
        now_ms: cx.now_ms,
        text: cx.text,
    };
    let Run { out, state, .. } = first;
    let mut run = Run {
        client,
        cx: &bcx,
        out,
        state,
        secrets: Vec::new(),
    };
    let db = Database::new(client, site.db_id.clone());
    let (pending, version_before) = check_live(&mut run, &db, &dump, &b, opts, &shown)?;

    // 3. The plan.
    plan(&mut run, opts, &shown, bytes.len(), &site.db_name, &pending);
    if !opts.yes {
        return Err(fail(
            &run,
            Code::RestoreUnconfirmed,
            key!("cli.error.restore_unconfirmed"),
            &[("path", shown)],
        ));
    }

    // 4. The bookmark, saved before anything changes.
    let mark = migrations::bookmark(&db, client).map_err(|e| run.api(&e))?;
    let undo = setup::ROLLBACK_COMMAND.replace("{bookmark}", &mark);
    let (at, _) = utc(cx.now_ms);
    let mut rec = RollbackRecord {
        at,
        kind: KIND.into(),
        to: shown.clone(),
        previous: Some(mark.clone()),
        smoke: None,
        outcome: Some("started".into()),
    };
    run.update(|s| push_capped(&mut s.rollbacks, rec.clone()))?;
    run.line(key!("cli.restore.bookmark"), &[("bookmark", mark.clone())]);
    run.line(key!("cli.restore.undo"), &[("command", undo.clone())]);
    run.line(key!("cli.restore.cancels"), &[]);

    // 5-8. Import, migrations, cache version, credentials.
    let date = backup_date(&run, &opts.file);
    let result = replace(
        &mut run,
        &db,
        &site,
        clock,
        (&bytes, &dump),
        version_before,
        (&mark, &undo, &date),
    );
    rec.outcome = Some(match &result {
        Ok(()) => "restored".into(),
        Err(s) => s.outcome.into(),
    });
    let saved = put_record(&mut run, &rec);
    if rec.outcome.as_deref() != Some("failed") {
        let event = Event::new(
            EventKind::Migration,
            request_id("restore", cx.now_ms),
            at_ms(cx),
        )
        .subject(&format!(
            "database restored from {shown} {} (undo: bookmark {mark})",
            rec.outcome.as_deref().unwrap_or("-")
        ));
        log_events(&mut run, &db, &[event]);
    }
    if let Err(s) = result {
        if let Err(e) = saved {
            let _ = writeln!(run.out, "{}", cx.text.error_line(e.code, &e.message));
        }
        return Err(s.failure);
    }
    saved?;
    run.line(key!("cli.restore.done"), &[("file", shown)]);
    run.line(key!("cli.restore.undo"), &[("command", undo)]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_site_id_and_migrations_in_either_insert_form() {
        let text = "PRAGMA defer_foreign_keys=TRUE;\n\
            CREATE TABLE IF NOT EXISTS \"d1_migrations\"(\n id INTEGER PRIMARY KEY, name TEXT \
            UNIQUE, applied_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP NOT NULL);\n\
            INSERT INTO \"d1_migrations\" VALUES(1,'0001_init.sql','2026-11-01 08:00:00');\n\
            INSERT INTO d1_migrations (applied_at, name, id) VALUES ('x', '0002_a;b.sql', 2);\n\
            CREATE TABLE settings (\n  key TEXT PRIMARY KEY, -- a comment with 'quote\n  \
            value_json TEXT NOT NULL, updated_at INTEGER NOT NULL, PRIMARY KEY (key));\n\
            INSERT INTO \"settings\" VALUES('origin','\"https://a.b\"',1),\
            ('site_id','\"ab''c\"',1);\n\
            CREATE TRIGGER t AFTER INSERT ON settings BEGIN \
              INSERT INTO settings VALUES('site_id','\"no\"',1); END;\n\
            /* INSERT INTO settings VALUES('site_id','\"no\"',1); */\n\
            -- rows \"d1_migrations\" 2\n-- rows \"settings\" 2\n-- end of plumblog dump\n";
        let d = parse(text).unwrap();
        assert_eq!(
            d.counts,
            [
                ("d1_migrations".to_string(), 2),
                ("settings".to_string(), 2)
            ]
        );
        assert_eq!(d.migrations, ["0001_init.sql", "0002_a;b.sql"]);
        assert_eq!(d.site_id.as_deref(), Some("\"ab'c\""));
    }

    #[test]
    fn refuses_what_is_not_a_backup() {
        assert_eq!(parse("hello"), Err(Reason::NoMigrationsTable));
        assert_eq!(
            parse("CREATE TABLE d1_migrations (id, name, applied_at);"),
            Err(Reason::NoSettingsTable)
        );
        assert_eq!(
            parse("INSERT INTO settings VALUES('a','b',1);"),
            Err(Reason::RowBeforeTable)
        );
        assert_eq!(
            parse("CREATE TABLE settings (key, value_json); INSERT INTO settings VALUES('a"),
            Err(Reason::Unterminated)
        );
        assert_eq!(
            parse(
                "CREATE TABLE d1_migrations (id, name); CREATE TABLE settings (key, value_json);\
                 INSERT INTO d1_migrations VALUES(1, 2);"
            ),
            Err(Reason::RowUnreadable)
        );
        // A cut file: everything but the row counts at the end.
        assert_eq!(
            parse(
                "CREATE TABLE d1_migrations (id, name); CREATE TABLE settings (key, value_json);"
            ),
            Err(Reason::NoRowCounts)
        );
    }

    #[test]
    fn dates_come_from_backup_names_only() {
        assert_eq!(
            date_from_name("20261101T080000Z-plumblog.sql").as_deref(),
            Some("2026-11-01T08:00:00Z")
        );
        assert_eq!(date_from_name("backup.sql"), None);
        assert_eq!(date_from_name("2026110xT080000Z-a.sql"), None);
        assert_eq!(date_from_name("20261101T080000Z.sql"), None);
    }

    #[test]
    fn self_pruning_tables_exist() {
        let schema: String = crate::migrations::bundled()
            .into_iter()
            .map(|m| m.sql)
            .collect();
        for t in SELF_PRUNING {
            assert!(
                schema.contains(&format!("TABLE {t} ("))
                    || schema.contains(&format!("EXISTS {t} (")),
                "{t}"
            );
        }
    }

    #[test]
    fn names_are_quoted() {
        assert_eq!(ident("a\"b"), "\"a\"\"b\"");
    }
}
