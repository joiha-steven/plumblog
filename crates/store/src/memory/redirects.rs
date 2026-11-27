use plumblog_model::Result;

use super::{MemoryStore, State, invalid};
use crate::{Redirect, RedirectStore};

impl State {
    /// Shared with `save_post`, which moves a published post's old path.
    pub(super) fn put_redirect(&mut self, from: &str, to: &str, now: i64) -> Result<()> {
        if from == to {
            return Err(invalid(format!("redirect {from:?} points to itself")));
        }
        // No loops: `to` is a live target now, so nothing may redirect away from it.
        self.redirects.remove(to);
        // No chains: whatever pointed at `from` now points straight at `to`.
        for r in self.redirects.values_mut().filter(|r| r.to_path == from) {
            to.clone_into(&mut r.to_path);
        }
        self.redirects.insert(
            from.to_owned(),
            Redirect {
                from_path: from.to_owned(),
                to_path: to.to_owned(),
                created_at: now,
            },
        );
        Ok(())
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "in-memory stand-in for async D1 storage"
)]
impl RedirectStore for MemoryStore {
    async fn find_redirect(&self, path: &str) -> Result<Option<String>> {
        Ok(self.read(|s| s.redirects.get(path).map(|r| r.to_path.clone())))
    }

    async fn put_redirect(&self, from: &str, to: &str, now: i64) -> Result<()> {
        self.write(|s| s.put_redirect(from, to, now))
    }

    async fn delete_redirect(&self, from: &str) -> Result<()> {
        self.write(|s| {
            s.redirects.remove(from);
            Ok(())
        })
    }

    async fn list_redirects(&self) -> Result<Vec<Redirect>> {
        Ok(self.read(|s| s.redirects.values().cloned().collect()))
    }
}
