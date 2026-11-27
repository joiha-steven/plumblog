//! Test builds only (feature `contract-tests`, and the same dev var as `/__contract`):
//! drive webhooks through the real code until write routes exist
//! (tools/worker-test/webhooks.test.mjs).
//! - `POST /__webhooks/create?url=..&events=a,b[&policy=production]`: saves a webhook and
//!   returns `{"id":..,"secret":"whsec_.."}`, the only time the secret is shown.
//! - `POST /__webhooks/emit?event=..&post=<id>`: queues the event for that post and sends
//!   attempt 1 after the response (`wait_until`), as a write route will.
//! - `POST /__webhooks/deliver?policy=production`: one delivery run now, with the given
//!   policy, so a test can show the production guard refusing a row before sending.

use plumblog_model::{Error, ErrorCode, PostId};
use plumblog_syslog::{EventKind, Scope, request_id};
use plumblog_webhook::{Policy, Sender, WebhookEvent, create_webhook, enqueue};
use worker::{Context, Env, Request, Response, Result, console_error};

use super::{Fetch, WebCrypto, delivery_limit, now_ms, policy, post_info, raw_key};
use crate::d1::D1Store;

const PREFIX: &str = "/__webhooks/";

fn job_id() -> String {
    format!("test-{:x}", now_ms())
}

fn fail(status: u16, e: &Error) -> Result<Option<Response>> {
    let body = format!(
        "{{\"error\":\"{}\",\"message\":{:?}}}",
        e.code.as_str(),
        e.to_string()
    );
    Response::error(body, status).map(Some)
}

/// # Errors
/// Only when the response itself cannot be built.
pub async fn handle(req: &Request, env: &Env, ctx: &Context) -> Result<Option<Response>> {
    if !crate::contract::enabled(env) || req.method().as_ref() != "POST" {
        return Ok(None);
    }
    let Some(action) = req.path().strip_prefix(PREFIX).map(str::to_owned) else {
        return Ok(None);
    };
    let url = req.url()?;
    let q = |k: &str| {
        url.query_pairs()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.into_owned())
    };
    let pol = match q("policy").as_deref() {
        Some("production") => Policy::Production,
        _ => policy(env),
    };
    let cf_ray = req.headers().get("cf-ray").ok().flatten();
    let scope = Scope::new(request_id(cf_ray.as_deref(), job_id), None);
    let store = D1Store::new(env.d1("DB")?);
    let report = |m: &str| console_error!("{m}");
    match action.as_str() {
        "create" => {
            let events: Vec<WebhookEvent> = q("events")
                .unwrap_or_default()
                .split(',')
                .filter_map(WebhookEvent::parse)
                .collect();
            create(
                env,
                &store,
                &scope,
                &q("url").unwrap_or_default(),
                &events,
                pol,
            )
            .await
        }
        "emit" => {
            let Some(event) = q("event").as_deref().and_then(WebhookEvent::parse) else {
                return Response::error("event", 400).map(Some);
            };
            let id = PostId(q("post").and_then(|p| p.parse().ok()).unwrap_or(0));
            let queued = async {
                let post = post_info(&store, id).await?;
                enqueue(&store, &WebCrypto, event, &post, now_ms()).await
            }
            .await;
            match queued {
                Ok(ids) => {
                    // get_post, list webhooks, queue: 3 subrequests before the send.
                    let env = env.clone();
                    ctx.wait_until(async move {
                        if let Ok(db) = env.d1("DB") {
                            super::deliver_job(
                                &env,
                                &D1Store::new(db),
                                &scope,
                                3,
                                super::AFTER_RESPONSE,
                                true,
                            )
                            .await;
                        }
                    });
                    Response::ok(format!("{ids:?}")).map(Some)
                }
                Err(e) => fail(400, &e),
            }
        }
        "deliver" => {
            let raw = raw_key(env);
            let sender = Sender {
                store: &store,
                crypto: &WebCrypto,
                http: &Fetch,
                raw_key: raw.as_deref(),
                policy: pol,
            };
            match sender
                .deliver_due(&scope, now_ms, delivery_limit(0), report)
                .await
            {
                Ok(run) => Response::ok(format!("{run:?}")).map(Some),
                Err(e) => fail(500, &e),
            }
        }
        _ => Ok(None),
    }
}

async fn create(
    env: &Env,
    store: &D1Store,
    scope: &Scope,
    target: &str,
    events: &[WebhookEvent],
    pol: Policy,
) -> Result<Option<Response>> {
    let raw = raw_key(env);
    match create_webhook(
        store,
        &WebCrypto,
        raw.as_deref(),
        target,
        events,
        pol,
        now_ms(),
    )
    .await
    {
        Ok(c) => Response::ok(format!("{{\"id\":{},\"secret\":\"{}\"}}", c.id, c.secret)).map(Some),
        Err(e) => {
            console_error!("{} webhook.create_failed error={e}", scope.log_prefix());
            if e.code == ErrorCode::WebhookKeyInvalid {
                let event = scope
                    .event(EventKind::Error, u64::try_from(now_ms()).unwrap_or(0))
                    .subject("webhook.create")
                    .error(e.code);
                plumblog_syslog::record(store, event, |m: &str| console_error!("{m}")).await;
            }
            fail(400, &e)
        }
    }
}
