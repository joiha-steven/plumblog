#[cfg(feature = "contract-tests")]
mod contract;
pub mod d1;
mod jobs;
mod keys;
mod pages;
pub mod webhooks;

use worker::{Context, Env, Request, Response, Result, ScheduleContext, ScheduledEvent, event};

#[event(fetch)]
async fn fetch(req: Request, env: Env, ctx: Context) -> Result<Response> {
    // Test builds only: /__contract empties the database, so it never exists in a release build.
    #[cfg(feature = "contract-tests")]
    if let Some(res) = contract::handle(&req, &env).await? {
        return Ok(res);
    }
    #[cfg(feature = "contract-tests")]
    if let Some(res) = webhooks::test_routes::handle(&req, &env, &ctx).await? {
        return Ok(res);
    }
    match (req.method().as_ref(), req.path().as_str()) {
        ("GET", "/healthz") => Response::ok("ok"),
        ("GET" | "HEAD", _) => pages::serve(&req, &env, &ctx).await,
        _ => Ok(Response::empty()?.with_status(405)),
    }
}

/// The one cron trigger, every 10 minutes; `jobs::run` picks the jobs by scheduled time.
#[event(scheduled)]
async fn scheduled(event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    jobs::run(&env, event.schedule()).await;
}
