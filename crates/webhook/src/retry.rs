//! When to try again, and what an HTTP result means.

use plumblog_model::stable_codes;

const MINUTE_MS: i64 = 60 * 1000;
const HOUR_MS: i64 = 60 * MINUTE_MS;

/// Waits after failed attempts 1, 2 and 3; attempt 4 is the last.
pub const RETRY_DELAYS_MS: [i64; 3] = [10 * MINUTE_MS, HOUR_MS, 6 * HOUR_MS];
/// Attempt 1 plus one per retry delay.
pub const MAX_ATTEMPTS: i64 = 4;
/// Most deliveries attempted per cron run: the free plan allows 50 subrequests per
/// invocation, shared with the rest of the run.
pub const MAX_PER_RUN: u32 = 40;
/// How long a claimed delivery is held before another run may take it, if the run that
/// claimed it dies mid-send. Longer than any single run can last.
pub const LEASE_MS: i64 = 15 * MINUTE_MS;
/// Finished deliveries are kept this long (by creation time) for the admin, then pruned.
pub const KEEP_MS: i64 = 7 * 24 * HOUR_MS;
/// Most finished deliveries pruned per run; a backlog drains over later runs.
pub const PRUNE_PER_RUN: u32 = 1_000;

/// When to try again after failed attempt number `attempt` (1-based) at `now_ms`; `None`
/// when that was the last attempt.
#[must_use]
pub fn next_attempt_at(attempt: i64, now_ms: i64) -> Option<i64> {
    let i = usize::try_from(attempt.checked_sub(1)?).ok()?;
    RETRY_DELAYS_MS.get(i).map(|d| now_ms.saturating_add(*d))
}

stable_codes! {
    /// Why an attempt failed; stored in `webhook_deliveries.last_error` and logged, so a code
    /// is never renamed once shipped. `redirect`: a 3xx (not followed, the new location could
    /// be anywhere); `status`: 4xx/5xx; `url_blocked`: the URL no longer passes the guard;
    /// `key_invalid`: `WEBHOOK_KEY` missing or malformed; `secret_unreadable`: the stored
    /// secret is not sealed or does not open; `disabled`: the webhook was disabled after the
    /// event; `abandoned`: leases ran out more times than there are attempts (runs kept
    /// dying); `crypto`: the platform's crypto failed while signing.
    pub enum Failure {
        Redirect => "redirect",
        Status => "status",
        Timeout => "timeout",
        Network => "network",
        UrlBlocked => "url_blocked",
        KeyInvalid => "key_invalid",
        SecretUnreadable => "secret_unreadable",
        Disabled => "disabled",
        Abandoned => "abandoned",
        Crypto => "crypto",
    }
}

impl Failure {
    /// Whether waiting can fix it (a missing key can be set; a blocked URL or unreadable
    /// secret stays so until the owner edits the webhook, which makes new deliveries).
    #[must_use]
    pub const fn retryable(self) -> bool {
        match self {
            Self::Redirect
            | Self::Status
            | Self::Timeout
            | Self::Network
            | Self::KeyInvalid
            | Self::Crypto => true,
            Self::UrlBlocked | Self::SecretUnreadable | Self::Disabled | Self::Abandoned => false,
        }
    }
}

/// What the transport reports when no status came back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpError {
    Timeout,
    Network,
    /// A redirect the platform hides (an opaque redirect: status 0, no location). Never
    /// followed, same as a 3xx.
    Redirect,
}

/// One attempt's result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Delivered {
        status: u16,
    },
    Failed {
        status: Option<u16>,
        failure: Failure,
    },
}

/// 2xx is delivered; anything else failed. A 410 Gone is an ordinary failure: disabling
/// webhooks on a receiver's say-so is left to the owner.
#[must_use]
pub fn classify(result: Result<u16, HttpError>) -> Outcome {
    match result {
        Ok(status @ 200..=299) => Outcome::Delivered { status },
        Ok(status @ 300..=399) => Outcome::Failed {
            status: Some(status),
            failure: Failure::Redirect,
        },
        Ok(status) => Outcome::Failed {
            status: Some(status),
            failure: Failure::Status,
        },
        Err(HttpError::Timeout) => Outcome::Failed {
            status: None,
            failure: Failure::Timeout,
        },
        Err(HttpError::Network) => Outcome::Failed {
            status: None,
            failure: Failure::Network,
        },
        Err(HttpError::Redirect) => Outcome::Failed {
            status: None,
            failure: Failure::Redirect,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Failure, HttpError, MAX_ATTEMPTS, Outcome, RETRY_DELAYS_MS, classify, next_attempt_at,
    };

    #[test]
    fn schedule_is_10_min_1_h_6_h_then_done() {
        let now = 1_000_000;
        assert_eq!(next_attempt_at(1, now), Some(now + 600_000));
        assert_eq!(next_attempt_at(2, now), Some(now + 3_600_000));
        assert_eq!(next_attempt_at(3, now), Some(now + 21_600_000));
        assert_eq!(next_attempt_at(4, now), None);
        assert_eq!(next_attempt_at(5, now), None);
        assert_eq!(next_attempt_at(0, now), None);
        assert_eq!(next_attempt_at(-3, now), None);
        assert_eq!(
            MAX_ATTEMPTS,
            i64::try_from(RETRY_DELAYS_MS.len()).unwrap() + 1
        );
    }

    #[test]
    fn classification() {
        assert_eq!(classify(Ok(200)), Outcome::Delivered { status: 200 });
        assert_eq!(classify(Ok(204)), Outcome::Delivered { status: 204 });
        for (got, status, failure) in [
            (Ok(301), Some(301), Failure::Redirect),
            (Ok(308), Some(308), Failure::Redirect),
            (Ok(400), Some(400), Failure::Status),
            (Ok(410), Some(410), Failure::Status),
            (Ok(500), Some(500), Failure::Status),
            (Ok(100), Some(100), Failure::Status),
            (Err(HttpError::Timeout), None, Failure::Timeout),
            (Err(HttpError::Network), None, Failure::Network),
            (Err(HttpError::Redirect), None, Failure::Redirect),
        ] {
            assert_eq!(
                classify(got),
                Outcome::Failed { status, failure },
                "{got:?}"
            );
        }
    }

    #[test]
    fn failure_codes_are_pinned() {
        let codes: Vec<(&str, bool)> = Failure::ALL
            .iter()
            .map(|f| (f.as_str(), f.retryable()))
            .collect();
        assert_eq!(
            codes,
            [
                ("redirect", true),
                ("status", true),
                ("timeout", true),
                ("network", true),
                ("url_blocked", false),
                ("key_invalid", true),
                ("secret_unreadable", false),
                ("disabled", false),
                ("abandoned", false),
                ("crypto", true),
            ]
        );
    }
}
