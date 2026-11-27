//! Every locale key this crate asks for from Rust. Keys live in the default theme's locale
//! files; `scripts/check` reads this file and fails if a key is missing there or if `Key(`
//! appears anywhere else in Rust code.

use plumblog_i18n::Key;

/// A post date: `{month} {day}, {year}`.
pub const DATE_SHORT: Key = Key("date.short");

/// Short month names, January first, filled into [`DATE_SHORT`].
pub const MONTHS: [Key; 12] = [
    Key("date.month_1"),
    Key("date.month_2"),
    Key("date.month_3"),
    Key("date.month_4"),
    Key("date.month_5"),
    Key("date.month_6"),
    Key("date.month_7"),
    Key("date.month_8"),
    Key("date.month_9"),
    Key("date.month_10"),
    Key("date.month_11"),
    Key("date.month_12"),
];
