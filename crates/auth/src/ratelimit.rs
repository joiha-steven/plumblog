//! Sign-in rate limiting policy. The Worker keeps one `login_failures` row per key (the SHA-256
//! hex of [`login_key`] or [`ip_key`], never the raw login or IP), writes only on a failed
//! attempt ([`record_failure`]) and asks [`decide`] before checking a password. Times are UTC
//! milliseconds.

use plumblog_model::{Error, ErrorCode, Result};
use std::net::{IpAddr, Ipv6Addr};

/// Failures count within a fixed window that starts at the first failure.
pub const WINDOW_MS: i64 = 15 * 60 * 1000;
/// From this many failures on any key, a Turnstile check is required (or, without Turnstile,
/// the key is locked until its window ends).
pub const TURNSTILE_AFTER: i64 = 5;
/// From this many, sign-in is refused before the password is even checked.
pub const LOCK_AFTER: i64 = 20;

/// One `login_failures` row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Failures {
    pub count: i64,
    pub window_start: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    NeedTurnstile,
    Locked { until: i64 },
}

impl Decision {
    /// The error to answer with, or `None` to go on checking the password. `NeedTurnstile`
    /// is an error only when the request carries no valid Turnstile token.
    #[must_use]
    pub fn error(self) -> Option<Error> {
        match self {
            Self::Allow => None,
            Self::NeedTurnstile => Some(Error::new(
                ErrorCode::TurnstileRequired,
                "too many failed sign-ins; Turnstile required",
            )),
            Self::Locked { until } => Some(Error::new(
                ErrorCode::Locked,
                format!("too many failed sign-ins; locked until {until}"),
            )),
        }
    }
}

/// The window a row is in, if still open at `now`. A start in the future (clock skew, bad
/// row) counts from `now`, so a lock can never last longer than one window.
fn open_window(row: &Failures, now: i64) -> Option<i64> {
    let start = row.window_start.min(now);
    (now < start.saturating_add(WINDOW_MS)).then_some(start)
}

/// What to do with a sign-in attempt, given the rows found for its keys (missing rows are
/// simply absent from `rows`).
#[must_use]
pub fn decide(rows: &[Failures], now: i64, turnstile_configured: bool) -> Decision {
    let mut locked_until: Option<i64> = None;
    let mut turnstile = false;
    for row in rows {
        let Some(start) = open_window(row, now) else {
            continue;
        };
        let end = start.saturating_add(WINDOW_MS);
        let lock =
            row.count >= LOCK_AFTER || (row.count >= TURNSTILE_AFTER && !turnstile_configured);
        if lock {
            locked_until = Some(locked_until.map_or(end, |u| u.max(end)));
        } else if row.count >= TURNSTILE_AFTER {
            turnstile = true;
        }
    }
    match (locked_until, turnstile) {
        (Some(until), _) => Decision::Locked { until },
        (None, true) => Decision::NeedTurnstile,
        (None, false) => Decision::Allow,
    }
}

/// The row to write after a failed attempt: a closed (or missing) window restarts at `now`.
#[must_use]
pub fn record_failure(row: Option<Failures>, now: i64) -> Failures {
    if let Some(r) = row
        && let Some(start) = open_window(&r, now)
    {
        return Failures {
            count: r.count.max(0).saturating_add(1),
            window_start: start,
        };
    }
    Failures {
        count: 1,
        window_start: now,
    }
}

/// Text to hash for the per-login key. Lowercased like the stretch salt, so `Alice` and
/// `alice` share one budget.
#[must_use]
pub fn login_key(login: &str) -> String {
    format!("login:{}", login.to_lowercase())
}

/// Text to hash for the per-address key. IPv6 is cut to its /64 (one subscriber usually owns a
/// whole /64, so per-address limits would be free to dodge); IPv4-mapped IPv6 counts as IPv4.
///
/// # Errors
///
/// [`ErrorCode::InvalidInput`] when `ip` is not a plain IPv4 or IPv6 address (no port, zone,
/// brackets or whitespace); the caller logs it and must not skip limiting silently.
pub fn ip_key(ip: &str) -> Result<String> {
    let addr: IpAddr = ip.parse().map_err(|_| {
        Error::new(
            ErrorCode::InvalidInput,
            "client address is not an IP address",
        )
    })?;
    Ok(match addr {
        IpAddr::V4(v4) => format!("ip:{v4}"),
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                format!("ip:{v4}")
            } else {
                let s = v6.segments();
                let net = Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0);
                format!("ip:{net}/64")
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::{
        Decision, Failures, LOCK_AFTER, TURNSTILE_AFTER, WINDOW_MS, decide, ip_key, login_key,
        record_failure,
    };
    use plumblog_model::ErrorCode;

    const NOW: i64 = 1_700_000_000_000;

    fn row(count: i64, age: i64) -> Failures {
        Failures {
            count,
            window_start: NOW - age,
        }
    }

    #[test]
    fn thresholds_with_turnstile() {
        assert_eq!(decide(&[], NOW, true), Decision::Allow);
        assert_eq!(
            decide(&[row(TURNSTILE_AFTER - 1, 0)], NOW, true),
            Decision::Allow
        );
        assert_eq!(
            decide(&[row(TURNSTILE_AFTER, 0)], NOW, true),
            Decision::NeedTurnstile
        );
        assert_eq!(
            decide(&[row(LOCK_AFTER - 1, 60_000)], NOW, true),
            Decision::NeedTurnstile
        );
        assert_eq!(
            decide(&[row(LOCK_AFTER, 60_000)], NOW, true),
            Decision::Locked {
                until: NOW - 60_000 + WINDOW_MS
            }
        );
    }

    #[test]
    fn without_turnstile_five_failures_lock() {
        assert_eq!(decide(&[row(4, 0)], NOW, false), Decision::Allow);
        assert_eq!(
            decide(&[row(5, 1_000)], NOW, false),
            Decision::Locked {
                until: NOW - 1_000 + WINDOW_MS
            }
        );
    }

    #[test]
    fn any_key_counts_and_the_strictest_wins() {
        let login = row(2, 0);
        let ip = row(7, 0);
        assert_eq!(decide(&[login, ip], NOW, true), Decision::NeedTurnstile);
        let locked_early = row(25, 600_000);
        let locked_late = row(21, 60_000);
        assert_eq!(
            decide(&[locked_early, ip, locked_late], NOW, true),
            Decision::Locked {
                until: NOW - 60_000 + WINDOW_MS
            }
        );
    }

    #[test]
    fn closed_windows_are_ignored_and_restart() {
        let old = row(100, WINDOW_MS);
        assert_eq!(decide(&[old], NOW, true), Decision::Allow);
        assert_eq!(
            decide(&[row(100, WINDOW_MS - 1)], NOW, true),
            Decision::Locked { until: NOW + 1 }
        );
        assert_eq!(
            record_failure(Some(old), NOW),
            Failures {
                count: 1,
                window_start: NOW
            }
        );
        assert_eq!(
            record_failure(None, NOW),
            Failures {
                count: 1,
                window_start: NOW
            }
        );
        assert_eq!(
            record_failure(Some(row(4, 10)), NOW),
            Failures {
                count: 5,
                window_start: NOW - 10
            }
        );
    }

    #[test]
    fn garbage_rows_cannot_lock_forever_or_overflow() {
        let future = Failures {
            count: 50,
            window_start: i64::MAX,
        };
        assert_eq!(
            decide(&[future], NOW, true),
            Decision::Locked {
                until: NOW + WINDOW_MS
            }
        );
        assert_eq!(record_failure(Some(future), NOW).window_start, NOW);
        let huge = Failures {
            count: i64::MAX,
            window_start: NOW,
        };
        assert_eq!(record_failure(Some(huge), NOW).count, i64::MAX);
        let negative = Failures {
            count: -5,
            window_start: NOW,
        };
        assert_eq!(decide(&[negative], NOW, true), Decision::Allow);
        assert_eq!(record_failure(Some(negative), NOW).count, 1);
        let ancient = Failures {
            count: 50,
            window_start: i64::MIN,
        };
        assert_eq!(decide(&[ancient], NOW, true), Decision::Allow);
        assert_eq!(decide(&[future], i64::MAX, true), Decision::Allow);
    }

    #[test]
    fn decision_errors_use_stable_codes() {
        assert!(Decision::Allow.error().is_none());
        assert_eq!(
            Decision::NeedTurnstile.error().unwrap().code,
            ErrorCode::TurnstileRequired
        );
        assert_eq!(
            Decision::Locked { until: 5 }.error().unwrap().code,
            ErrorCode::Locked
        );
    }

    #[test]
    fn keys() {
        assert_eq!(login_key("Alice"), "login:alice");
        assert_eq!(login_key("\u{c9}MILE"), "login:\u{e9}mile");
        assert_eq!(ip_key("203.0.113.7").unwrap(), "ip:203.0.113.7");
        assert_eq!(ip_key("0.0.0.0").unwrap(), "ip:0.0.0.0");
        assert_eq!(
            ip_key("2001:db8:1:2:3:4:5:6").unwrap(),
            "ip:2001:db8:1:2::/64"
        );
        // Same /64, same key; different /64, different key.
        assert_eq!(
            ip_key("2001:DB8:1:2:ffff::1").unwrap(),
            ip_key("2001:db8:1:2::").unwrap()
        );
        assert_ne!(
            ip_key("2001:db8:1:3::1").unwrap(),
            ip_key("2001:db8:1:2::1").unwrap()
        );
        assert_eq!(ip_key("::1").unwrap(), "ip:::/64");
        assert_eq!(ip_key("::ffff:203.0.113.7").unwrap(), "ip:203.0.113.7");
        assert_eq!(ip_key("::ffff:cb00:7107").unwrap(), "ip:203.0.113.7");
        for bad in [
            "",
            " 203.0.113.7",
            "203.0.113.7 ",
            "203.0.113.7:443",
            "[2001:db8::1]",
            "fe80::1%eth0",
            "256.1.1.1",
            "1.2.3",
            "01.2.3.4",
            "2001:db8::1::2",
            "localhost",
            "203.0.113.7, 10.0.0.1",
            "\u{661}.2.3.4",
        ] {
            assert_eq!(
                ip_key(bad).unwrap_err().code,
                ErrorCode::InvalidInput,
                "{bad:?}"
            );
        }
    }
}
