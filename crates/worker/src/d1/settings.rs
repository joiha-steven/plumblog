use plumblog_model::Result;
use plumblog_store::{OptionOwner, PluginState, SettingsStore};

use super::{D1Store, Row, bump, bump_if, flag, int, opt_text, text};

fn pair(r: &mut Row) -> Result<(String, String)> {
    Ok((r.text()?, r.text()?))
}

impl SettingsStore for D1Store {
    // 1 query (every page).
    async fn all_settings(&self) -> Result<Vec<(String, String)>> {
        let s = self.stmt(
            "all_settings",
            "SELECT key, value_json FROM settings ORDER BY key",
            &[],
        )?;
        self.rows(s).await?.iter_mut().map(pair).collect()
    }

    // 1 query.
    async fn settings_updated_at(&self) -> Result<Option<i64>> {
        let s = self.stmt(
            "settings_updated_at",
            "SELECT MAX(updated_at) FROM settings",
            &[],
        )?;
        match self.row(s).await? {
            Some(mut r) => r.opt_int(),
            None => Ok(None),
        }
    }

    // 1 query.
    async fn get_setting(&self, key: &str) -> Result<Option<String>> {
        let s = self.stmt(
            "get_setting",
            "SELECT value_json FROM settings WHERE key = ?",
            &[text(key)],
        )?;
        self.row(s).await?.map(|mut r| r.text()).transpose()
    }

    async fn put_setting(&self, key: &str, value_json: &str, now: i64) -> Result<()> {
        let up = self.stmt(
            "put_setting",
            "INSERT INTO settings (key, value_json, updated_at) VALUES (?, ?, ?) \
             ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json, \
             updated_at = excluded.updated_at",
            &[text(key), text(value_json), int(now)?],
        )?;
        self.batch("put_setting", vec![up, bump(self)?])
            .await
            .map(drop)
    }

    async fn delete_setting(&self, key: &str) -> Result<()> {
        let bump = bump_if(
            self,
            "EXISTS (SELECT 1 FROM settings WHERE key = ?)",
            &[text(key)],
        )?;
        let del = self.stmt(
            "delete_setting",
            "DELETE FROM settings WHERE key = ?",
            &[text(key)],
        )?;
        self.batch("delete_setting", vec![bump, del])
            .await
            .map(drop)
    }

    // 1 query (render: options of the active theme or a plugin).
    async fn options(&self, owner: OptionOwner, owner_id: &str) -> Result<Vec<(String, String)>> {
        let s = self.stmt(
            "options",
            "SELECT key, value_json FROM extension_options WHERE owner_kind = ? AND owner_id = ? \
             ORDER BY key",
            &[text(owner.as_str()), text(owner_id)],
        )?;
        self.rows(s).await?.iter_mut().map(pair).collect()
    }

    async fn put_option(
        &self,
        owner: OptionOwner,
        owner_id: &str,
        key: &str,
        value_json: &str,
        now: i64,
    ) -> Result<()> {
        let up = self.stmt(
            "put_option",
            "INSERT INTO extension_options (owner_kind, owner_id, key, value_json, updated_at) \
             VALUES (?, ?, ?, ?, ?) ON CONFLICT (owner_kind, owner_id, key) DO UPDATE SET \
             value_json = excluded.value_json, updated_at = excluded.updated_at",
            &[
                text(owner.as_str()),
                text(owner_id),
                text(key),
                text(value_json),
                int(now)?,
            ],
        )?;
        self.batch("put_option", vec![up, bump(self)?])
            .await
            .map(drop)
    }

    async fn delete_options(&self, owner: OptionOwner, owner_id: &str) -> Result<u64> {
        let bump = bump_if(
            self,
            "EXISTS (SELECT 1 FROM extension_options WHERE owner_kind = ? AND owner_id = ?)",
            &[text(owner.as_str()), text(owner_id)],
        )?;
        let del = self.stmt(
            "delete_options",
            "DELETE FROM extension_options WHERE owner_kind = ? AND owner_id = ?",
            &[text(owner.as_str()), text(owner_id)],
        )?;
        let results = self.batch("delete_options", vec![bump, del]).await?;
        Self::changes("delete_options", &results[1])
    }

    // 1 query.
    async fn list_plugins(&self) -> Result<Vec<PluginState>> {
        let s = self.stmt(
            "list_plugins",
            "SELECT id, enabled, disabled_reason, updated_at FROM plugins ORDER BY id",
            &[],
        )?;
        self.rows(s)
            .await?
            .iter_mut()
            .map(|r| {
                Ok(PluginState {
                    id: r.text()?,
                    enabled: r.flag()?,
                    disabled_reason: r.opt_text()?,
                    updated_at: r.int()?,
                })
            })
            .collect()
    }

    async fn put_plugin(&self, plugin: PluginState) -> Result<()> {
        let up = self.stmt(
            "put_plugin",
            "INSERT INTO plugins (id, enabled, disabled_reason, updated_at) VALUES (?, ?, ?, ?) \
             ON CONFLICT (id) DO UPDATE SET enabled = excluded.enabled, \
             disabled_reason = excluded.disabled_reason, updated_at = excluded.updated_at",
            &[
                text(&plugin.id),
                flag(plugin.enabled),
                opt_text(plugin.disabled_reason.as_deref()),
                int(plugin.updated_at)?,
            ],
        )?;
        self.batch("put_plugin", vec![up, bump(self)?])
            .await
            .map(drop)
    }
}
