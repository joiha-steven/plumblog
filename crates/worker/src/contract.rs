//! Test builds only (feature `contract-tests`): runs the store contract suite against the
//! real D1 binding, so `D1Store` is held to the same cases as the in-memory store.
//! `GET /__contract` lists case names; `POST /__contract/<name>` empties the database and
//! runs one case. Cases share one database, so the caller runs them one at a time.
//! D1-only cases (`d1::selftest`) are listed and run the same way. `POST /__seed` empties the
//! database and saves the sample posts (search tests, local previews).

use plumblog_store::contract;
use worker::{Env, Request, Response, Result};

use crate::d1::{D1Store, selftest};

const PREFIX: &str = "/__contract";
const SEED: &str = "/__seed";

/// Second lock besides the cargo feature: the route empties the database, so it also needs
/// this variable, which only tools/wrangler.dev.toml sets.
const ENABLE_VAR: &str = "PLUMBLOG_CONTRACT_TESTS";

pub async fn handle(req: &Request, env: &Env) -> Result<Option<Response>> {
    if env.var(ENABLE_VAR).map(|v| v.to_string()).ok().as_deref() != Some("wipe-db") {
        return Ok(None);
    }
    let path = req.path();
    let method = req.method();
    let method = method.as_ref();
    // Empties the database and saves the sample posts, so search tests and local previews
    // read the same posts the sample pages show.
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
