//! What reader pages read from the store (D1 in the Worker, the in-memory store in host
//! tests): the content version, theme and plugin options, plugin state, and search. Posts
//! still come from `source` until the storage issue moves them.

use super::err;
use super::search::SearchSource;
use plumblog_store::{OptionOwner, PostStore, PostSummary, SettingsStore, SiteStore};

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
}

/// Any store, for both page data and search.
pub struct Db<'a, S>(pub &'a S);

impl<S: SettingsStore + SiteStore> ExtSource for Db<'_, S> {
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
