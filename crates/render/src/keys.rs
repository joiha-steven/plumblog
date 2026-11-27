//! Every locale key this crate asks for from Rust, kept in one place by convention. Keys live
//! in the default theme's locale files; `scripts/check` reads every `key!("...")` in this
//! crate and fails if one is missing there.

use plumblog_i18n::{Key, key};

/// A post date: `{month} {day}, {year}`.
pub const DATE_SHORT: Key = key!("date.short");

/// Short month names, January first, filled into [`DATE_SHORT`].
pub const MONTHS: [Key; 12] = [
    key!("date.month_1"),
    key!("date.month_2"),
    key!("date.month_3"),
    key!("date.month_4"),
    key!("date.month_5"),
    key!("date.month_6"),
    key!("date.month_7"),
    key!("date.month_8"),
    key!("date.month_9"),
    key!("date.month_10"),
    key!("date.month_11"),
    key!("date.month_12"),
];
