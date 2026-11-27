use plumblog_model::Result;
use plumblog_store::{Redirect, RedirectStore};

use super::{D1Store, Stmt, int, invalid, text};

/// The statements of `put_redirect`, for its own batch and for `save_post`'s.
pub(super) fn put_redirect_stmts(
    store: &D1Store,
    from: &str,
    to: &str,
    now: i64,
) -> Result<Vec<Stmt>> {
    if from == to {
        return Err(invalid(format!("redirect {from:?} points to itself")));
    }
    Ok(vec![
        // No loops: `to` is a live target now, so nothing may redirect away from it.
        store.stmt(
            "put_redirect_unloop",
            "DELETE FROM redirects WHERE from_path = ?",
            &[text(to)],
        )?,
        // No chains: whatever pointed at `from` now points straight at `to`.
        store.stmt(
            "put_redirect_unchain",
            "UPDATE redirects SET to_path = ? WHERE to_path = ?",
            &[text(to), text(from)],
        )?,
        store.stmt(
            "put_redirect",
            "INSERT INTO redirects (from_path, to_path, created_at) VALUES (?, ?, ?) \
             ON CONFLICT (from_path) DO UPDATE SET to_path = excluded.to_path, \
             created_at = excluded.created_at",
            &[text(from), text(to), int(now)?],
        )?,
    ])
}

impl RedirectStore for D1Store {
    // 1 query (before every reader 404).
    async fn find_redirect(&self, path: &str) -> Result<Option<String>> {
        let s = self.stmt(
            "find_redirect",
            "SELECT to_path FROM redirects WHERE from_path = ?",
            &[text(path)],
        )?;
        self.row(s).await?.map(|mut r| r.text()).transpose()
    }

    async fn put_redirect(&self, from: &str, to: &str, now: i64) -> Result<()> {
        let stmts = put_redirect_stmts(self, from, to, now)?;
        self.batch("put_redirect", stmts).await.map(drop)
    }

    async fn delete_redirect(&self, from: &str) -> Result<()> {
        let s = self.stmt(
            "delete_redirect",
            "DELETE FROM redirects WHERE from_path = ?",
            &[text(from)],
        )?;
        self.exec(s).await.map(drop)
    }

    // 1 query.
    async fn list_redirects(&self) -> Result<Vec<Redirect>> {
        let s = self.stmt(
            "list_redirects",
            "SELECT from_path, to_path, created_at FROM redirects ORDER BY from_path",
            &[],
        )?;
        self.rows(s)
            .await?
            .iter_mut()
            .map(|r| {
                Ok(Redirect {
                    from_path: r.text()?,
                    to_path: r.text()?,
                    created_at: r.int()?,
                })
            })
            .collect()
    }
}
