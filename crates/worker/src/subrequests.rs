//! Test builds count the binding and fetch calls this isolate makes, so a worker-test can hold
//! a cron run to the subrequest budget the jobs count by hand (`webhooks::SUBREQUESTS`). Every
//! D1 call goes through `D1Store`, every R2 call of the cron through `pages`, and the only
//! fetches are the theme's (ASSETS) and webhook deliveries; each calls [`count`] once.

#[cfg(feature = "contract-tests")]
static COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
/// `COUNT` at the last [`lap`], so each job's calls are its own.
#[cfg(feature = "contract-tests")]
static LAP: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// One subrequest made. A no-op in a release build.
#[inline]
pub fn count() {
    #[cfg(feature = "contract-tests")]
    COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// The calls counted since the last call, and a fresh start.
#[cfg(feature = "contract-tests")]
pub fn take() -> u32 {
    LAP.store(0, std::sync::atomic::Ordering::Relaxed);
    COUNT.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// Ends job `name`'s count: logs the calls since the previous lap next to the `budgeted` the
/// job reports, as `job.subrequests <name> counted=.. budgeted=..`. Every job `jobs::run`
/// starts must call this (tools/worker-test/subreq-guard.test.mjs lists them and requires a
/// worker-test asserting each). A no-op in a release build.
#[inline]
pub fn lap(name: &str, budgeted: u32) {
    #[cfg(feature = "contract-tests")]
    {
        use std::sync::atomic::Ordering::Relaxed;
        let now = COUNT.load(Relaxed);
        let counted = now - LAP.swap(now, Relaxed);
        worker::console_log!("job.subrequests {name} counted={counted} budgeted={budgeted}");
    }
    #[cfg(not(feature = "contract-tests"))]
    let _ = (name, budgeted);
}
