//! Every core locale key the Worker asks for from Rust, kept in one place by convention.
//! `scripts/check` reads every `key!("...")` in this crate and fails if one is missing from
//! `locales/en.toml`.

use plumblog_i18n::{Key, key};

/// Plain-text body of a 500 page.
pub const PAGE_FAILED: Key = key!("error.page_failed");

/// Site title until the owner sets one.
pub const SITE_TITLE_FALLBACK: Key = key!("site.title_fallback");

/// Plain-text body of a 404 where no theme page applies.
pub const PAGE_NOT_FOUND: Key = key!("error.page_not_found");

/// Heading of the post list in llms.txt.
pub const LLMS_POSTS: Key = key!("llms.posts");

/// Admin window title and installed app name; `{site}` is the site title.
pub const ADMIN_TITLE: Key = key!("admin.title");

/// Shown when the browser runs no JavaScript.
pub const ADMIN_NOSCRIPT: Key = key!("admin.noscript");

/// Body of a 404 for a missing admin file (a script, stylesheet or chunk).
pub const ADMIN_NOT_FOUND: Key = key!("admin.file_not_found");
