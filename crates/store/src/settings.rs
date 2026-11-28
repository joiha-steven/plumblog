//! Site settings, theme/plugin options and plugin state (`settings`, `extension_options`,
//! `plugins`). Values are JSON the store never parses.

use plumblog_model::{Result, stable_codes};

stable_codes! {
    /// Stored in `extension_options.owner_kind`.
    pub enum OptionOwner {
        Theme => "theme",
        Plugin => "plugin",
    }
}

/// A row of `plugins`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginState {
    pub id: String,
    pub enabled: bool,
    /// `None` unless the plugin was disabled automatically.
    pub disabled_reason: Option<String>,
    pub updated_at: i64,
}

// Workers are single-threaded, so futures need not be `Send` (see plumblog_auth::Crypto).
#[allow(async_fn_in_trait)]
pub trait SettingsStore {
    /// `(key, value_json)` ordered by key.
    async fn all_settings(&self) -> Result<Vec<(String, String)>>;
    /// Newest `updated_at` (ms) of any setting, `None` when there are none: the site's last
    /// real change time, for a feed with no posts. 1 query.
    async fn settings_updated_at(&self) -> Result<Option<i64>>;
    async fn get_setting(&self, key: &str) -> Result<Option<String>>;
    /// Upsert; bumps the content version.
    async fn put_setting(&self, key: &str, value_json: &str, now: i64) -> Result<()>;
    /// Absent key is Ok and does not bump; otherwise bumps.
    async fn delete_setting(&self, key: &str) -> Result<()>;
    /// `(key, value_json)` of one theme or plugin, ordered by key.
    async fn options(&self, owner: OptionOwner, owner_id: &str) -> Result<Vec<(String, String)>>;
    /// Upsert; bumps the content version.
    async fn put_option(
        &self,
        owner: OptionOwner,
        owner_id: &str,
        key: &str,
        value_json: &str,
        now: i64,
    ) -> Result<()>;
    /// Deletes every option of one theme or plugin; returns how many; bumps if any.
    async fn delete_options(&self, owner: OptionOwner, owner_id: &str) -> Result<u64>;
    /// Ordered by id.
    async fn list_plugins(&self) -> Result<Vec<PluginState>>;
    /// Upsert; bumps the content version.
    async fn put_plugin(&self, plugin: PluginState) -> Result<()>;
}
