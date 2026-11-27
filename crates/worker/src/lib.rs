mod api;
#[cfg(feature = "contract-tests")]
mod contract;
#[cfg(feature = "contract-tests")]
mod crypto_test;
pub mod d1;
mod keys;
mod pages;
mod webcrypto;

use worker::{Context, Env, Request, Response, Result, event};

#[event(fetch)]
async fn fetch(req: Request, env: Env, ctx: Context) -> Result<Response> {
    // Test builds only: /__contract empties the database, so it never exists in a release build.
    #[cfg(feature = "contract-tests")]
    let mut req = req;
    #[cfg(feature = "contract-tests")]
    if let Some(res) = contract::handle(&req, &env).await? {
        return Ok(res);
    }
    #[cfg(feature = "contract-tests")]
    if let Some(res) = crypto_test::handle(&mut req, &env).await? {
        return Ok(res);
    }
    if req.path().starts_with(api::PREFIX) {
        return api::handle(req, &env).await;
    }
    match (req.method().as_ref(), req.path().as_str()) {
        ("GET", "/healthz") => Response::ok("ok"),
        ("GET" | "HEAD", _) => pages::serve(&req, &env, &ctx).await,
        _ => Ok(Response::empty()?.with_status(405)),
    }
}
