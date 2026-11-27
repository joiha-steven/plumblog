//! Every core locale key the Worker asks for from Rust, kept in one place by convention.
//! `scripts/check` reads every `key!("...")` in this crate and fails if one is missing from
//! `locales/en.toml`.

use plumblog_i18n::{Key, key};

/// Plain-text body of a 500 page.
pub const PAGE_FAILED: Key = key!("error.page_failed");

/// Heading of the post list in llms.txt.
pub const LLMS_POSTS: Key = key!("llms.posts");
