//! What reader pages read from the store (D1 in the Worker, the in-memory store in host
//! tests): the content version, theme and plugin options, plugin state, and search. Posts
//! still come from `source` until the storage issue moves them.

use super::search::SearchSource;
use super::{err, routes};
use crate::api::body::json_string;
use plumblog_markdown::render_comment;
use plumblog_render::{CommentItem, Comments};
use plumblog_store::{
    CommentStatus, CommentStore, ORIGIN_KEY, OptionOwner, PostStatus, PostStore, PostSummary,
    SettingsStore, SiteStore, TURNSTILE_SITE_KEY, comment,
};

/// Approved comments a post page lists (the newest; paging comes later). Bounds the page's
/// size and the read.
pub const COMMENTS_SHOWN: u32 = 100;

/// Store reads that shape every page.
// Workers are single-threaded, so futures need not be `Send` (see auth::Crypto).
#[allow(async_fn_in_trait)]
pub trait ExtSource {
    /// Bumped by every write readers can see, option changes included; part of every cache
    /// key and `ETag`.
    async fn content_version(&self) -> Result<u64, String>;
    /// `(key, value_json)` of the theme's stored options.
    async fn theme_options(&self, theme: &str) -> Result<Vec<(String, String)>, String>;
    /// Enabled plugins with their stored options, in id order.
    async fn enabled_plugins(&self) -> Result<Vec<(String, Vec<(String, String)>)>, String>;
    /// The comment section of a post; `None` when the site has comments off. The newest
    /// [`COMMENTS_SHOWN`] approved comments, oldest first, with the HTML stored at submit.
    async fn comments(&self, lang: &str, slug: &str) -> Result<Option<Comments>, String>;
}

/// Any store, for both page data and search.
pub struct Db<'a, S>(pub &'a S);

impl<S: SettingsStore> Db<'_, S> {
    /// The Turnstile site key when it and the site origin are both set, else empty (no widget).
    async fn turnstile_key(&self) -> Result<String, String> {
        let get = |k: &'static str| async move {
            Ok::<_, String>(
                self.0
                    .get_setting(k)
                    .await
                    .map_err(err)?
                    .and_then(|v| json_string(&v))
                    .filter(|v| !v.is_empty()),
            )
        };
        let key = get(TURNSTILE_SITE_KEY).await?;
        Ok(match (key, get(ORIGIN_KEY).await?) {
            (Some(key), Some(_)) => key,
            _ => String::new(),
        })
    }
}

impl<S: SettingsStore + SiteStore + PostStore + CommentStore> ExtSource for Db<'_, S> {
    // 1 query.
    async fn content_version(&self) -> Result<u64, String> {
        let v = self.0.content_version().await.map_err(err)?;
        u64::try_from(v).map_err(|_| format!("negative content version {v}"))
    }

    // 1 query.
    async fn theme_options(&self, theme: &str) -> Result<Vec<(String, String)>, String> {
        self.0.options(OptionOwner::Theme, theme).await.map_err(err)
    }

    // 1 query, plus 1 per enabled plugin.
    async fn enabled_plugins(&self) -> Result<Vec<(String, Vec<(String, String)>)>, String> {
        let mut out = Vec::new();
        for p in self.0.list_plugins().await.map_err(err)? {
            if p.enabled {
                let options = self
                    .0
                    .options(OptionOwner::Plugin, &p.id)
                    .await
                    .map_err(err)?;
                out.push((p.id, options));
            }
        }
        Ok(out)
    }

    // 1 query when the site has comments off; else 2 more (the post, its approved comments).
    async fn comments(&self, lang: &str, slug: &str) -> Result<Option<Comments>, String> {
        if !comment::site_enabled(self.0).await.map_err(err)? {
            return Ok(None);
        }
        let (open, rows) = match self.0.find_post(lang, slug).await.map_err(err)? {
            Some(p) if p.status == PostStatus::Published => (
                comment::is_open(true, &p),
                self.0
                    .latest_comments(p.id, CommentStatus::Approved, COMMENTS_SHOWN)
                    .await
                    .map_err(err)?,
            ),
            _ => (false, (Vec::new(), false)),
        };
        let (rows, more) = rows;
        // Settings only, never the Worker secret: settings writes bump the content version, so
        // cached pages follow them. A page with a widget but no secret says "unavailable" on
        // send. Read only for a page with an open form; a read error fails the render, so it
        // is never cached.
        let turnstile_key = if open {
            self.turnstile_key().await?
        } else {
            String::new()
        };
        let items = rows
            .into_iter()
            .map(|c| CommentItem {
                id: c.id.0,
                // Rows from before 0004_comment_html.sql have no stored HTML.
                html: c.body_html.unwrap_or_else(|| render_comment(&c.body).html),
                author: c.author_name,
                owner: false,
                // The store keeps milliseconds; dates are formatted from seconds.
                created_at: c.created_at.div_euclid(1000),
            })
            .collect();
        Ok(Some(Comments {
            open,
            items,
            action_url: routes::comments(slug),
            turnstile_key,
            max_length: comment::BODY_MAX,
            name_max_length: comment::NAME_MAX,
            max_links: comment::LINKS_MAX,
            more,
        }))
    }
}

impl<S: PostStore> SearchSource for Db<'_, S> {
    async fn search(
        &self,
        lang: &str,
        query: &str,
        limit: u32,
    ) -> Result<Vec<PostSummary>, String> {
        self.0.search(lang, query, limit).await.map_err(err)
    }
}
