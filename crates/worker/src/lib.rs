mod admin;
mod api;
mod comments;
#[cfg(feature = "contract-tests")]
mod contract;
#[cfg(feature = "contract-tests")]
mod crypto_test;
pub mod d1;
mod jobs;
mod keys;
mod media;
mod pages;
mod subrequests;
mod theme_runtime;
mod webcrypto;
pub mod webhooks;

use worker::{Context, Env, Request, Response, Result, ScheduleContext, ScheduledEvent, event};

#[event(fetch)]
async fn fetch(mut req: Request, env: Env, ctx: Context) -> Result<Response> {
    // Test builds only: /__contract empties the database, so it never exists in a release build.
    #[cfg(feature = "contract-tests")]
    if let Some(res) = contract::instance(&req, &env) {
        return res;
    }
    #[cfg(feature = "contract-tests")]
    if let Some(res) = contract::handle(&req, &env).await? {
        return Ok(res);
    }
    #[cfg(feature = "contract-tests")]
    if let Some(res) = webhooks::test_routes::handle(&req, &env, &ctx).await? {
        return Ok(res);
    }
    #[cfg(feature = "contract-tests")]
    if let Some(res) = crypto_test::handle(&mut req, &env).await? {
        return Ok(res);
    }
    if req.path().starts_with(api::PREFIX) || req.path() == api::MCP_PATH {
        return api::handle(req, &env).await;
    }
    let path = req.path();
    if let ("POST", Some((lang, slug))) =
        (req.method().as_ref(), pages::routes::comment_form(&path))
    {
        return comments::handle(&mut req, &env, lang, slug).await;
    }
    match (req.method().as_ref(), path.as_str()) {
        ("GET", "/healthz") => healthz(&env),
        ("GET" | "HEAD", p) if p == admin::PREFIX || p.starts_with("/admin/") => {
            admin::serve(&req, &env).await
        }
        ("GET" | "HEAD", p) if p.starts_with(media::PREFIX) => media::serve(&req, &env).await,
        ("GET" | "HEAD", _) => {
            theme_runtime::prepare(&env).await;
            pages::serve(&req, &env, &ctx).await
        }
        _ => Ok(Response::empty()?.with_status(405)),
    }
}

/// Version header name; `blog deploy` waits for the new version id here before it calls a
/// deploy good (crates/cli deploy smoke check).
const VERSION_HEADER: &str = "x-plumblog-version";

/// `ok`, plus the running version's id from the `CF_VERSION_METADATA` binding. Without the
/// binding (an older config) there is simply no header.
fn healthz(env: &Env) -> Result<Response> {
    let mut res = Response::ok("ok")?;
    if let Some(id) = version_id(env) {
        res.headers_mut().set(VERSION_HEADER, &id)?;
    }
    Ok(res)
}

/// The running version's id, or `None` without the binding or when it is not a string.
pub(crate) fn version_id(env: &Env) -> Option<String> {
    worker::js_sys::Reflect::get(env, &"CF_VERSION_METADATA".into())
        .ok()
        .filter(worker::wasm_bindgen::JsValue::is_object)
        .and_then(|m| worker::js_sys::Reflect::get(&m, &"id".into()).ok())
        .and_then(|v| v.as_string())
}

/// The one cron trigger, every 10 minutes; `jobs::run` picks the jobs by scheduled time.
#[event(scheduled)]
async fn scheduled(event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    jobs::run(&env, event.schedule()).await;
}
