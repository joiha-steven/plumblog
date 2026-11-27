//! Cloudflare REST API access for `blog setup` and `blog deploy` (issue #7). No wrangler.
//!
//! - [`endpoint`]: the table of every call, its permission and doc page.
//! - [`client`]: HTTP, the Cloudflare envelope, retries, errors.
//! - [`token`]: the API token, `.env`, verifying it and choosing the account.

pub mod client;
pub mod endpoint;
pub mod token;

pub use client::{ApiError, ApiErrorKind, Call, Client, CloudflareMessage};
pub use endpoint::{Endpoint, Permission};
pub use token::{Account, ApiToken, Credentials, TokenError, TokenKind, Verified};
