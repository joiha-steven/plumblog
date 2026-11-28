//! A fake Cloudflare API for tests: a `TcpListener` on 127.0.0.1 in a thread, minimal
//! HTTP/1.1 (one request per connection), in-memory state, every request recorded.
//!
//! Extend it by adding fields to [`State`] and arms to [`route`]; each arm checks the
//! request shape and returns the Cloudflare envelope. D1 query runs the SQL in a real
//! in-memory SQLite per database (rusqlite), so tests can read the rows the Worker would.

use rusqlite::Connection;
use rusqlite::types::ValueRef;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Owner {
    User,
    /// Account-owned token: verifies only under this account and sees only it.
    Account(String),
}

#[derive(Clone, Debug)]
pub struct Token {
    /// `active`, `disabled` or `expired`, as verify reports it.
    pub status: &'static str,
    pub owner: Owner,
    /// Dashboard labels, e.g. `Account | D1 | Edit`.
    pub permissions: Vec<&'static str>,
    pub expires_on: Option<&'static str>,
}

impl Token {
    pub fn user(permissions: &[&'static str]) -> Self {
        Self {
            status: "active",
            owner: Owner::User,
            permissions: permissions.to_vec(),
            expires_on: None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Failure {
    /// Reply with this status and a Cloudflare error envelope.
    Status(u16),
    /// 200 with a body that is not JSON.
    Malformed,
    /// Close the connection without a reply.
    Drop,
    /// 400 whose error message repeats the request's Authorization header.
    EchoToken,
    /// 400 whose error message repeats the request body.
    EchoBody,
    /// 502 with an HTML body holding the bearer value from character `n` on.
    HtmlWithToken(usize),
    /// Do the request, then close the connection without a reply (a lost reply).
    DropAfter,
}

#[derive(Clone, Debug)]
pub struct Recorded {
    pub method: String,
    pub path: String,
    pub query: String,
    /// Lower-case names.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl Recorded {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }
}

/// A Worker script.
#[derive(Clone, Debug, Default)]
pub struct Script {
    pub secrets: HashMap<String, String>,
    pub workers_dev: bool,
    pub uploads: usize,
    /// Version ids, oldest first.
    pub versions: Vec<String>,
    /// Deployments, newest first: the version id at 100%.
    pub deployments: Vec<String>,
    /// The last `PUT .../schedules` body.
    pub schedules: Option<Value>,
}

/// A Worker version as uploaded: its metadata part and module part names.
#[derive(Clone, Debug)]
pub struct Version {
    pub id: String,
    pub metadata: Value,
    pub modules: Vec<String>,
}

/// The smoke target the fake serves at `/healthz` and `/` (no auth, like a public site).
#[derive(Clone, Debug, Default)]
pub struct Site {
    /// Script whose active version `/healthz` reports.
    pub script: String,
    /// The first n `/healthz` requests answer 404 (a route still warming up).
    pub ready_after: usize,
    /// Report this version instead of the active one.
    pub version_override: Option<String>,
    /// `/` answers this status (default 200) while the given version is active.
    pub home_status: Option<(String, u16)>,
    pub hits: usize,
    /// `/healthz` answers only after this long (a slow site).
    pub delay: std::time::Duration,
    /// Hosts (first path segment, `<base>/<host>/healthz`) that answer 526, like a custom
    /// domain whose certificate is not ready.
    pub down_hosts: Vec<String>,
    /// The host of every public request, in order.
    pub hosts: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct Widget {
    pub sitekey: String,
    pub secret: String,
    pub name: String,
    pub domains: Vec<String>,
    pub mode: String,
}

#[derive(Default)]
pub struct State {
    /// `(id, name)`; user tokens see them all.
    pub accounts: Vec<(String, String)>,
    pub tokens: HashMap<String, Token>,
    /// D1 databases: `{uuid, name}`.
    pub d1: Vec<Value>,
    /// R2 bucket names; listed with a cursor, as R2 does.
    pub r2: Vec<String>,
    /// `<bucket>/<key>` -> object, read with `GET .../r2/buckets/<bucket>/objects/<key>`.
    pub objects: HashMap<String, Object>,
    pub requests: Vec<Recorded>,
    /// `(n, failure)`: the n-th request (1-based, counting every request) fails.
    pub failures: Vec<(usize, Failure)>,
    /// `(pattern, failure)`: the first request whose `METHOD /path` contains `pattern`
    /// fails, once.
    pub fail_on: Vec<(String, Failure)>,
    /// `(id, sqlite)` per D1 database.
    pub sqlite: HashMap<String, Connection>,
    /// Bookmarks handed out, in order.
    pub bookmarks: Vec<String>,
    /// The account's workers.dev subdomain.
    pub subdomain: Option<String>,
    pub scripts: HashMap<String, Script>,
    /// Zones: `{id, name, account: {id}}`.
    pub zones: Vec<Value>,
    /// Worker custom domains: `{id, hostname, service, zone_id}`.
    pub domains: Vec<Value>,
    pub widgets: Vec<Widget>,
    /// `(trigger, sql)`: after the first D1 query whose body contains `trigger`, run `sql`
    /// in that database, once (another client acting between two of ours).
    pub after_query: Vec<(String, String)>,
    pub versions: Vec<Version>,
    /// Asset hashes Cloudflare already has.
    pub assets: std::collections::HashSet<String>,
    /// Upload session JWT -> hashes still missing.
    pub sessions: HashMap<String, Vec<String>>,
    /// Completion JWTs handed out.
    pub completions: Vec<String>,
    /// Restores done: `(database id, bookmark)`.
    pub restores: Vec<(String, String)>,
    pub site: Site,
    /// The fake's own base URL (presigned URLs point at it).
    pub base: String,
    /// Manifest path of each asset hash an upload session asked for.
    pub asset_paths: HashMap<String, String>,
    /// Replaces the result of `GET .../deployments` (a malformed reply).
    pub deployments_override: Option<Value>,
    /// `Some(n)`: after a `POST .../deployments` beyond the first n requests, every
    /// `GET .../deployments` gets no reply.
    pub blind_after_switch: Option<usize>,
    /// `(pattern, failure)`: the first request whose body contains `pattern` fails, once.
    pub fail_body: Vec<(String, Failure)>,
    /// `(pattern, action)`: before answering the first request whose `METHOD /path` contains
    /// `pattern`, run `action` once (e.g. someone editing site.local.toml meanwhile).
    pub on_request: Vec<(String, Box<dyn FnOnce() + Send>)>,
    /// D1 import uploads by file name: `(database id, etag from init, bytes once PUT)`.
    pub uploads: HashMap<String, (String, String, Option<String>)>,
    /// Replaces the `ETag` an upload answers with (bytes damaged on the way).
    pub upload_etag: Option<String>,
    /// The next upload answers this status, keeping nothing.
    pub upload_status: Option<u16>,
    /// Refuse any write to `sqlite_sequence`, in queries and imports (D1 may).
    pub refuse_sequence_writes: bool,
    /// The next import fails with this error, applying nothing.
    pub import_error: Option<String>,
    /// Polls an import answers `active` before its result.
    pub import_polls: usize,
    /// `(database id, polls so far, final reply)` of the running import.
    pub importing: Option<(String, usize, Value)>,
    /// Run after the next import succeeds, in the same database: a row lost on the way.
    pub after_import: Option<String>,
    /// `(request number, reply body bytes)` of every API reply.
    pub reply_bytes: Vec<(usize, usize)>,
}

/// An R2 object: bytes (text, for the tests) and the `ETag` header it answers with.
#[derive(Clone, Debug)]
pub struct Object {
    pub body: String,
    pub etag: Option<String>,
}

pub struct Fake {
    pub base: String,
    state: Arc<Mutex<State>>,
}

impl Fake {
    pub fn start(state: State) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let mut state = state;
        state.base.clone_from(&base);
        // D1 enforces foreign keys on every query and import; a test's own database too.
        for conn in state.sqlite.values() {
            conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
        }
        let state = Arc::new(Mutex::new(state));
        let shared = Arc::clone(&state);
        // Never joined: the thread ends with the test process.
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                // One thread per connection, so a slow reply (`Site::delay`) never holds up
                // the next request; the state lock keeps requests in arrival order.
                let shared = Arc::clone(&shared);
                thread::spawn(move || serve(&stream, &shared));
            }
        });
        Self { base, state }
    }

    pub fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }

    pub fn requests(&self) -> Vec<Recorded> {
        self.state().requests.clone()
    }

    pub fn fail(&self, nth: usize, failure: Failure) {
        self.state().failures.push((nth, failure));
    }

    /// The next request matching `pattern` (`METHOD /path` substring) fails once.
    pub fn fail_on(&self, pattern: &str, failure: Failure) {
        self.state().fail_on.push((pattern.to_string(), failure));
    }

    /// The next request whose body contains `pattern` fails once.
    pub fn fail_body(&self, pattern: &str, failure: Failure) {
        self.state().fail_body.push((pattern.to_string(), failure));
    }

    /// Runs `action` before answering the next request matching `pattern`.
    pub fn on_request(&self, pattern: &str, action: impl FnOnce() + Send + 'static) {
        self.state()
            .on_request
            .push((pattern.to_string(), Box::new(action)));
    }

    /// Rows of `sql` in database `id`, as JSON objects.
    pub fn sql(&self, id: &str, sql: &str) -> Vec<Map<String, Value>> {
        let st = self.state();
        let conn = st.sqlite.get(id).expect("no such database");
        run_rows(conn, sql, &[]).expect("query failed")
    }

    /// Runs `sql` (several statements allowed) in database `id`.
    pub fn exec(&self, id: &str, sql: &str) {
        let st = self.state();
        st.sqlite
            .get(id)
            .expect("no such database")
            .execute_batch(sql)
            .unwrap();
    }
}

fn read_request(stream: &TcpStream) -> Option<Recorded> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?.to_string();
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        reader.read_line(&mut h).ok()?;
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        let (n, v) = h.split_once(':')?;
        headers.push((n.trim().to_ascii_lowercase(), v.trim().to_string()));
    }
    let len: usize = headers
        .iter()
        .find(|(n, _)| n == "content-length")
        .and_then(|(_, v)| v.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; len];
    reader.read_exact(&mut body).ok()?;
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    Some(Recorded {
        method,
        path: path.to_string(),
        query: query.to_string(),
        headers,
        body: String::from_utf8_lossy(&body).into_owned(),
    })
}

/// `extra`: more header lines, each ending in `\r\n`.
fn write_reply_with(mut stream: &TcpStream, status: u16, body: &str, extra: &str) {
    let reply = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         {extra}Connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(reply.as_bytes());
}

/// The public side: the site's `/healthz` and `/`, and presigned upload URLs. No token.
fn serve_public(st: &mut State, req: &Recorded) -> Option<(u16, String, String)> {
    let active = |st: &State| {
        st.scripts
            .get(&st.site.script)
            .and_then(|s| s.deployments.first().cloned())
    };
    // `<base>/<host>/healthz`: the CLI's test smoke URL names the host it means.
    let mut path = req.path.as_str();
    if let Some((host, rest)) = path[1..].split_once('/')
        && host.contains('.')
        && !host.starts_with("__")
    {
        st.site.hosts.push(host.to_string());
        if st.site.down_hosts.iter().any(|h| h == host) {
            return Some((526, "invalid certificate".into(), String::new()));
        }
        path = &req.path[host.len() + 1..];
        let _ = rest;
    }
    match (req.method.as_str(), path) {
        ("GET", "/healthz") => {
            st.site.hits += 1;
            if st.site.hits <= st.site.ready_after {
                return Some((404, "not yet".into(), String::new()));
            }
            let v = st.site.version_override.clone().or_else(|| active(st));
            let header = v
                .map(|v| format!("x-plumblog-version: {v}\r\n"))
                .unwrap_or_default();
            Some((200, "ok".into(), header))
        }
        ("GET", "/") => {
            let status = match (&st.site.home_status, active(st)) {
                (Some((v, s)), Some(a)) if *v == a => *s,
                _ => 200,
            };
            Some((status, "<html></html>".into(), String::new()))
        }
        ("PUT", p) if p.starts_with("/__upload/") => Some(upload(st, req, p)),
        _ => None,
    }
}

fn serve(stream: &TcpStream, state: &Mutex<State>) {
    let Some(req) = read_request(stream) else {
        return;
    };
    let mut st = state.lock().unwrap();
    st.requests.push(req.clone());
    let n = st.requests.len();
    if let Some((status, body, extra)) = serve_public(&mut st, &req) {
        let delay = if req.path.ends_with("/healthz") {
            st.site.delay
        } else {
            std::time::Duration::ZERO
        };
        drop(st);
        thread::sleep(delay);
        write_reply_with(stream, status, &body, &extra);
        return;
    }
    let mut failure = st.failures.iter().find(|(i, _)| *i == n).map(|(_, f)| *f);
    let from = st.blind_after_switch.unwrap_or(n - 1).min(n - 1);
    let switched = st.requests[from..n - 1]
        .iter()
        .any(|r| r.method == "POST" && r.path.ends_with("/deployments"));
    if switched && req.method == "GET" && req.path.ends_with("/deployments") {
        failure = Some(Failure::Drop);
    }
    let line = format!("{} {}", req.method, req.path);
    if let Some(i) = st
        .on_request
        .iter()
        .position(|(p, _)| line.contains(p.as_str()))
    {
        let (_, action) = st.on_request.remove(i);
        action();
    }
    if failure.is_none()
        && let Some(i) = st
            .fail_body
            .iter()
            .position(|(p, _)| req.body.contains(p.as_str()))
    {
        failure = Some(st.fail_body.remove(i).1);
    }
    if failure.is_none()
        && let Some(i) = st
            .fail_on
            .iter()
            .position(|(p, _)| line.contains(p.as_str()))
    {
        failure = Some(st.fail_on.remove(i).1);
    }
    let mut extra = String::new();
    let (status, body) = match failure {
        Some(Failure::Drop) => return,
        Some(Failure::DropAfter) => {
            let _ = route(&mut st, &req);
            return;
        }
        Some(Failure::Malformed) => (200, "<html>not json".to_string()),
        Some(Failure::Status(s)) => (s, err(s, 10013, "injected failure").1.to_string()),
        Some(Failure::EchoToken) => {
            let auth = req.header("authorization").unwrap_or("").to_string();
            (
                400,
                err(400, 6003, &format!("Invalid request headers: {auth}"))
                    .1
                    .to_string(),
            )
        }
        Some(Failure::EchoBody) => (
            400,
            err(400, 1001, &format!("Bad body: {}", req.body))
                .1
                .to_string(),
        ),
        Some(Failure::HtmlWithToken(n)) => {
            let auth = req.header("authorization").unwrap_or("");
            let bearer = auth.strip_prefix("Bearer ").unwrap_or(auth);
            (502, format!("{}{bearer}</pre></html>", "x".repeat(n)))
        }
        None => {
            if let Some((s, b, e)) = r2_object(&st, &req) {
                extra = e;
                (s, b)
            } else {
                let (s, v) = route(&mut st, &req);
                (s, v.to_string())
            }
        }
    };
    st.reply_bytes.push((n, body.len()));
    drop(st);
    write_reply_with(stream, status, &body, &extra);
}

fn ok(result: &Value) -> (u16, Value) {
    (
        200,
        json!({"success": true, "errors": [], "messages": [], "result": result}),
    )
}

fn err(status: u16, code: i64, message: &str) -> (u16, Value) {
    (
        status,
        json!({"success": false, "errors": [{"code": code, "message": message}], "messages": [], "result": null}),
    )
}

/// Accounts a token can see.
fn visible(st: &State, token: &Token) -> Vec<(String, String)> {
    st.accounts
        .iter()
        .filter(|(id, _)| match &token.owner {
            Owner::User => true,
            Owner::Account(own) => own == id,
        })
        .cloned()
        .collect()
}

fn route(st: &mut State, req: &Recorded) -> (u16, Value) {
    if req.method == "POST" && req.path.ends_with("/workers/assets/upload") {
        return assets_upload(st, req);
    }
    let Some(token) = req
        .header("authorization")
        .and_then(|a| a.strip_prefix("Bearer "))
        .and_then(|t| st.tokens.get(t))
        .cloned()
    else {
        return err(401, 10000, "Authentication error");
    };
    if req.method == "GET" && !req.body.is_empty() {
        return err(400, 1001, "GET with a body");
    }
    let segs: Vec<&str> = req.path.split('/').skip(1).collect();

    // Verify answers for any known token; every other call needs an active one.
    match (req.method.as_str(), segs.as_slice()) {
        ("GET", ["user", "tokens", "verify"]) => {
            return if token.owner == Owner::User {
                ok(&json!({"id": "t1", "status": token.status, "expires_on": token.expires_on}))
            } else {
                err(401, 1000, "Invalid API Token")
            };
        }
        ("GET", ["accounts", a, "tokens", "verify"]) => {
            return if token.owner == Owner::Account((*a).to_string()) {
                ok(&json!({"id": "t1", "status": token.status, "expires_on": token.expires_on}))
            } else {
                err(401, 1000, "Invalid API Token")
            };
        }
        _ => {}
    }
    if token.status != "active" {
        return err(401, 1000, "Invalid API Token");
    }
    let need = |label: &str| token.permissions.contains(&label);
    let can_see = |a: &str| visible(st, &token).iter().any(|(id, _)| id == a);
    match (req.method.as_str(), segs.as_slice()) {
        ("GET", ["accounts"]) => {
            let list: Vec<Value> = visible(st, &token)
                .into_iter()
                .map(|(id, name)| json!({"id": id, "name": name}))
                .collect();
            paged(&list, &req.query)
        }
        (_, ["accounts", a, ..]) if !can_see(a) => {
            err(403, 9109, "Unauthorized to access requested resource")
        }
        (_, ["accounts", _, "d1", ..]) => route_d1(st, req, &segs, &need),
        (_, ["accounts", _, "r2", ..]) => route_r2(st, req, &segs, &need),
        (_, ["accounts", _, "workers", ..]) => route_workers(st, req, &segs, &need),
        (_, ["zones", ..]) => route_zones(st, req, &segs, &need),
        (_, ["accounts", _, "challenges", ..]) => route_turnstile(st, req, &segs, &need),
        _ => err(404, 7003, "Could not route to this path"),
    }
}

/// D1: databases, query (real SQLite), Time Travel bookmarks.
fn route_d1(
    st: &mut State,
    req: &Recorded,
    segs: &[&str],
    need: &dyn Fn(&str) -> bool,
) -> (u16, Value) {
    match (req.method.as_str(), segs) {
        (_, ["accounts", _, "d1", ..]) if !need("Account | D1 | Edit") => {
            err(403, 10000, "Authentication error")
        }
        ("GET", ["accounts", _, "d1", "database"]) => paged(&st.d1, &req.query),
        ("POST", ["accounts", _, "d1", "database"]) => {
            if req.header("content-type") != Some("application/json") {
                return err(400, 1001, "expected application/json");
            }
            let body: Value = serde_json::from_str(&req.body).unwrap_or_default();
            let Some(name) = body.get("name").and_then(Value::as_str) else {
                return err(400, 1001, "name is required");
            };
            if st.d1.iter().any(|d| d["name"] == name) {
                return err(400, 7502, "A database with that name already exists");
            }
            let id = format!("db-{}", st.d1.len() + 1);
            let db = json!({"uuid": id, "name": name});
            st.d1.push(db.clone());
            let conn = Connection::open_in_memory().unwrap();
            conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
            st.sqlite.insert(id, conn);
            ok(&db)
        }
        (
            "GET",
            [
                "accounts",
                _,
                "d1",
                "database",
                id,
                "time_travel",
                "bookmark",
            ],
        ) => {
            if !st.sqlite.contains_key(*id) {
                return err(404, 7404, "database not found");
            }
            let b = format!("bm-{}-{}", id, st.bookmarks.len() + 1);
            st.bookmarks.push(b.clone());
            ok(&json!({"bookmark": b}))
        }
        ("POST", ["accounts", _, "d1", "database", id, "import"]) => d1_import(st, req, id),
        (
            "POST",
            [
                "accounts",
                _,
                "d1",
                "database",
                id,
                "time_travel",
                "restore",
            ],
        ) => d1_restore(st, req, id),
        ("POST", ["accounts", _, "d1", "database", id, "query"]) => {
            let body: Value = serde_json::from_str(&req.body).unwrap_or_default();
            let Some(conn) = st.sqlite.get(*id) else {
                return err(404, 7404, "database not found");
            };
            if st.refuse_sequence_writes && writes_sequence(&req.body) {
                return err(
                    400,
                    7500,
                    "not authorized to use sqlite_sequence: SQLITE_AUTH",
                );
            }
            let reply = d1_query(conn, &body);
            if let Some(i) = st
                .after_query
                .iter()
                .position(|(t, _)| req.body.contains(t.as_str()))
            {
                let (_, sql) = st.after_query.remove(i);
                st.sqlite[*id].execute_batch(&sql).unwrap();
            }
            reply
        }
        _ => err(404, 7003, "Could not route to this path"),
    }
}

fn md5_hex(bytes: &[u8]) -> String {
    use md5::{Digest, Md5};
    use std::fmt::Write as _;
    let mut s = String::new();
    for b in Md5::digest(bytes) {
        let _ = write!(s, "{b:02x}");
    }
    s
}

/// The presigned upload URL D1 import's `init` hands out: like R2, it takes no credentials
/// and answers with the MD5 of the bytes as a quoted `ETag`.
fn upload(st: &mut State, req: &Recorded, path: &str) -> (u16, String, String) {
    if query(&req.query, "X-Amz-Signature") != Some("upl0ad") {
        return (403, "bad signature".into(), String::new());
    }
    if req.header("authorization").is_some() {
        return (400, "credentials not allowed".into(), String::new());
    }
    if let Some(status) = st.upload_status.take() {
        return (status, "unavailable".into(), String::new());
    }
    let name = path.trim_start_matches("/__upload/");
    let Some(entry) = st.uploads.get_mut(name) else {
        return (404, "no such upload".into(), String::new());
    };
    entry.2 = Some(req.body.clone());
    let etag = st
        .upload_etag
        .clone()
        .unwrap_or_else(|| md5_hex(req.body.as_bytes()));
    (200, String::new(), format!("ETag: \"{etag}\"\r\n"))
}

/// Runs an uploaded file in one transaction, as D1 import does ("your DB will return to its
/// original state" on failure, wrangler 4.160.0), and returns the final poll reply.
fn apply_import(st: &mut State, id: &str, sql: &str) -> Value {
    let conn = &st.sqlite[id];
    let applied = match st.import_error.take() {
        Some(e) => Err(e),
        None if st.refuse_sequence_writes && writes_sequence(sql) => {
            Err("not authorized to use sqlite_sequence: SQLITE_AUTH".into())
        }
        None => import_sql(conn, sql),
    };
    match applied {
        Ok(()) => {
            if let Some(lost) = st.after_import.take() {
                conn.execute_batch(&lost).unwrap();
            }
            let b = format!("bm-{}-{}", id, st.bookmarks.len() + 1);
            st.bookmarks.push(b.clone());
            json!({"success": true, "type": "import", "status": "complete", "at_bookmark": b,
                   "messages": ["Imported"],
                   "result": {"num_queries": sql.matches(';').count(), "final_bookmark": b,
                              "meta": {"duration": 1.0, "rows_read": 0, "rows_written": 0}}})
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            json!({"success": true, "type": "import", "status": "error", "at_bookmark": "imp-bm",
                   "error": e, "errors": [e], "messages": ["Import failed"]})
        }
    }
}

fn writes_sequence(sql: &str) -> bool {
    let up = sql.to_ascii_uppercase();
    [
        "INTO SQLITE_SEQUENCE",
        "UPDATE SQLITE_SEQUENCE",
        "DELETE FROM SQLITE_SEQUENCE",
    ]
    .iter()
    .any(|w| up.contains(w))
}

/// What the fake's import does with an uploaded file: all of it in one transaction, or
/// nothing (the caller rolls back on error).
pub fn import_sql(conn: &Connection, sql: &str) -> Result<(), String> {
    conn.execute_batch("BEGIN")
        .and_then(|()| conn.execute_batch(sql))
        .and_then(|()| conn.execute_batch("COMMIT"))
        .map_err(|e| e.to_string())
}

/// D1 import (wrangler 4.160.0 `executeRemotely`): `init` hands out an upload URL, `ingest`
/// runs the uploaded file, `poll` follows it.
fn d1_import(st: &mut State, req: &Recorded, id: &str) -> (u16, Value) {
    if req.header("content-type") != Some("application/json") {
        return err(400, 1001, "expected application/json");
    }
    let body: Value = serde_json::from_str(&req.body).unwrap_or_default();
    let active = json!({"success": true, "type": "import", "status": "active",
                        "at_bookmark": "imp-bm", "messages": ["Processing"]});
    match body["action"].as_str() {
        Some("init") => {
            let Some(etag) = body["etag"]
                .as_str()
                .filter(|e| e.len() == 32 && e.bytes().all(|b| b.is_ascii_hexdigit()))
            else {
                return err(400, 7400, "etag must be the hex MD5 of the file");
            };
            let name = format!("{id}-{}.sql", st.uploads.len() + 1);
            st.uploads
                .insert(name.clone(), (id.to_string(), etag.to_string(), None));
            let url = format!("{}/__upload/{name}?X-Amz-Signature=upl0ad", st.base);
            ok(&json!({"success": true, "upload_url": url, "filename": name}))
        }
        Some("ingest") => {
            let (Some(name), Some(etag)) = (body["filename"].as_str(), body["etag"].as_str())
            else {
                return err(400, 7400, "filename and etag are required");
            };
            let Some((db, want, Some(sql))) = st.uploads.get(name).cloned() else {
                return err(400, 7400, "file not uploaded");
            };
            if db != id || want != etag || md5_hex(sql.as_bytes()) != etag {
                return err(400, 7400, "etag does not match the upload");
            }
            let reply = apply_import(st, id, &sql);
            if st.import_polls == 0 {
                return ok(&reply);
            }
            st.importing = Some((id.to_string(), 0, reply));
            ok(&active)
        }
        Some("poll") => {
            let Some((db, n, reply)) = st.importing.take() else {
                return err(400, 7400, "no import running");
            };
            if db != id || body["current_bookmark"] != "imp-bm" {
                return err(400, 7400, "wrong bookmark");
            }
            if n + 1 < st.import_polls {
                st.importing = Some((db, n + 1, reply));
                return ok(&active);
            }
            ok(&reply)
        }
        _ => err(400, 7400, "unknown action"),
    }
}

/// Time Travel restore; the bookmark must be one the fake handed out.
fn d1_restore(st: &mut State, req: &Recorded, id: &str) -> (u16, Value) {
    let Some(b) = query(&req.query, "bookmark") else {
        return err(400, 7400, "bookmark query parameter is required");
    };
    if !st.bookmarks.iter().any(|x| x == b) {
        return err(400, 7400, "unknown bookmark");
    }
    st.restores.push((id.to_string(), b.to_string()));
    let prev = format!("bm-{}-before-restore-{}", id, st.restores.len());
    ok(&json!({"bookmark": b, "previous_bookmark": prev,
               "message": "Restored"}))
}

/// `GET /accounts/<a>/r2/buckets/<bucket>/objects/<key>`: the object's bytes with an `ETag`,
/// or a JSON 404 (R2 code 10007). Keys keep their slashes and arrive percent-encoded.
fn r2_object(st: &State, req: &Recorded) -> Option<(u16, String, String)> {
    let segs: Vec<&str> = req.path.split('/').skip(1).collect();
    let ["accounts", _, "r2", "buckets", bucket, "objects", key @ ..] = segs.as_slice() else {
        return None;
    };
    if req.method != "GET" {
        return None;
    }
    let token = req
        .header("authorization")
        .and_then(|a| a.strip_prefix("Bearer "))
        .and_then(|t| st.tokens.get(t));
    let json = |(s, v): (u16, Value)| Some((s, v.to_string(), String::new()));
    let Some(token) = token else {
        return json(err(401, 10000, "Authentication error"));
    };
    if !token
        .permissions
        .contains(&"Account | Workers R2 Storage | Edit")
    {
        return json(err(403, 10000, "Authentication error"));
    }
    let key = key
        .iter()
        .map(|s| unescape(s))
        .collect::<Vec<_>>()
        .join("/");
    match st.objects.get(&format!("{bucket}/{key}")) {
        Some(o) => {
            let etag = o
                .etag
                .as_ref()
                .map(|e| format!("ETag: \"{e}\"\r\n"))
                .unwrap_or_default();
            Some((200, o.body.clone(), etag))
        }
        None => json(err(404, 10007, "The specified object does not exist")),
    }
}

/// `%XX` escapes of a path segment (UTF-8).
fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && let Some(v) = s
                .get(i + 1..i + 3)
                .and_then(|h| u8::from_str_radix(h, 16).ok())
        {
            out.push(v);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// R2 buckets, listed by cursor as R2 does.
fn route_r2(
    st: &mut State,
    req: &Recorded,
    segs: &[&str],
    need: &dyn Fn(&str) -> bool,
) -> (u16, Value) {
    match (req.method.as_str(), segs) {
        (_, ["accounts", _, "r2", ..]) if !need("Account | Workers R2 Storage | Edit") => {
            err(403, 10000, "Authentication error")
        }
        ("GET", ["accounts", _, "r2", "buckets"]) => {
            let per: usize = query(&req.query, "per_page")
                .and_then(|v| v.parse().ok())
                .unwrap_or(20);
            let from: usize = query(&req.query, "cursor")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let page: Vec<Value> = st
                .r2
                .iter()
                .skip(from)
                .take(per)
                .map(|n| json!({"name": n}))
                .collect();
            let next = from + page.len();
            let cursor = if next < st.r2.len() {
                next.to_string()
            } else {
                String::new()
            };
            (
                200,
                json!({"success": true, "errors": [], "messages": [], "result": {"buckets": page},
                   "result_info": {"cursor": cursor, "per_page": per}}),
            )
        }
        ("POST", ["accounts", _, "r2", "buckets"]) => {
            let body: Value = serde_json::from_str(&req.body).unwrap_or_default();
            let Some(name) = body.get("name").and_then(Value::as_str) else {
                return err(400, 10005, "name is required");
            };
            if st.r2.iter().any(|b| b == name) {
                return err(409, 10004, "The bucket you tried to create already exists");
            }
            st.r2.push(name.to_string());
            ok(&json!({"name": name}))
        }
        _ => err(404, 7003, "Could not route to this path"),
    }
}

/// Script paths [`route_versions`] answers.
const SCRIPT_OPS: &[&str] = &[
    "assets-upload-session",
    "versions",
    "deployments",
    "schedules",
];

/// Worker scripts, secrets, workers.dev and custom domains.
fn route_workers(
    st: &mut State,
    req: &Recorded,
    segs: &[&str],
    need: &dyn Fn(&str) -> bool,
) -> (u16, Value) {
    match (req.method.as_str(), segs) {
        (_, ["accounts", _, "workers", ..]) if !need("Account | Workers Scripts | Edit") => {
            err(403, 10000, "Authentication error")
        }
        ("GET", ["accounts", _, "workers", "subdomain"]) => match &st.subdomain {
            Some(s) => ok(&json!({"subdomain": s})),
            None => err(
                404,
                10007,
                "This account does not have a workers.dev subdomain",
            ),
        },
        ("PUT", ["accounts", _, "workers", "scripts", name]) => {
            let ctype = req.header("content-type").unwrap_or("");
            let Some(boundary) = ctype.strip_prefix("multipart/form-data; boundary=") else {
                return err(400, 10021, "expected multipart/form-data");
            };
            let b = &req.body;
            let shaped = b.contains(&format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"metadata\""
            )) && b.contains("\"main_module\":\"worker.js\"")
                && b.contains("name=\"worker.js\"; filename=\"worker.js\"")
                && b.contains("Content-Type: application/javascript+module")
                && b.contains("status: 503")
                && b.ends_with(&format!("--{boundary}--\r\n"));
            if !shaped {
                return err(400, 10021, "bad multipart upload");
            }
            placeholder(st, name, req);
            ok(&json!({"id": name}))
        }
        (_, ["accounts", _, "workers", "scripts", name, ..]) if !st.scripts.contains_key(*name) => {
            err(404, 10007, "workers.api.error.script_not_found")
        }
        ("GET", ["accounts", _, "workers", "scripts", name, "secrets"]) => {
            let mut names: Vec<&String> = st.scripts[*name].secrets.keys().collect();
            names.sort();
            ok(&json!(
                names
                    .iter()
                    .map(|n| json!({"name": n, "type": "secret_text"}))
                    .collect::<Vec<_>>()
            ))
        }
        ("DELETE", ["accounts", _, "workers", "scripts", _, "secrets", _])
        | ("PUT", ["accounts", _, "workers", "scripts", _, "secrets"])
        | (
            "GET",
            ["accounts", _, "workers", "scripts", _, "versions", _]
            | ["accounts", _, "workers", "scripts", _, "settings"],
        ) => script_meta(st, req, segs),
        (_, ["accounts", _, "workers", "scripts", _, op]) if SCRIPT_OPS.contains(op) => {
            route_versions(st, req, segs)
        }
        ("GET", ["accounts", _, "workers", "scripts", name, "subdomain"]) => {
            let on = st.scripts[*name].workers_dev;
            ok(&json!({"enabled": on, "previews_enabled": false}))
        }
        ("POST", ["accounts", _, "workers", "scripts", name, "subdomain"]) => {
            let body: Value = serde_json::from_str(&req.body).unwrap_or_default();
            let Some(on) = body["enabled"].as_bool() else {
                return err(400, 10021, "enabled is required");
            };
            st.scripts.get_mut(*name).unwrap().workers_dev = on;
            ok(&json!({"enabled": on}))
        }
        // Paged, and any hostname filter ignored: the client must filter itself.
        ("GET", ["accounts", _, "workers", "domains"]) => paged(&st.domains, &req.query),
        ("PUT", ["accounts", _, "workers", "domains"]) => {
            let body: Value = serde_json::from_str(&req.body).unwrap_or_default();
            let (Some(h), Some(svc), Some(zone)) = (
                body["hostname"].as_str(),
                body["service"].as_str(),
                body["zone_id"].as_str(),
            ) else {
                return err(400, 100_101, "hostname, service and zone_id are required");
            };
            if !st.zones.iter().any(|z| z["id"] == zone) || !st.scripts.contains_key(svc) {
                return err(400, 100_102, "unknown zone or service");
            }
            st.domains.retain(|d| d["hostname"] != h);
            let d = json!({"id": format!("dom-{}", st.domains.len() + 1), "hostname": h,
                       "service": svc, "zone_id": zone, "environment": body["environment"]});
            st.domains.push(d.clone());
            ok(&d)
        }
        _ => err(404, 7003, "Could not route to this path"),
    }
}

/// Secret delete and the deployed version's bindings (script settings).
fn script_meta(st: &mut State, req: &Recorded, segs: &[&str]) -> (u16, Value) {
    match (req.method.as_str(), segs) {
        ("DELETE", ["accounts", _, "workers", "scripts", name, "secrets", secret]) => {
            let script = st.scripts.get_mut(*name).unwrap();
            if script.secrets.remove(*secret).is_none() {
                return err(404, 10056, "secret not found");
            }
            secret_version(st, name);
            ok(&json!(null))
        }
        ("PUT", ["accounts", _, "workers", "scripts", name, "secrets"]) => {
            let body: Value = serde_json::from_str(&req.body).unwrap_or_default();
            let (Some(n), Some(t), Some("secret_text")) = (
                body["name"].as_str(),
                body["text"].as_str(),
                body["type"].as_str(),
            ) else {
                return err(400, 10021, "secret needs name, text and type secret_text");
            };
            let script = st.scripts.get_mut(*name).unwrap();
            script.secrets.insert(n.to_string(), t.to_string());
            secret_version(st, name);
            ok(&json!({"name": n, "type": "secret_text"}))
        }
        // `result.resources.bindings`, as wrangler's `versions view` reads it.
        ("GET", ["accounts", _, "workers", "scripts", name, "versions", id]) => {
            if !st.scripts[*name].versions.iter().any(|v| v == id) {
                return err(404, 10007, "version not found");
            }
            let meta = st
                .versions
                .iter()
                .find(|v| v.id == *id)
                .map(|v| v.metadata.clone())
                .unwrap_or_default();
            let bindings = Some(meta["bindings"].clone())
                .filter(Value::is_array)
                .unwrap_or_else(|| json!([]));
            ok(&json!({"id": id, "resources": {"bindings": bindings}}))
        }
        // The deployed version's bindings, as the API reports them.
        ("GET", ["accounts", _, "workers", "scripts", name, "settings"]) => {
            let active = st.scripts[*name].deployments.first().cloned();
            let bindings = st
                .versions
                .iter()
                .find(|v| Some(&v.id) == active.as_ref())
                .map(|v| v.metadata["bindings"].clone())
                .filter(Value::is_array)
                .unwrap_or_else(|| json!([]));
            ok(&json!({"bindings": bindings, "logpush": false}))
        }
        _ => err(404, 7003, "Could not route to this path"),
    }
}

/// The next upload's id: `...8000-<n>`, counting uploads only.
fn upload_id(st: &State) -> String {
    let n = st
        .versions
        .iter()
        .filter(|v| v.id.contains("-8000-"))
        .count()
        + 1;
    format!("00000000-0000-4000-8000-{n:012}")
}

/// A secret change makes a new version from the active one (same bindings) and deploys it,
/// as Cloudflare does. Its own id series, so upload ids stay `...8000-<n>`.
fn secret_version(st: &mut State, name: &str) {
    let n = st
        .versions
        .iter()
        .filter(|v| v.id.contains("-9000-"))
        .count()
        + 1;
    let id = format!("00000000-0000-4000-9000-{n:012}");
    let active = st.scripts[name].deployments.first().cloned();
    let (metadata, modules) = st
        .versions
        .iter()
        .find(|v| Some(&v.id) == active.as_ref())
        .map(|v| (v.metadata.clone(), v.modules.clone()))
        .unwrap_or_default();
    st.versions.push(Version {
        id: id.clone(),
        metadata,
        modules,
    });
    let script = st.scripts.get_mut(name).unwrap();
    script.versions.push(id.clone());
    script.deployments.insert(0, id);
}

/// A script upload makes a version and deploys it, as Cloudflare does.
fn placeholder(st: &mut State, name: &str, req: &Recorded) {
    let metadata = multipart(req)
        .and_then(|p| p.into_iter().find(|(h, _)| h.contains("name=\"metadata\"")))
        .and_then(|(_, m)| serde_json::from_str(&m).ok())
        .unwrap_or_default();
    let id = upload_id(st);
    st.versions.push(Version {
        id: id.clone(),
        metadata,
        modules: vec!["worker.js".into()],
    });
    let script = st.scripts.entry(name.to_string()).or_default();
    script.uploads += 1;
    script.versions.push(id.clone());
    script.deployments.insert(0, id);
}

/// `POST .../versions`: multipart with `metadata` and module parts.
fn new_version(st: &mut State, req: &Recorded, name: &str) -> (u16, Value) {
    let Some(parts) = multipart(req) else {
        return err(400, 10021, "expected multipart/form-data");
    };
    let Some((_, meta)) = parts.iter().find(|(h, _)| h.contains("name=\"metadata\"")) else {
        return err(400, 10021, "no metadata part");
    };
    let meta: Value = serde_json::from_str(meta).unwrap_or_default();
    let modules: Vec<String> = parts
        .iter()
        .filter_map(|(h, _)| {
            let n = h.split("filename=\"").nth(1)?.split('"').next()?;
            Some(n.to_string())
        })
        .collect();
    let main = meta["main_module"].as_str().unwrap_or("");
    if !modules.iter().any(|m| m == main) {
        return err(400, 10021, "main_module has no part");
    }
    // Strict inheritance: every `inherit` binding must exist on the script.
    if query(&req.query, "bindings_inherit") != Some("strict") {
        return err(400, 10021, "expected bindings_inherit=strict");
    }
    for b in meta["bindings"].as_array().into_iter().flatten() {
        let n = b["name"].as_str().unwrap_or("");
        if b["type"] == "inherit" && !st.scripts[name].secrets.contains_key(n) {
            return err(400, 10021, &format!("binding {n} could not be inherited"));
        }
    }
    let jwt = meta["assets"]["jwt"].as_str().unwrap_or("");
    if !st.completions.iter().any(|c| c == jwt) {
        return err(400, 10021, "assets jwt is not a completion token");
    }
    let id = upload_id(st);
    st.versions.push(Version {
        id: id.clone(),
        metadata: meta.clone(),
        modules,
    });
    let script = st.scripts.get_mut(name).unwrap();
    script.versions.push(id.clone());
    // Without keep_bindings for secrets, a new version has none (as Cloudflare drops
    // bindings the metadata does not list).
    let keep = meta["keep_bindings"]
        .as_array()
        .is_some_and(|k| k.iter().any(|x| x == "secret_text"));
    if !keep {
        script.secrets.clear();
    }
    ok(&json!({"id": id, "number": script.versions.len()}))
}

/// Assets upload sessions, versions, deployments and cron schedules of a script.
fn route_versions(st: &mut State, req: &Recorded, segs: &[&str]) -> (u16, Value) {
    match (req.method.as_str(), segs) {
        (
            "POST",
            [
                "accounts",
                _,
                "workers",
                "scripts",
                _,
                "assets-upload-session",
            ],
        ) => {
            let body: Value = serde_json::from_str(&req.body).unwrap_or_default();
            let Some(manifest) = body["manifest"].as_object() else {
                return err(400, 10021, "manifest is required");
            };
            let mut missing = Vec::new();
            for (path, entry) in manifest {
                let hash = entry["hash"].as_str().unwrap_or("");
                let hex32 = hash.len() == 32 && hash.bytes().all(|b| b.is_ascii_hexdigit());
                if !path.starts_with('/') || !hex32 || entry["size"].as_u64().is_none() {
                    return err(400, 10021, "bad manifest entry");
                }
                st.asset_paths.insert(hash.to_string(), path.clone());
                if !st.assets.contains(hash) && !missing.contains(&hash.to_string()) {
                    missing.push(hash.to_string());
                }
            }
            let jwt = format!("sessionJWT{}", st.sessions.len() + 1);
            // Two files per bucket, so tests see several uploads.
            let buckets: Vec<Vec<String>> = missing.chunks(2).map(<[String]>::to_vec).collect();
            st.sessions.insert(jwt.clone(), missing.clone());
            if missing.is_empty() {
                st.completions.push(jwt.clone());
            }
            ok(&json!({"jwt": jwt, "buckets": buckets}))
        }
        ("POST", ["accounts", _, "workers", "scripts", name, "versions"]) => {
            new_version(st, req, name)
        }
        ("GET", ["accounts", _, "workers", "scripts", name, "versions"]) => {
            let items: Vec<Value> = st.scripts[*name]
                .versions
                .iter()
                .rev()
                .map(|v| json!({"id": v}))
                .collect();
            ok(&json!({"items": items}))
        }
        ("GET", ["accounts", _, "workers", "scripts", name, "deployments"]) => {
            if let Some(v) = &st.deployments_override {
                return ok(v);
            }
            let list: Vec<Value> = st.scripts[*name]
                .deployments
                .iter()
                .map(|v| {
                    json!({"id": format!("dep-{v}"), "strategy": "percentage",
                                "versions": [{"version_id": v, "percentage": 100.0}]})
                })
                .collect();
            ok(&json!({"deployments": list}))
        }
        ("POST", ["accounts", _, "workers", "scripts", name, "deployments"]) => {
            let body: Value = serde_json::from_str(&req.body).unwrap_or_default();
            let v = body["versions"][0]["version_id"].as_str().unwrap_or("");
            let script = st.scripts.get_mut(*name).unwrap();
            if body["strategy"] != "percentage"
                || body["versions"][0]["percentage"] != 100
                || !script.versions.iter().any(|x| x == v)
            {
                return err(400, 10021, "bad deployment");
            }
            script.deployments.insert(0, v.to_string());
            ok(&json!({"id": format!("dep-{v}")}))
        }
        ("PUT", ["accounts", _, "workers", "scripts", name, "schedules"]) => {
            let body: Value = serde_json::from_str(&req.body).unwrap_or_default();
            if !body.is_array() {
                return err(400, 10021, "schedules must be an array");
            }
            st.scripts.get_mut(*name).unwrap().schedules = Some(body.clone());
            ok(&json!({"schedules": body}))
        }
        _ => err(404, 7003, "Could not route to this path"),
    }
}

/// Zones, filtered by name and account.
fn route_zones(
    st: &mut State,
    req: &Recorded,
    segs: &[&str],
    need: &dyn Fn(&str) -> bool,
) -> (u16, Value) {
    match (req.method.as_str(), segs) {
        (_, ["zones", ..]) if !need("Zone | Zone | Read") => {
            err(403, 9109, "Unauthorized to access requested resource")
        }
        ("GET", ["zones"]) => {
            let name = query(&req.query, "name");
            let acct = query(&req.query, "account.id");
            let list: Vec<Value> = st
                .zones
                .iter()
                .filter(|z| name.is_some_and(|n| z["name"] == n))
                .filter(|z| acct.is_some_and(|a| z["account"]["id"] == a))
                .cloned()
                .collect();
            ok(&json!(list))
        }
        _ => err(404, 7003, "Could not route to this path"),
    }
}

/// Turnstile widgets; the list has no secrets, GET one has.
fn route_turnstile(
    st: &mut State,
    req: &Recorded,
    segs: &[&str],
    need: &dyn Fn(&str) -> bool,
) -> (u16, Value) {
    match (req.method.as_str(), segs) {
        (_, ["accounts", _, "challenges", ..]) if !need("Account | Turnstile | Edit") => {
            err(403, 10000, "Authentication error")
        }
        ("GET", ["accounts", _, "challenges", "widgets"]) => {
            let list: Vec<Value> = st
                .widgets
                .iter()
                .map(|w| {
                    json!({"sitekey": w.sitekey, "name": w.name, "domains": w.domains,
                            "mode": w.mode})
                })
                .collect();
            paged(&list, &req.query)
        }
        ("POST", ["accounts", _, "challenges", "widgets"]) => {
            let body: Value = serde_json::from_str(&req.body).unwrap_or_default();
            let domains: Vec<String> = body["domains"]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|d| d.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            let (Some(name), Some(mode)) = (body["name"].as_str(), body["mode"].as_str()) else {
                return err(400, 10001, "name and mode are required");
            };
            if domains.is_empty() || domains.len() > 10 {
                return err(400, 10001, "domains: 1 to 10");
            }
            let n = st.widgets.len() + 1;
            let w = Widget {
                sitekey: format!("0x4AAAAAAAsitekey{n}"),
                secret: format!("0x4AAAAAAAsecret{n}SECRETvalue"),
                name: name.to_string(),
                domains,
                mode: mode.to_string(),
            };
            let reply = json!({"sitekey": w.sitekey, "secret": w.secret, "name": w.name,
                           "domains": w.domains, "mode": w.mode});
            st.widgets.push(w);
            ok(&reply)
        }
        ("GET", ["accounts", _, "challenges", "widgets", key]) => {
            match st.widgets.iter().find(|w| w.sitekey == *key) {
                Some(w) => ok(&json!({"sitekey": w.sitekey, "secret": w.secret,
                                  "name": w.name, "domains": w.domains, "mode": w.mode})),
                None => err(404, 10404, "widget not found"),
            }
        }
        _ => err(404, 7003, "Could not route to this path"),
    }
}

/// `(part headers, part content)` of a multipart request body.
fn multipart(req: &Recorded) -> Option<Vec<(String, String)>> {
    let ctype = req.header("content-type")?;
    let boundary = ctype.strip_prefix("multipart/form-data; boundary=")?;
    let body = req.body.strip_suffix(&format!("--{boundary}--\r\n"))?;
    let mut out = Vec::new();
    for chunk in body.split(&format!("--{boundary}\r\n")).skip(1) {
        let chunk = chunk.strip_suffix("\r\n")?;
        let (head, content) = chunk.split_once("\r\n\r\n")?;
        out.push((head.to_string(), content.to_string()));
    }
    Some(out)
}

fn b64_decode(s: &str) -> Option<Vec<u8>> {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0;
    for c in s.bytes().filter(|c| *c != b'=') {
        let v = u32::try_from(A.iter().position(|a| *a == c)?).ok()?;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((acc >> bits) & 0xff).ok()?);
        }
    }
    Some(out)
}

/// Assets upload: authorized by the session JWT only; each part is base64 of a file whose
/// hash the session asked for. The last bucket gets the completion JWT.
fn assets_upload(st: &mut State, req: &Recorded) -> (u16, Value) {
    let jwt = req
        .header("authorization")
        .and_then(|a| a.strip_prefix("Bearer "))
        .unwrap_or("");
    if !st.sessions.contains_key(jwt) {
        return err(401, 10000, "upload needs the session JWT");
    }
    if query(&req.query, "base64") != Some("true") {
        return err(400, 10021, "base64=true is required");
    }
    let Some(parts) = multipart(req) else {
        return err(400, 10021, "expected multipart/form-data");
    };
    for (head, content) in &parts {
        let Some(hash) = head
            .split("name=\"")
            .nth(1)
            .and_then(|r| r.split('"').next())
        else {
            return err(400, 10021, "part without a name");
        };
        if b64_decode(content).is_none() {
            return err(400, 10021, "part is not base64");
        }
        // Content as sent (base64) plus the manifest path's extension.
        let ext = st
            .asset_paths
            .get(hash)
            .and_then(|p| p.rsplit_once('.'))
            .map_or("", |(_, e)| e);
        let mut h = plumblog_auth::base64url::hex(&plumblog_auth::sha256::sha256(
            format!("{content}{ext}").as_bytes(),
        ));
        h.truncate(32);
        if h != hash || !head.contains(&format!("filename=\"{hash}\"")) {
            return err(400, 10021, "hash does not match the content");
        }
        st.assets.insert(hash.to_string());
        st.sessions.get_mut(jwt).unwrap().retain(|m| m != hash);
    }
    if st.sessions[jwt].is_empty() {
        let done = format!("completionJWT{}", st.completions.len() + 1);
        st.completions.push(done.clone());
        let (_, mut v) = ok(&json!({"jwt": done}));
        v["success"] = json!(true);
        return (201, v);
    }
    ok(&json!({}))
}

fn to_json(v: ValueRef) -> Value {
    match v {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(i) => json!(i),
        ValueRef::Real(f) => json!(f),
        ValueRef::Text(t) => json!(String::from_utf8_lossy(t)),
        ValueRef::Blob(b) => json!(b),
    }
}

/// One statement: rows if it returns columns, else the change count.
fn run_rows(
    conn: &Connection,
    sql: &str,
    params: &[String],
) -> rusqlite::Result<Vec<Map<String, Value>>> {
    let mut stmt = conn.prepare(sql)?;
    let names: Vec<String> = stmt
        .column_names()
        .iter()
        .map(ToString::to_string)
        .collect();
    let mut rows = stmt.query(rusqlite::params_from_iter(params.iter()))?;
    let mut out = Vec::new();
    while let Some(row) = rows.next()? {
        let mut m = Map::new();
        for (i, n) in names.iter().enumerate() {
            m.insert(n.clone(), to_json(row.get_ref(i)?));
        }
        out.push(m);
    }
    Ok(out)
}

fn one(conn: &Connection, sql: &str, params: &[String]) -> rusqlite::Result<Value> {
    let before = conn.total_changes();
    let rows = match run_rows(conn, sql, params) {
        // rusqlite prepares only one statement (and may fail checking the rest, which can
        // name tables made earlier in the same text): `{sql}` without params runs all of
        // them in order, like D1, and not atomically.
        Err(_) if params.is_empty() => {
            conn.execute_batch(sql)?;
            Vec::new()
        }
        other => other?,
    };
    let changes = conn.total_changes() - before;
    Ok(json!({"results": rows, "success": true, "meta": {"changes": changes}}))
}

/// `{sql, params?}` or `{batch: [{sql, params?}]}`. D1 params are strings.
fn d1_query(conn: &Connection, body: &Value) -> (u16, Value) {
    let stmts: Vec<&Value> = match body.get("batch").and_then(Value::as_array) {
        Some(b) => b.iter().collect(),
        None => vec![body],
    };
    let mut results = Vec::new();
    for s in stmts {
        let Some(sql) = s.get("sql").and_then(Value::as_str) else {
            return err(400, 7400, "sql is required");
        };
        let params: Option<Vec<String>> = match s.get("params") {
            None => Some(Vec::new()),
            Some(Value::Array(a)) => a.iter().map(|p| p.as_str().map(str::to_string)).collect(),
            Some(_) => None,
        };
        let Some(params) = params else {
            return err(400, 7400, "params must be strings");
        };
        match one(conn, sql, &params) {
            Ok(r) => results.push(r),
            Err(e) => return err(400, 7500, &format!("{e}: SQLITE_ERROR")),
        }
    }
    ok(&json!(results))
}

fn query<'a>(q: &'a str, name: &str) -> Option<&'a str> {
    q.split('&')
        .find_map(|p| p.strip_prefix(name)?.strip_prefix('='))
}

/// `page` / `per_page` paging with `result_info`, as most list endpoints do.
fn paged(list: &[Value], q: &str) -> (u16, Value) {
    let per: usize = query(q, "per_page")
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let page: usize = query(q, "page")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
        .max(1);
    let items: Vec<Value> = list
        .iter()
        .skip((page - 1) * per)
        .take(per)
        .cloned()
        .collect();
    let total_pages = list.len().div_ceil(per).max(1);
    (
        200,
        json!({"success": true, "errors": [], "messages": [], "result": items,
               "result_info": {"page": page, "per_page": per, "count": items.len(),
                               "total_count": list.len(), "total_pages": total_pages}}),
    )
}
