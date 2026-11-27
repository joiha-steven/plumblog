//! The storage contract: cases every [`Store`] must pass, run unchanged against
//! `MemoryStore` (cargo test) and D1 (the Worker). Each case starts from an empty store
//! (site version 1, no rows), creates its own data, never assumes id values and uses fixed
//! times. A case returns `Err` instead of panicking: a panic in wasm aborts the Worker.

mod accounts;
mod find;
mod kit;
mod library;
mod listing;
mod posts;
mod site;

use crate::Store;

/// Lists every case once: the names in [`CASES`] and the dispatch in [`run_case`] come from
/// the same list, so they cannot drift apart.
macro_rules! cases {
    ($($module:ident :: $case:ident),* $(,)?) => {
        /// Every case name, in run order.
        pub const CASES: &[&str] = &[$(stringify!($case)),*];

        /// Runs one case on `store`, which must be empty. `Err` says which case, what was
        /// expected and what came back; an unknown name is `Err` too.
        ///
        /// # Errors
        /// When the store breaks the contract, or `name` is not in [`CASES`].
        pub async fn run_case<S: Store>(name: &str, store: &S) -> Result<(), String> {
            let out = match name {
                $(stringify!($case) => $module::$case(store).await,)*
                _ => return Err(format!("unknown contract case {name:?}")),
            };
            out.map_err(|e| format!("{name}: {e}"))
        }
    };
}

cases! {
    site::site_starts_at_version_1,
    accounts::users_round_trip,
    accounts::user_errors,
    accounts::user_delete_effects,
    accounts::sessions,
    accounts::passkeys,
    accounts::tokens,
    accounts::invites,
    accounts::accept_invite_conflict_is_atomic,
    library::media_round_trip,
    library::media_paging,
    library::media_delete_clears_cover,
    library::tags_round_trip,
    library::tag_links_and_versions,
    library::series_round_trip,
    library::series_links_and_versions,
    posts::post_insert_and_update,
    posts::post_save_errors,
    posts::post_save_is_atomic,
    posts::post_save_versions,
    posts::trash_restore_delete,
    posts::post_delete_cascades,
    posts::publish_due,
    posts::revisions,
    posts::translations,
    listing::post_paging_published_desc,
    listing::post_paging_updated_desc,
    listing::post_paging_series_asc,
    listing::post_filters,
    listing::limits_cap_at_max_page,
    find::search_vietnamese,
    find::search_scope_and_order,
    find::search_hostile_input,
    find::redirects_round_trip,
    find::redirect_chains_and_loops,
    find::post_rename_redirects,
    find::redirect_kept_until_live,
    find::restore_removes_redirect,
    site::comments_round_trip,
    site::comment_delete_cascades,
    site::comment_versions,
    site::reader_metadata_versions,
    site::settings_round_trip,
    site::options_round_trip,
    site::plugins_round_trip,
    site::webhooks_round_trip,
    site::log_query_and_cursor,
    site::log_prune,
}

#[cfg(test)]
mod tests {
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    use super::{CASES, run_case};
    use crate::memory::MemoryStore;

    /// The memory store never awaits anything, so one poll finishes it; avoids an executor
    /// dependency just for tests.
    fn run<F: Future>(f: F) -> F::Output {
        match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("memory store future was pending"),
        }
    }

    /// Every case on a fresh store; reports all failures at once.
    #[test]
    fn memory_store_passes_the_contract() {
        let failures: Vec<String> = CASES
            .iter()
            .filter_map(|name| run(run_case(name, &MemoryStore::default())).err())
            .collect();
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }

    #[test]
    fn unknown_case_is_an_error() {
        assert!(run(run_case("no_such_case", &MemoryStore::default())).is_err());
    }

    #[test]
    fn case_names_are_unique() {
        let mut names = CASES.to_vec();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), CASES.len());
    }
}
