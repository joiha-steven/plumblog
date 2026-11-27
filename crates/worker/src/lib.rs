#[cfg(feature = "contract-tests")]
mod contract;
pub mod d1;
mod keys;
mod pages;

use worker::{Context, Env, Request, Response, Result, event};

#[event(fetch)]
async fn fetch(req: Request, env: Env, ctx: Context) -> Result<Response> {
    // Test builds only: /__contract empties the database, so it never exists in a release build.
    #[cfg(feature = "contract-tests")]
    if let Some(res) = contract::handle(&req, &env).await? {
        return Ok(res);
    }
    #[cfg(not(feature = "contract-tests"))]
    let _ = env;
    match (req.method().as_ref(), req.path().as_str()) {
        ("GET", "/healthz") => Response::ok("ok"),
        ("GET" | "HEAD", _) => pages::serve(&req, &ctx).await,
        _ => Ok(Response::empty()?.with_status(405)),
    }
}
