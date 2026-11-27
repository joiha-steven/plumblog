//! Scheduled jobs (the Worker's cron trigger). Today one: system log retention, daily.

use plumblog_store::{RETENTION_JOB, retention_job};
use plumblog_syslog::{Retention, Scope, request_id};
use worker::{Env, console_error, console_log, js_sys};

use crate::d1::D1Store;

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

/// Runs every job. Failures go to the Worker log and, where storage works, to the system log
/// as a `job.run` entry with the error code; a cron handler has no one to return them to.
pub async fn run(env: &Env) {
    let scope = Scope::new(request_id(None, job_id), None);
    let prefix = scope.log_prefix();
    let store = match env.d1("DB") {
        Ok(db) => D1Store::new(db),
        Err(e) => {
            console_error!("{prefix} job.run {RETENTION_JOB}: no D1 binding DB: {e}");
            return;
        }
    };
    let now = worker::Date::now().as_millis();
    let report = |m: &str| console_error!("{m}");
    match retention_job(&store, &scope, Retention::default(), now, report).await {
        Ok(deleted) => console_log!("{prefix} job.run {RETENTION_JOB} deleted={deleted}"),
        Err(e) => console_error!("{prefix} job.run {RETENTION_JOB} failed: {e}"),
    }
}
