//! Test builds only (feature `contract-tests`): runs the store contract suite against the
//! real D1 binding, so `D1Store` is held to the same cases as the in-memory store.
//! `GET /__contract` lists case names; `POST /__contract/<name>` empties the database and
//! runs one case. Cases share one database, so the caller runs them one at a time.
//! D1-only cases (`d1::selftest`) are listed and run the same way. `POST /__seed` empties the
//! database and saves the sample posts (search tests, local previews); `POST /__option`
//! stores one option value (options tests). `POST /__preview` renders a draft (form fields
//! `title`, `body`, `lang`) as the page it would publish as, for the admin mock.

use plumblog_store::{OptionOwner, SettingsStore, contract};
use worker::{Env, Request, Response, Result};

use crate::d1::{D1Store, selftest};

const PREFIX: &str = "/__contract";
const SEED: &str = "/__seed";
const OPTION: &str = "/__option";
const PREVIEW: &str = "/__preview";
/// The preview page runs no script even if opened outside the editor's frame, and only the
/// admin may frame it. `allow-same-origin` keeps the site's origin so the theme's fonts pass
/// their CORS check; scripts stay off (`sandbox` without `allow-scripts`, and `script-src`).
/// `/api/preview` (#10) sends the same.
pub const PREVIEW_CSP: &str =
    "sandbox allow-same-origin; script-src 'none'; frame-ancestors 'self'";

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
    if path == PREVIEW && method == "POST" {
        return preview(req, env).await.map(Some);
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

/// `POST /__preview`: the draft in the form (`title`, `body`, `lang`) as the page it would
/// publish as.
async fn preview(req: &Request, env: &Env) -> Result<Response> {
    let mut req = req.clone()?;
    let form = req.form_data().await?;
    let field = |k: &str| match form.get(k) {
        Some(worker::FormEntry::Field(v)) => v,
        _ => String::new(),
    };
    let store = D1Store::new(env.d1("DB")?);
    let db = crate::pages::db_source(&store);
    let src = crate::pages::source::SampleSource::get();
    let started = worker::Date::now().as_millis();
    match crate::pages::preview(src, &db, &field("title"), &field("body"), &field("lang")).await {
        Ok(html) => {
            let h = worker::Headers::new();
            h.set("content-type", "text/html; charset=utf-8")?;
            h.set("cache-control", "no-store")?;
            h.set("x-robots-tag", "noindex")?;
            h.set("content-security-policy", PREVIEW_CSP)?;
            // As on reader pages: the render cost of a long draft, for local measurement.
            let ms = worker::Date::now().as_millis().saturating_sub(started);
            h.set("server-timing", &format!("render;dur={ms}"))?;
            Ok(Response::ok(html)?.with_headers(h))
        }
        Err(e) => Response::error(format!("preview failed: {e}"), 500),
    }
}
