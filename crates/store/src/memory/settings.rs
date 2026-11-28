use plumblog_model::Result;

use super::{MemoryStore, count};
use crate::{OptionOwner, PluginState, SettingsStore};

#[expect(
    clippy::unused_async_trait_impl,
    reason = "in-memory stand-in for async D1 storage"
)]
impl SettingsStore for MemoryStore {
    async fn all_settings(&self) -> Result<Vec<(String, String)>> {
        Ok(self.read(|s| {
            s.settings
                .iter()
                .map(|(k, (v, _))| (k.clone(), v.clone()))
                .collect()
        }))
    }

    async fn settings_updated_at(&self) -> Result<Option<i64>> {
        Ok(self.read(|s| s.settings.values().map(|(_, at)| *at).max()))
    }

    async fn get_setting(&self, key: &str) -> Result<Option<String>> {
        Ok(self.read(|s| s.settings.get(key).map(|(v, _)| v.clone())))
    }

    async fn put_setting(&self, key: &str, value_json: &str, now: i64) -> Result<()> {
        self.write(|s| {
            s.settings
                .insert(key.to_owned(), (value_json.to_owned(), now));
            s.bump();
            Ok(())
        })
    }

    async fn delete_setting(&self, key: &str) -> Result<()> {
        self.write(|s| {
            if s.settings.remove(key).is_some() {
                s.bump();
            }
            Ok(())
        })
    }

    async fn options(&self, owner: OptionOwner, owner_id: &str) -> Result<Vec<(String, String)>> {
        Ok(self.read(|s| {
            s.options
                .iter()
                .filter(|((o, id, _), _)| *o == owner.as_str() && id == owner_id)
                .map(|((_, _, k), (v, _))| (k.clone(), v.clone()))
                .collect()
        }))
    }

    async fn put_option(
        &self,
        owner: OptionOwner,
        owner_id: &str,
        key: &str,
        value_json: &str,
        now: i64,
    ) -> Result<()> {
        self.write(|s| {
            s.options.insert(
                (owner.as_str(), owner_id.to_owned(), key.to_owned()),
                (value_json.to_owned(), now),
            );
            s.bump();
            Ok(())
        })
    }

    async fn delete_options(&self, owner: OptionOwner, owner_id: &str) -> Result<u64> {
        self.write(|s| {
            let before = s.options.len();
            s.options
                .retain(|(o, id, _), _| !(*o == owner.as_str() && id == owner_id));
            let n = before - s.options.len();
            if n > 0 {
                s.bump();
            }
            Ok(count(n))
        })
    }

    async fn list_plugins(&self) -> Result<Vec<PluginState>> {
        Ok(self.read(|s| s.plugins.values().cloned().collect()))
    }

    async fn put_plugin(&self, plugin: PluginState) -> Result<()> {
        self.write(|s| {
            s.plugins.insert(plugin.id.clone(), plugin);
            s.bump();
            Ok(())
        })
    }
}
