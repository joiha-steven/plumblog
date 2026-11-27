//! Retention: how much log to keep on the free plan. Pruning is planned here and run by a
//! scheduled job, never per event, so the log costs one write per event plus one daily batch.

const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// Keep events younger than `max_age_days`, and at most `max_rows` of them (newest win).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Retention {
    pub max_age_days: u32,
    pub max_rows: u64,
}

impl Default for Retention {
    /// 90 days covers a quarter of history; 10,000 rows of about 300 bytes is ~3 MB of the
    /// 5 GB D1 free storage, and pruning stays far below the 100,000 daily writes.
    fn default() -> Self {
        Self {
            max_age_days: 90,
            max_rows: 10_000,
        }
    }
}

/// What the storage layer deletes: rows older than `older_than_ms`, then the
/// `delete_oldest` oldest rows that remain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Prune {
    pub older_than_ms: u64,
    pub delete_oldest: u64,
}

impl Retention {
    /// Plans a prune. `rows_after_age_cut` is the row count once the age cut is applied
    /// (the store counts it with `at_ms >= older_than_ms`), so both limits hold afterwards.
    #[must_use]
    pub fn plan(self, now_ms: u64, rows_after_age_cut: u64) -> Prune {
        let max_age = u64::from(self.max_age_days).saturating_mul(DAY_MS);
        Prune {
            older_than_ms: now_ms.saturating_sub(max_age),
            delete_oldest: rows_after_age_cut.saturating_sub(self.max_rows),
        }
    }

    /// The age cut alone, for the store to count what would remain before calling
    /// [`Retention::plan`].
    #[must_use]
    pub fn age_cut_ms(self, now_ms: u64) -> u64 {
        self.plan(now_ms, 0).older_than_ms
    }
}

#[cfg(test)]
mod tests {
    use super::{DAY_MS, Prune, Retention};

    #[test]
    fn plan_applies_age_then_row_cap() {
        let r = Retention {
            max_age_days: 30,
            max_rows: 100,
        };
        let now = 1_000 * DAY_MS;
        assert_eq!(
            r.plan(now, 80),
            Prune {
                older_than_ms: 970 * DAY_MS,
                delete_oldest: 0
            }
        );
        assert_eq!(r.plan(now, 130).delete_oldest, 30);
        assert_eq!(r.age_cut_ms(now), 970 * DAY_MS);
        // A clock before the epoch window never underflows.
        assert_eq!(r.plan(5, 0).older_than_ms, 0);
        let huge = Retention {
            max_age_days: u32::MAX,
            max_rows: 0,
        };
        assert_eq!(
            huge.plan(now, 3),
            Prune {
                older_than_ms: 0,
                delete_oldest: 3
            }
        );
        assert_eq!(Retention::default().max_rows, 10_000);
    }
}
