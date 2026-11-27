//! Test builds only (feature `contract-tests`): runs the store contract suite against the
//! real D1 binding, so `D1Store` is held to the same cases as the in-memory store.
//! `GET /__contract` lists case names; `POST /__contract/<name>` empties the database and
//! runs one case. Cases share one database, so the caller runs them one at a time.
//! D1-only cases (`d1::selftest`) are listed and run the same way. `POST /__seed` empties the
//! database and saves the sample posts (search tests, local previews); `POST /__option`
//! stores one option value (options tests). `POST /__moderate` runs the real moderation
//! function (comments tests). [`TestGate`] stands in for Turnstile and the rate limit.

use plumblog_model::{CommentId, Error, ErrorCode, RequestId, Result as CoreResult, Role, UserId};
use plumblog_store::comment::moderate;
use plumblog_store::{Actor, Moderation, OptionOwner, SettingsStore, contract};
use plumblog_syslog::Scope;
use std::cell::RefCell;
use std::collections::HashMap;
use worker::{Env, Request, Response, Result};

use crate::comments::CommentGate;

use crate::d1::{D1Store, selftest};

const PREFIX: &str = "/__contract";
const SEED: &str = "/__seed";
const OPTION: &str = "/__option";
const MODERATE: &str = "/__moderate";

/// Second lock besides the cargo feature: the route empties the database, so it also needs
/// this variable, which only tools/wrangler.dev.toml sets.
const ENABLE_VAR: &str = "PLUMBLOG_CONTRACT_TESTS";

fn enabled(env: &Env) -> bool {
    env.var(ENABLE_VAR).map(|v| v.to_string()).ok().as_deref() == Some("wipe-db")
}

/// The only Turnstile token [`TestGate`] accepts.
pub const TEST_TOKEN: &str = "plumblog-test-token";
/// Comments one test client may send per isolate before [`TestGate`] refuses.
pub const TEST_LIMIT: u32 = 3;
/// Names the limiter bucket, so tests do not share one; `unconfigured` asks for the
/// production gate instead.
const TEST_CLIENT: &str = "x-plumblog-test-client";

thread_local! {
    static SENT: RefCell<HashMap<String, u32>> = RefCell::new(HashMap::new());
}

/// Comment gate for test builds with the test variable set (the same double lock as
/// `/__contract`): a fixed token passes verification and each client may send
/// [`TEST_LIMIT`] comments, counted in memory.
pub struct TestGate {
    client: String,
}

impl TestGate {
    pub fn for_request(env: &Env, req: &Request) -> Option<Self> {
        if !enabled(env) {
            return None;
        }
        let client = req.headers().get(TEST_CLIENT).ok().flatten();
        if client.as_deref() == Some("unconfigured") {
            return None;
        }
        Some(Self {
            client: client.unwrap_or_default(),
        })
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "stands in for the async Turnstile and limiter gate"
)]
impl CommentGate for TestGate {
    async fn verify(&self, token: &str, _remote_ip: Option<&str>) -> CoreResult<()> {
        if token == TEST_TOKEN {
            Ok(())
        } else {
            Err(Error::new(
                ErrorCode::CommentCheckFailed,
                "test token mismatch",
            ))
        }
    }

    async fn allow(&self, remote_ip: Option<&str>) -> CoreResult<()> {
        let key = format!("{}|{}", self.client, remote_ip.unwrap_or_default());
        let n = SENT.with(|m| {
            let mut m = m.borrow_mut();
            let n = m.entry(key).or_insert(0);
            *n += 1;
            *n
        });
        if n <= TEST_LIMIT {
            Ok(())
        } else {
            Err(Error::new(
                ErrorCode::CommentRateLimited,
                format!("attempt {n}"),
            ))
        }
    }
}

pub async fn handle(req: &Request, env: &Env) -> Result<Option<Response>> {
    if !enabled(env) {
        return Ok(None);
    }
    let path = req.path();
    let method = req.method();
    let method = method.as_ref();
    // Empties the database and saves the sample posts, so search tests and local previews
    // read the same posts the sample pages show.
    // Writes one theme or plugin option the way the admin will (bumps the content version):
    // `?owner=theme|plugin&id=<name>&key=<key>`, body = the JSON value.
    if path == OPTION && method == "POST" {
        let url = req.url()?;
        let q = |k: &str| {
            url.query_pairs()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.into_owned())
                .unwrap_or_default()
        };
        let owner = match q("owner").as_str() {
            "theme" => OptionOwner::Theme,
            "plugin" => OptionOwner::Plugin,
            other => return Response::error(format!("owner {other:?}"), 400).map(Some),
        };
        let mut req = req.clone()?;
        let value = req.text().await?;
        let store = D1Store::new(env.d1("DB")?);
        let now = i64::try_from(worker::Date::now().as_millis()).unwrap_or(0);
        return match store
            .put_option(owner, &q("id"), &q("key"), &value, now)
            .await
        {
            Ok(()) => Response::ok("ok").map(Some),
            Err(e) => Response::error(format!("put_option failed: {e}"), 500).map(Some),
        };
    }
    // `?id=<comment>&action=approved|spam|trashed|deleted[&role=owner|writer&user=<id>]`;
    // no role means no signed-in user.
    if path == MODERATE && method == "POST" {
        let url = req.url()?;
        let q = |k: &str| {
            url.query_pairs()
                .find(|(n, _)| n == k)
                .map(|(_, v)| v.into_owned())
                .unwrap_or_default()
        };
        let (Ok(id), Some(action)) = (q("id").parse(), Moderation::parse(&q("action"))) else {
            return Response::error("id and action", 400).map(Some);
        };
        let actor = Role::parse(&q("role")).map(|role| Actor {
            id: UserId(q("user").parse().unwrap_or(1)),
            role,
        });
        let scope = Scope::new(
            RequestId::parse("test-moderate").unwrap_or_else(RequestId::unknown),
            actor.map(|a| a.id),
        );
        let store = D1Store::new(env.d1("DB")?);
        let now = worker::Date::now().as_millis();
        let report = |m: &str| worker::console_error!("{m}");
        return match moderate(&store, &scope, actor, CommentId(id), action, now, report).await {
            Ok(()) => Response::ok("ok").map(Some),
            Err(e) => Response::error(e.code.as_str(), 403).map(Some),
        };
    }
    if path == SEED && method == "POST" {
        let store = D1Store::new(env.d1("DB")?);
        if let Err(e) = store.wipe().await {
            return Response::error(format!("wipe failed: {e}"), 500).map(Some);
        }
        return match crate::pages::seed::sample_posts(&store).await {
            Ok(n) => Response::ok(format!("{n} posts")).map(Some),
            Err(e) => Response::error(format!("seed failed: {e}"), 500).map(Some),
        };
    }
    if path == PREFIX && method == "GET" {
        let all: Vec<&str> = contract::CASES
            .iter()
            .chain(selftest::CASES)
            .copied()
            .collect();
        return Response::ok(all.join("\n")).map(Some);
    }
    let Some(name) = path.strip_prefix(PREFIX).and_then(|p| p.strip_prefix('/')) else {
        return Ok(None);
    };
    if method != "POST" {
        return Ok(None);
    }
    let store = D1Store::new(env.d1("DB")?);
    if let Err(e) = store.wipe().await {
        return Response::error(format!("wipe failed: {e}"), 500).map(Some);
    }
    let out = match selftest::run(name, &store).await {
        Some(out) => out,
        None => contract::run_case(name, &store).await,
    };
    match out {
        Ok(()) => Response::ok("ok").map(Some),
        Err(msg) => Response::error(msg, 500).map(Some),
    }
}
