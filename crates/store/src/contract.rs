//! The storage contract: cases every [`Store`] must pass, run unchanged against
//! `MemoryStore` (cargo test) and D1 (the Worker). Each case starts from an empty store
//! (site version 1, no rows), creates its own data, never assumes id values and uses fixed
//! times. A case returns `Err` instead of panicking: a panic in wasm aborts the Worker.

mod accounts;
mod deliveries;
mod export;
mod find;
mod kit;
mod library;
mod listing;
mod posts;
mod reader;
mod reset;
mod signin;
mod site;
mod user_writes;

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
    user_writes::user_narrow_writes,
    user_writes::change_password_rotates_sessions,
    user_writes::change_login_passkey_only,
    user_writes::user_writes_do_not_undo_each_other,
    user_writes::disable_ends_sessions,
    user_writes::last_owner_is_kept,
    user_writes::second_owner_can_be_demoted,
    user_writes::disabled_user_credentials_do_not_resolve,
    accounts::passkeys,
    accounts::passkey_sign_count,
    accounts::delete_own_passkey,
    accounts::tokens,
    accounts::token_scopes_round_trip,
    accounts::invites,
    accounts::accept_invite_conflict_is_atomic,
    signin::login_failures_count_and_restart,
    signin::login_failures_multi_key_read,
    signin::login_failures_stop_at_the_cap,
    signin::login_failures_count_capped,
    signin::login_failures_sweep_is_bounded,
    signin::challenges_are_single_use,
    signin::challenges_expire,
    signin::challenges_cascade_on_user_delete,
    signin::setup_refuses_bad_codes,
    signin::setup_creates_first_owner_once,
    signin::site_id_is_set_once,
    signin::owner_reset_is_single_use,
    reset::proof_is_for_its_own_user,
    reset::stale_proof_creates_nothing,
    reset::owner_reset_redeem_replaces_credentials,
    reset::owner_reset_redeem_revokes_all_tokens,
    reset::owner_reset_needs_an_active_owner,
    signin::session_renewal,
    library::media_round_trip,
    library::media_paging,
    library::media_sha256_per_uploader,
    library::media_variants_swap,
    library::media_usage_tracks_saves,
    library::media_in_use_cannot_be_deleted,
    library::public_media_follows_live_posts,
    library::taxonomy_paging,
    library::tags_round_trip,
    library::tag_links_and_versions,
    library::tag_delete_stales_editors,
    library::series_round_trip,
    library::series_links_and_versions,
    library::series_delete_stales_editors,
    posts::post_insert_and_update,
    posts::post_save_errors,
    posts::post_save_is_atomic,
    posts::post_save_versions,
    posts::post_version_rules,
    posts::post_version_conflict,
    posts::post_version_status_changes,
    posts::trash_restore_delete,
    posts::post_delete_cascades,
    posts::publish_due,
    posts::publish_due_limit,
    posts::revisions,
    posts::translations,
    posts::post_client_id_replays,
    posts::post_client_id_concurrent,
    posts::post_stale_equal_save,
    listing::post_paging_published_desc,
    listing::post_paging_updated_desc,
    listing::post_paging_series_asc,
    listing::post_filters,
    listing::post_counts_by_status,
    listing::probe_row_past_max_page,
    listing::limits_cap_at_max_page,
    reader::reader_posts_published_only,
    reader::reader_posts_offset_and_count,
    reader::reader_tags_and_tag_filter,
    reader::reader_years_at_the_edges,
    reader::reader_translations_published_only,
    reader::reader_neighbours_by_date,
    reader::reader_feed_and_sitemap,
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
    site::comment_moderation,
    site::comment_submit_and_latest,
    export::comments_by_id_pages_every_status,
    export::users_by_id_pages,
    export::export_pages_past_max_page,
    site::reader_metadata_versions,
    site::settings_round_trip,
    site::options_round_trip,
    site::plugins_round_trip,
    site::webhooks_round_trip,
    deliveries::deliveries_round_trip,
    deliveries::deliveries_claim_and_lease,
    deliveries::deliveries_finish,
    deliveries::deliveries_cascade_and_prune,
    site::log_query_and_cursor,
    site::log_filters,
    site::log_filters_combined_and_paged,
    site::log_prune,
    site::log_detail_redacted,
    site::log_retention_job,
    site::log_retention_job_record_failure,
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
