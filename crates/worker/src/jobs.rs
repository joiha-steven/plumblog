//! Scheduled jobs. One cron trigger, every 10 minutes (the free plan allows few triggers),
//! dispatched here by the scheduled time: every run publishes due posts and sends due
//! webhook deliveries, then pre-renders reader pages into R2 and deletes retired ones
//! (`pages::prerender`); the run scheduled 03:10-03:19 UTC also prunes the system log and old
//! deliveries. Each job stands alone: one failing never skips the others.

use plumblog_store::{RETENTION_JOB, retention_job};
use plumblog_syslog::{Retention, Scope, request_id};
use plumblog_webhook::{PRUNE_JOB, prune_job};
use worker::{Env, console_error, console_log, js_sys};

use crate::d1::D1Store;
use crate::webhooks;

/// A cron run has no `cf-ray`, so its request id is generated. It only links log lines to
/// the admin entry, so `Math.random` is enough.
fn job_id() -> String {
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a random float in [0, 1) scaled into u64 range"
    )]
    let n = (js_sys::Math::random() * 1e15) as u64;
    format!("job-{n:x}")
}

const DAY_MS: f64 = 86_400_000.0;
/// The daily jobs' slot: minutes 190..200 of the UTC day (03:10-03:19), which holds exactly
/// one `*/10` run. A missed run skips one day; retention catches up the next.
const DAILY_FROM_MIN: f64 = 190.0;
const DAILY_TO_MIN: f64 = 200.0;

/// Whether the run scheduled at `scheduled_ms` (UTC) also runs the daily jobs.
#[must_use]
pub fn is_daily(scheduled_ms: f64) -> bool {
    let minute = (scheduled_ms.rem_euclid(DAY_MS) / 60_000.0).floor();
    (DAILY_FROM_MIN..DAILY_TO_MIN).contains(&minute)
}

/// Runs the jobs due at `scheduled_ms`. Failures go to the Worker log and, where storage
/// works, to the system log with an error code; a cron handler has no one to return them to.
pub async fn run(env: &Env, scheduled_ms: f64) {
    // Test builds: count only this run's calls (`crate::subrequests`).
    #[cfg(feature = "contract-tests")]
    crate::subrequests::take();
    let scope = Scope::new(request_id(None, job_id), None);
    let prefix = scope.log_prefix();
    let store = match env.d1("DB") {
        Ok(db) => D1Store::new(db),
        Err(e) => {
            console_error!("{prefix} job.run: no D1 binding DB: {e}");
            return;
        }
    };
    // Subrequests used so far (see webhooks::SUBREQUESTS); deliveries get what is left.
    let mut used = webhooks::publish_job(&store, &scope).await;
    // Test builds: the publish job alone too, so a test can measure its calls per post.
    #[cfg(feature = "contract-tests")]
    let publish = (crate::subrequests::counted(), used);
    let daily = is_daily(scheduled_ms);
    if daily {
        let now = worker::Date::now().as_millis();
        let report = |m: &str| console_error!("{m}");
        match retention_job(&store, &scope, Retention::default(), now, report).await {
            Ok(deleted) => console_log!("{prefix} job.run {RETENTION_JOB} deleted={deleted}"),
            Err(e) => console_error!("{prefix} job.run {RETENTION_JOB} failed: {e}"),
        }
        let now = i64::try_from(now).unwrap_or(i64::MAX);
        match prune_job(&store, &scope, now, report).await {
            Ok(deleted) => console_log!("{prefix} job.run {PRUNE_JOB} deleted={deleted}"),
            Err(e) => console_error!("{prefix} job.run {PRUNE_JOB} failed: {e}"),
        }
        used += webhooks::DAILY_JOBS;
    }
    used += webhooks::deliver_job(
        env,
        &store,
        &scope,
        used,
        plumblog_webhook::MAX_PER_RUN,
        daily,
    )
    .await;
    // Last: a CPU cut-off then loses only this work, which the next run redoes.
    let pages = crate::pages::store_cron(
        env,
        &store,
        &scope,
        webhooks::SUBREQUESTS.saturating_sub(used),
    )
    .await;
    // Test builds: the calls this run made next to the jobs' own count, which
    // tools/worker-test/r2cron.test.mjs holds to webhooks::SUBREQUESTS.
    #[cfg(feature = "contract-tests")]
    console_log!(
        "{prefix} job.run subrequests counted={} budgeted={} publish_counted={} publish_budgeted={}",
        crate::subrequests::take(),
        used + pages,
        publish.0,
        publish.1
    );
    #[cfg(not(feature = "contract-tests"))]
    let _ = pages;
}

#[cfg(test)]
mod tests {
    use super::is_daily;

    const MIN: f64 = 60_000.0;
    /// 2026-11-27T00:00:00Z.
    const DAY: f64 = 1_795_737_600_000.0;

    #[test]
    fn only_the_0310_run_is_daily() {
        let daily: Vec<f64> = (0..144)
            .map(|i| DAY + f64::from(i) * 10.0 * MIN)
            .filter(|t| is_daily(*t))
            .collect();
        assert_eq!(daily, [DAY + 190.0 * MIN], "exactly one */10 run a day");
        // A late start within the slot still counts; the next slot does not.
        assert!(is_daily(DAY + 199.0 * MIN + 59_000.0));
        assert!(!is_daily(DAY + 200.0 * MIN));
        assert!(!is_daily(DAY + 189.0 * MIN));
    }
}
