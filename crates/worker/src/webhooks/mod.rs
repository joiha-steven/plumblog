//! Outgoing webhooks on the Worker: `WebCrypto`, `fetch`, the `WEBHOOK_KEY` secret, and the
//! jobs that queue and send deliveries (logic in `plumblog_webhook`).

mod crypto;
mod http;
#[cfg(feature = "contract-tests")]
pub mod test_routes;

use plumblog_model::{PostId, Result};
use plumblog_store::{PostStatus, PostStore, WebhookStore};
use plumblog_syslog::{EventKind, Scope};
use plumblog_webhook::{
    DELIVERY_JOB, MAX_PER_RUN, Policy, PostInfo, SealKey, Sender, enqueue, event_for,
};
use worker::{Env, console_error, console_log};

use crate::d1::D1Store;
pub use crypto::WebCrypto;
pub use http::Fetch;

/// Worker secret holding the key that seals signing secrets (base64 of 32 bytes).
pub const KEY_VAR: &str = "WEBHOOK_KEY";

/// Subject of the `job.run` entry for scheduled publishing.
pub const PUBLISH_JOB: &str = "posts.publish_due";

/// Free plan: 50 subrequests per invocation. Counted conservatively as if every D1 call
/// counts like a `fetch` (Cloudflare documents `fetch` and cache calls; D1 is not clearly
/// separate), so a run cannot fail half-way on the cap and resend after the lease.
pub const SUBREQUESTS: u32 = 50;
/// A delivery run's own calls before sending: claim, list webhooks.
pub const DELIVERY_FIXED: u32 = 2;
/// Per delivery: the `fetch`, `finish_attempt`, and its `webhook.delivery` log entry.
pub const PER_DELIVERY: u32 = 3;
/// Per post published by cron: `get_post`, list webhooks, queue its deliveries.
pub const PER_PUBLISHED: u32 = 3;
/// The publish job's own calls: `publish_due` and its `job.run` entry.
pub const PUBLISH_FIXED: u32 = 2;
/// The daily jobs: system log prune + entry, delivery prune + entry.
pub const DAILY_JOBS: u32 = 4;
/// Most posts one cron run publishes: (50 - 2 - 4) / 3 = 14, so publishing plus the daily
/// jobs always fit, whatever deliveries are left. Further due posts go live 10 min later
/// with their events; none is published without its `post.published` being queued.
pub const PUBLISH_PER_RUN: u32 = (SUBREQUESTS - PUBLISH_FIXED - DAILY_JOBS) / PER_PUBLISHED;

/// Subrequests the publish job uses when it published `n` posts.
#[must_use]
pub const fn publish_used(n: u32) -> u32 {
    if n == 0 {
        1
    } else {
        PUBLISH_FIXED + n * PER_PUBLISHED
    }
}

/// Most deliveries sent after a response (`waitUntil`, which gets 30 s past the response):
/// two slow receivers at the 10 s timeout still fit. The rest wait for the next cron run.
pub const AFTER_RESPONSE: u32 = 2;

/// How many deliveries fit after `used` subrequests: never more than
/// [`plumblog_webhook::MAX_PER_RUN`]. A normal cron run uses 1 (`publish_due`), so it sends
/// up to (50 - 1 - 2) / 3 = 15; the daily run 5 more (retention 2, prune 2, publish 1), 14.
#[must_use]
pub fn delivery_limit(used: u32) -> u32 {
    (SUBREQUESTS
        .saturating_sub(used)
        .saturating_sub(DELIVERY_FIXED)
        / PER_DELIVERY)
        .min(MAX_PER_RUN)
}

/// The `WEBHOOK_KEY` value, if the binding exists.
fn raw_key(env: &Env) -> Option<String> {
    env.secret(KEY_VAR).ok().map(|s| s.to_string())
}

/// Lets a test build deliver to the local receiver (`http://127.0.0.1:<port>`). Second lock
/// besides the cargo feature: only tools/wrangler.dev.toml sets it.
#[cfg(feature = "contract-tests")]
const TEST_RECEIVER_VAR: &str = "PLUMBLOG_WEBHOOK_TEST_RECEIVER";

/// The URL policy for this Worker. Always [`Policy::Production`] in a release build.
#[must_use]
pub fn policy(env: &Env) -> Policy {
    #[cfg(feature = "contract-tests")]
    if env
        .var(TEST_RECEIVER_VAR)
        .map(|v| v.to_string())
        .ok()
        .as_deref()
        == Some("allow-loopback-http")
    {
        return Policy::AllowLoopbackHttpForTests;
    }
    let _ = env;
    Policy::Production
}

fn now_ms() -> i64 {
    i64::try_from(worker::Date::now().as_millis()).unwrap_or(i64::MAX)
}

fn ms(t: i64) -> u64 {
    u64::try_from(t).unwrap_or(0)
}

/// What payloads say about post `id`, as stored now.
async fn post_info(store: &D1Store, id: PostId) -> Result<PostInfo> {
    let post = store.get_post(id).await?;
    let url = crate::pages::post_url(&post.lang, &post.slug).await;
    Ok(PostInfo::from_post(&post, url))
}

/// Publishes up to [`PUBLISH_PER_RUN`] due scheduled posts and queues `post.published` for
/// each, through the same [`event_for`] the write routes will use. Returns the subrequests
/// it used.
pub async fn publish_job(store: &D1Store, scope: &Scope) -> u32 {
    let prefix = scope.log_prefix();
    let report = |m: &str| console_error!("{m}");
    let now = now_ms();
    let ids = match store.publish_due(now, PUBLISH_PER_RUN).await {
        Ok(ids) => ids,
        Err(e) => {
            console_error!("{prefix} job.run {PUBLISH_JOB} failed: {e}");
            let event = scope
                .event(EventKind::ScheduledJob, ms(now))
                .subject(PUBLISH_JOB)
                .error(e.code);
            plumblog_syslog::record(store, event, report).await;
            return PUBLISH_FIXED;
        }
    };
    if ids.is_empty() {
        return publish_used(0);
    }
    let mut queued = 0usize;
    let mut failed: Vec<String> = Vec::new();
    for id in &ids {
        let queue = async {
            let after = post_info(store, *id).await?;
            // publish_due only moves scheduled -> published.
            let before = PostInfo {
                status: PostStatus::Scheduled,
                ..after.clone()
            };
            match event_for(Some(&before), Some(&after), false) {
                Some((event, post)) => enqueue(store, &WebCrypto, event, &post, now_ms()).await,
                None => Ok(Vec::new()),
            }
        };
        match queue.await {
            Ok(d) => queued += d.len(),
            Err(e) => {
                // The post is live; only its notification is lost. Logged for the owner.
                failed.push(id.0.to_string());
                console_error!("{prefix} webhook.enqueue_failed post={} error={e}", id.0);
            }
        }
    }
    console_log!(
        "{prefix} job.run {PUBLISH_JOB} published={} deliveries={queued} enqueue_failed={}",
        ids.len(),
        failed.len()
    );
    let mut event = scope
        .event(EventKind::ScheduledJob, ms(now))
        .subject(PUBLISH_JOB)
        .field("published", &ids.len().to_string())
        .field("deliveries", &queued.to_string());
    if !failed.is_empty() {
        // Post ids whose `post.published` was not queued: the owner can see which.
        event = event
            .field("enqueue_failed_posts", &failed.join(","))
            .error(plumblog_model::ErrorCode::WebhookDeliveryFailed);
    }
    plumblog_syslog::record(store, event, report).await;
    publish_used(u32::try_from(ids.len()).unwrap_or(u32::MAX))
}

/// Sends up to `max` due deliveries, within what is left of the subrequest budget after
/// `used`; with no room left it makes no call at all.
/// A missing or malformed `WEBHOOK_KEY` sends nothing and leaves every row due; it is
/// logged only when some webhook exists, so a blog without webhooks needs no key. The
/// system log gets it on the `daily` run only (one entry a day, not 144); other runs write
/// the Worker log line alone.
pub async fn deliver_job(
    env: &Env,
    store: &D1Store,
    scope: &Scope,
    used: u32,
    max: u32,
    daily: bool,
) {
    let limit = delivery_limit(used).min(max);
    if limit == 0 {
        return;
    }
    let prefix = scope.log_prefix();
    let report = |m: &str| console_error!("{m}");
    let raw = raw_key(env);
    if let Err(e) = SealKey::parse(raw.as_deref()) {
        match store.list_webhooks().await {
            Ok(w) if w.is_empty() => return,
            _ => {}
        }
        console_error!("{prefix} job.run {DELIVERY_JOB} not sent: {e}");
        if !daily {
            return;
        }
        let event = scope
            .event(EventKind::ScheduledJob, ms(now_ms()))
            .subject(DELIVERY_JOB)
            .error(e.code);
        plumblog_syslog::record(store, event, report).await;
        return;
    }
    let sender = Sender {
        store,
        crypto: &WebCrypto,
        http: &Fetch,
        raw_key: raw.as_deref(),
        policy: policy(env),
    };
    match sender.deliver_due(scope, now_ms, limit, report).await {
        Ok(run) if run.claimed == 0 => {}
        Ok(run) => console_log!(
            "{prefix} job.run {DELIVERY_JOB} claimed={} delivered={} failed={} skipped={}",
            run.claimed,
            run.delivered,
            run.failed,
            run.skipped
        ),
        Err(e) => console_error!("{prefix} job.run {DELIVERY_JOB} failed: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DAILY_JOBS, DELIVERY_FIXED, PER_DELIVERY, PUBLISH_PER_RUN, SUBREQUESTS, delivery_limit,
        publish_used,
    };

    #[test]
    fn runs_stay_inside_the_subrequest_cap() {
        for used in 0..=SUBREQUESTS - DELIVERY_FIXED {
            let n = delivery_limit(used);
            assert!(
                used + DELIVERY_FIXED + n * PER_DELIVERY <= SUBREQUESTS,
                "{used}"
            );
        }
        assert_eq!(delivery_limit(1), 15, "normal cron run");
        assert_eq!(delivery_limit(5), 14, "daily cron run");
        assert_eq!(delivery_limit(60), 0);
    }

    /// The whole cron run, worst case: the most posts published, the daily jobs, then
    /// deliveries in what is left (none claimed when nothing is left).
    #[test]
    fn a_cron_run_stays_inside_the_cap_on_the_publish_path() {
        assert_eq!(PUBLISH_PER_RUN, 14);
        for n in 0..=PUBLISH_PER_RUN {
            for daily in [0, DAILY_JOBS] {
                let used = publish_used(n) + daily;
                let d = delivery_limit(used);
                let deliver = if d == 0 {
                    0
                } else {
                    DELIVERY_FIXED + d * PER_DELIVERY
                };
                assert!(used + deliver <= SUBREQUESTS, "n={n} daily={daily}");
            }
        }
        assert_eq!(publish_used(PUBLISH_PER_RUN) + DAILY_JOBS, 48);
    }
}
