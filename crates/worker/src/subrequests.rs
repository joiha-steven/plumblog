//! Test builds count the binding and fetch calls this isolate makes, so a worker-test can hold
//! a cron run to the subrequest budget the jobs count by hand (`webhooks::SUBREQUESTS`). Every
//! D1 call goes through `D1Store`, every R2 call of the cron through `pages`, and the only
//! fetches are the theme's (ASSETS) and webhook deliveries; each calls [`count`] once.

#[cfg(feature = "contract-tests")]
static COUNT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// One subrequest made. A no-op in a release build.
#[inline]
pub fn count() {
    #[cfg(feature = "contract-tests")]
    COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
}

/// The calls counted since the last call, and a fresh start.
#[cfg(feature = "contract-tests")]
pub fn take() -> u32 {
    COUNT.swap(0, std::sync::atomic::Ordering::Relaxed)
}

/// The calls counted so far in this run, without resetting.
#[cfg(feature = "contract-tests")]
pub fn counted() -> u32 {
    COUNT.load(std::sync::atomic::Ordering::Relaxed)
}
