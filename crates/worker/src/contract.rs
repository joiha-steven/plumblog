//! Test builds only (feature `contract-tests`): runs the store contract suite against the
//! real D1 binding, so `D1Store` is held to the same cases as the in-memory store.
//! `GET /__contract` lists case names; `POST /__contract/<name>` empties the database and
//! runs one case. Cases share one database, so the caller runs them one at a time.
//! D1-only cases (`d1::selftest`) are listed and run the same way. `POST /__seed` empties the
//! database and saves the sample posts (search tests, local previews); `POST /__option`
//! stores one option value (options tests). `POST /__moderate` runs the real moderation
//! function (comments tests).

use plumblog_model::{CommentId, RequestId, Role, UserId};
use plumblog_store::comment::moderate;
use plumblog_store::{Actor, Moderation, OptionOwner, SettingsStore, contract};
use plumblog_syslog::Scope;
use worker::{Env, Request, Response, Result};

use crate::d1::{D1Store, selftest};

const PREFIX: &str = "/__contract";
const SEED: &str = "/__seed";
const OPTION: &str = "/__option";
const MODERATE: &str = "/__moderate";

/// Second lock besides the cargo feature: the route empties the database, so it also needs
/// this variable, which only tools/wrangler.dev.toml sets.
const ENABLE_VAR: &str = "PLUMBLOG_CONTRACT_TESTS";

/// Whether this Worker may serve test routes (`/__contract`, `/__webhooks/...`).
pub fn enabled(env: &Env) -> bool {
    env.var(ENABLE_VAR).map(|v| v.to_string()).ok().as_deref() == Some("wipe-db")
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
