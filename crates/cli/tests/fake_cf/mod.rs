//! A fake Cloudflare API for tests: a `TcpListener` on 127.0.0.1 in a thread, minimal
//! HTTP/1.1 (one request per connection), in-memory state, every request recorded.
//!
//! Extend it by adding fields to [`State`] and arms to [`route`]; each arm checks the
//! request shape and returns the Cloudflare envelope.

use serde_json::{Value, json};
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
    let failure = st.failures.iter().find(|(i, _)| *i == n).map(|(_, f)| *f);
    let (status, body) = match failure {
        Some(Failure::Drop) => return,
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
        (_, ["accounts", _, "d1", ..]) if !need("Account | D1 | Edit") => {
            err(403, 10000, "Authentication error")
        }
        ("GET", ["accounts", _, "d1", "database"]) => ok(&json!(st.d1)),
        ("POST", ["accounts", _, "d1", "database"]) => {
            if req.header("content-type") != Some("application/json") {
                return err(400, 1001, "expected application/json");
            }
            let body: Value = serde_json::from_str(&req.body).unwrap_or_default();
            let Some(name) = body.get("name").and_then(Value::as_str) else {
                return err(400, 1001, "name is required");
            };
            let db = json!({"uuid": format!("db-{}", st.d1.len() + 1), "name": name});
            st.d1.push(db.clone());
            ok(&db)
        }
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
        _ => err(404, 7003, "Could not route to this path"),
    }
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
