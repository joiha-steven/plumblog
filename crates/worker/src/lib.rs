use worker::{Context, Env, Request, Response, Result, event};

#[cfg(feature = "contract-tests")]
mod contract;
pub mod d1;

/// Plain function so routing is trivial to unit test without the runtime.
fn route(method: &str, path: &str) -> (u16, &'static str) {
    match (method, path) {
        ("GET", "/healthz") => (200, "ok"),
        _ => (404, "not found"),
    }
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    #[cfg(feature = "contract-tests")]
    if let Some(res) = contract::handle(&req, &env).await? {
        return Ok(res);
    }
    #[cfg(not(feature = "contract-tests"))]
    let _ = env;
    let (status, body) = route(req.method().as_ref(), req.path().as_str());
    Ok(Response::ok(body)?.with_status(status))
}

#[cfg(test)]
mod tests {
    use super::route;

    #[test]
    fn health_ok_and_rest_404() {
        assert_eq!(route("GET", "/healthz"), (200, "ok"));
        assert_eq!(route("POST", "/healthz").0, 404);
        assert_eq!(route("GET", "/").0, 404);
        assert_eq!(route("GET", "/__contract").0, 404);
    }
}
