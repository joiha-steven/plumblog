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
}

pub struct Fake {
    pub base: String,
    state: Arc<Mutex<State>>,
}

impl Fake {
    pub fn start(state: State) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(state));
        let shared = Arc::clone(&state);
        // Never joined: the thread ends with the test process.
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                serve(&stream, &shared);
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

fn write_reply(mut stream: &TcpStream, status: u16, body: &str) {
    let reply = format!(
        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(reply.as_bytes());
}

fn serve(stream: &TcpStream, state: &Mutex<State>) {
    let Some(req) = read_request(stream) else {
        return;
    };
    let mut st = state.lock().unwrap();
    st.requests.push(req.clone());
    let n = st.requests.len();
    let mut failure = st.failures.iter().find(|(i, _)| *i == n).map(|(_, f)| *f);
    let line = format!("{} {}", req.method, req.path);
    if failure.is_none()
        && let Some(i) = st
            .fail_on
            .iter()
            .position(|(p, _)| line.contains(p.as_str()))
    {
        failure = Some(st.fail_on.remove(i).1);
    }
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
            let (s, v) = route(&mut st, &req);
            (s, v.to_string())
        }
    };
    drop(st);
    write_reply(stream, status, &body);
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
            st.sqlite.insert(id, Connection::open_in_memory().unwrap());
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
        ("POST", ["accounts", _, "d1", "database", id, "query"]) => {
            let body: Value = serde_json::from_str(&req.body).unwrap_or_default();
            let Some(conn) = st.sqlite.get(*id) else {
                return err(404, 7404, "database not found");
            };
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
            let script = st.scripts.entry((*name).to_string()).or_default();
            script.uploads += 1;
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
            ok(&json!({"name": n, "type": "secret_text"}))
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
