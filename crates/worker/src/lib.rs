mod pages;

use worker::{Context, Env, Request, Response, Result, event};

#[event(fetch)]
async fn fetch(req: Request, _env: Env, ctx: Context) -> Result<Response> {
    match (req.method().as_ref(), req.path().as_str()) {
        ("GET", "/healthz") => Response::ok("ok"),
        ("GET" | "HEAD", _) => pages::serve(&req, &ctx).await,
        _ => Ok(Response::empty()?.with_status(405)),
    }
}
