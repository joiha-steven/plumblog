//! Retention: how much log to keep on the free plan. Run by a daily scheduled job, never per
//! event, so the log costs one write per event plus one bounded batch a day.

const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// Keep events younger than `max_age_days`, and at most `max_rows` of them (newest win).
/// One run deletes at most `per_run` rows, oldest first; a backlog (a burst of events, a
/// lowered cap) drains over the following runs. D1 counts every deleted row once per table
/// and index (four for `system_log`), so 2,000 deletes cost about 8,000 of the free plan's
/// 100,000 rows written a day.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retention {
    pub max_age_days: u32,
    pub max_rows: u32,
    pub per_run: u32,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            max_age_days: 90,
            max_rows: 10_000,
            per_run: 2_000,
        }
    }
}

impl Retention {
    /// Events with `at_ms` below this are past the age limit.
    #[must_use]
    pub fn age_cut_ms(self, now_ms: u64) -> u64 {
        now_ms.saturating_sub(u64::from(self.max_age_days).saturating_mul(DAY_MS))
    }
}

#[cfg(test)]
mod tests {
    use super::{DAY_MS, Retention};

    #[test]
    fn age_cut_is_overflow_safe() {
        let r = Retention {
            max_age_days: 30,
            ..Retention::default()
        };
        let now = 1_000 * DAY_MS;
        assert_eq!(r.age_cut_ms(now), 970 * DAY_MS);
        assert_eq!(r.age_cut_ms(5), 0);
        let huge = Retention {
            max_age_days: u32::MAX,
            ..r
        };
        assert_eq!(huge.age_cut_ms(now), 0);
        assert_eq!(
            Retention::default(),
            Retention {
                max_age_days: 90,
                max_rows: 10_000,
                per_run: 2_000
            }
        );
    }
}
