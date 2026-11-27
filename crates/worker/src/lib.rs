mod admin;
#[cfg(feature = "contract-tests")]
mod contract;
pub mod d1;
mod jobs;
mod keys;
mod pages;

use worker::{Context, Env, Request, Response, Result, ScheduleContext, ScheduledEvent, event};

#[event(fetch)]
async fn fetch(req: Request, env: Env, ctx: Context) -> Result<Response> {
    // Test builds only: /__contract empties the database, so it never exists in a release build.
    #[cfg(feature = "contract-tests")]
    if let Some(res) = contract::handle(&req, &env).await? {
        return Ok(res);
    }
    let path = req.path();
    match (req.method().as_ref(), path.as_str()) {
        ("GET", "/healthz") => Response::ok("ok"),
        ("GET" | "HEAD", p) if p == admin::PREFIX || p.starts_with("/admin/") => {
            admin::serve(&req).await
        }
        ("GET" | "HEAD", _) => pages::serve(&req, &env, &ctx).await,
        _ => Ok(Response::empty()?.with_status(405)),
    }
}

/// The cron trigger: daily, for system log retention (docs/ARCHITECTURE.md, System log).
#[event(scheduled)]
async fn scheduled(_event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    jobs::run(&env).await;
}
