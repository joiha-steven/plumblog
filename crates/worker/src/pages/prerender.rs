//! The cron's R2 page work (docs/ARCHITECTURE.md, R2 page store). After a deploy or a content
//! change, the first reader of each page paid a wasm render (40-130 ms) where an R2 copy costs
//! about 2 ms, and retired copies stayed in R2 forever. Each cron run, last, so a CPU cut-off
//! only loses this work:
//! - pre-render: home, the feed, the sitemap and the newest posts go to R2 for the running
//!   version id and content version, by the same render and rules as a reader miss;
//! - cleanup: delete groups of a content version below the current one (never readable
//!   again: `content_version` only grows) and every version id but the running one and the
//!   [`KEEP_OLD_IDS`] most recently first seen.
//!
//! When both finish, `pages/<id>/.done-v<cv><tag>` is written, so a steady run costs one D1
//! read and one R2 head. Everything is bounded by what is left of the run's subrequest budget
//! (`crate::webhooks::SUBREQUESTS`); unfinished work continues next run.
//!
//! Subrequests, counted as the rest of the cron counts them (every D1, R2 and fetch call is
//! one): steady state 2 (`content_version` and the done head; plus the theme fetch on a fresh
//! isolate with a pushed theme). Worst case: never more than the earlier jobs left (47 on a
//! normal run, whose publish and delivery checks take 3): [`FIXED`] 3 + [`LOG_RESERVE`] 2 +
//! [`DONE_PUT`] 1 + marker, post list and handled list 4 + the pages (6 + 3 x 11 + 6 + 7 = 52 with comments
//! open and no plugin; [`page_cost`]) + cleanup (at most [`CLEANUP_LISTS`] 16 lists and one
//! delete) = 79 for everything at once, so after a change a full pre-render can take two
//! runs, resuming by heads (a page already stored costs one head).

use super::cache;
use super::db::{Db, ExtSource};
use super::routes::{self, FeedFormat, Route};
use super::source::ReaderSource;
use crate::d1::D1Store;
use plumblog_model::ErrorCode;
use plumblog_syslog::{EventKind, Scope};
use worker::{Bucket, Env, console_error, console_log};

/// Subjects of the two parts in logs.
pub const PRERENDER_JOB: &str = "pages.prerender";
pub const CLEANUP_JOB: &str = "pages.cleanup";

/// Most pages one pre-render stores: home, [`PRERENDER_POSTS`] posts, the feed, the sitemap.
pub const PRERENDER_MAX: usize = 3 + PRERENDER_POSTS;
/// The newest published posts pre-rendered (in the default language: the ones home lists).
const PRERENDER_POSTS: usize = 3;
/// Version ids kept besides the running one, newest first seen first: room for rollbacks
/// and for versions still serving during a gradual deploy.
pub const KEEP_OLD_IDS: usize = 5;
/// Most R2 list calls one cleanup makes; each returns at most 1,000 keys.
pub const CLEANUP_LISTS: u32 = 16;
/// Most keys one cleanup deletes: one R2 `delete` call takes up to 1,000.
pub const CLEANUP_DELETES: usize = 1000;
/// Most version ids whose marker one cleanup reads (one list each, the running id first).
const INSPECT_MAX: usize = 12;

/// Every run: `content_version`, the theme fetch (`theme_runtime::prepare`), the done head.
const FIXED: u32 = 3;
/// One system log write per part, kept back so a failure is always logged.
const LOG_RESERVE: u32 = 2;
/// The done marker's put, kept back so finished work is always marked.
const DONE_PUT: u32 = 1;
/// Before the pages: the marker put, settings plus feed reads for the newest posts, and the
/// list of pages already handled.
const LIST_COST: u32 = 4;
/// D1 reads of every page (settings, theme options) and R2 calls (head, put).
const PAGE_BASE: u32 = 4;
/// D1 reads by route (`reader_db.rs`, `db.rs`): home 2 (count, rows); post 3 (post,
/// translations, neighbours) plus comments up to 4 (site switch, comments, Turnstile key 2);
/// feed 2 (rows, settings date); sitemap 3 (posts, tags, years).
const HOME_READS: u32 = 2;
const POST_READS: u32 = 7;
const FEED_READS: u32 = 2;
const SITEMAP_READS: u32 = 3;

/// Subrequests left in this run.
struct Budget(u32);

impl Budget {
    fn take(&mut self, n: u32) -> bool {
        let ok = self.0 >= n;
        if ok {
            self.0 -= n;
        }
        ok
    }
}

/// `pages/<id>/.done-v<cv><tag>`: this version's pre-render and cleanup are both done.
fn done_key(id: &str, version: u64, tag: &str) -> String {
    format!("pages/{id}/.done-v{version}{tag}")
}

/// `pages/<id>/.skip-v<cv><tag>`: under it, an empty key per path this version rendered but
/// could not store (not cacheable, too big, a theme failure, a redirect), so later runs do
/// not render it again and the done marker is not held back by it. Readers still render it.
fn handled_prefix(id: &str, version: u64, tag: &str) -> String {
    format!("pages/{id}/.skip-v{version}{tag}")
}

/// The content version of a key's rest after `pages/<id>/`: a group (`v12-t<hex>/`), a
/// handled group (`.skip-v12/`) or a done marker (`.done-v12`). Digits end at the tag or the
/// `/`, so `v12` and `v120` differ.
fn rest_version(rest: &str) -> Option<u64> {
    let v = rest
        .strip_prefix('v')
        .or_else(|| rest.strip_prefix(".done-v"))
        .or_else(|| rest.strip_prefix(".skip-v"))?;
    let end = v.find(|c: char| !c.is_ascii_digit()).unwrap_or(v.len());
    v[..end].parse().ok()
}

/// Ids to delete among `ids` (id, first-seen ms): all but `current` and the [`KEEP_OLD_IDS`]
/// newest marked; an id without a marker counts as oldest. `ids` may be only part of the
/// bucket's ids: an id older than [`KEEP_OLD_IDS`] others seen is outside the newest overall.
fn retired(current: &str, ids: &[(String, Option<u64>)]) -> Vec<String> {
    // A counting pass instead of a sort: `ids` is at most INSPECT_MAX long, and each sort
    // instance costs the wasm about 4 KB gzip. Ties go to the earlier id, as a stable sort would.
    let newer = |i: usize, t: u64| {
        ids.iter()
            .enumerate()
            .filter(|&(j, (id, s))| id != current && s.is_some_and(|s| s > t || (s == t && j < i)))
            .count()
    };
    ids.iter()
        .enumerate()
        .filter(|(_, (id, _))| id != current)
        .filter(|&(i, (_, seen))| seen.is_none_or(|t| newer(i, t) >= KEEP_OLD_IDS))
        .map(|(_, (id, _))| id.clone())
        .collect()
}

/// D1 reads a page at `path` may make, by its route, plus the theme's plugin reads.
fn page_cost(path: &str, plugin_reads: u32) -> u32 {
    let route = match routes::parse(path) {
        Route::Home { .. } => HOME_READS,
        Route::Feed { .. } => FEED_READS,
        Route::Sitemap => SITEMAP_READS,
        _ => POST_READS,
    };
    PAGE_BASE + route + plugin_reads
}

/// One part's outcome for the logs.
#[derive(Default)]
struct Report {
    /// Not finished within the budget or bounds (or failed): the next run continues.
    unfinished: bool,
    failed: Vec<String>,
    error: Option<String>,
}

impl Report {
    // `&dyn`: one copy in the wasm instead of one per error type.
    fn fail(&mut self, what: &str, e: &dyn std::fmt::Display) {
        self.unfinished = true;
        self.failed.push(what.to_owned());
        if self.error.is_none() {
            self.error = Some(e.to_string());
        }
    }
}

/// Runs both parts with `budget` subrequests and returns how many it counted on (test builds
/// check that against the real calls, `crate::subrequests`). Never fails the cron: errors go
/// to the Worker log and the system log ([`ErrorCode::PagePrerenderFailed`],
/// [`ErrorCode::PageCleanupFailed`]).
pub async fn cron(env: &Env, store: &D1Store, scope: &Scope, budget: u32) -> u32 {
    let prefix = scope.log_prefix();
    let Some(id) = crate::version_id(env) else {
        return 0;
    };
    let mut b = Budget(budget);
    if !b.take(FIXED + LOG_RESERVE + DONE_PUT) {
        console_log!("{prefix} job.run {PRERENDER_JOB} skipped: {budget} subrequests left");
        return 0;
    }
    crate::theme_runtime::prepare(env).await;
    let mut pre = Report::default();
    let mut clean = Report::default();
    run(env, store, &prefix, &id, &mut b, &mut pre, &mut clean).await;
    log(store, scope, PRERENDER_JOB, &pre).await;
    log(store, scope, CLEANUP_JOB, &clean).await;
    budget - b.0
}

/// Cleanup, then pre-render, then the done marker once both finished. Cleanup first: it is
/// cheap CPU, so a pre-render that always meets the CPU limit cannot starve it.
async fn run(
    env: &Env,
    store: &D1Store,
    prefix: &str,
    id: &str,
    b: &mut Budget,
    pre: &mut Report,
    clean: &mut Report,
) {
    let version = match Db(store).content_version().await {
        Ok(v) => v,
        Err(e) => return pre.fail("content_version", &e),
    };
    let bucket = match env.bucket(&cache::bucket_name(env)) {
        Ok(bucket) => bucket,
        Err(e) => return pre.fail("bucket", &e),
    };
    let done = done_key(id, version, crate::theme_runtime::cache_tag());
    crate::subrequests::count();
    match bucket.head(done.clone()).await {
        Ok(Some(_)) => return,
        Ok(None) => {}
        Err(e) => return pre.fail(&done, &e),
    }
    let deleted = cleanup(env, &bucket, id, version, b, clean).await;
    console_log!(
        "{prefix} job.run {CLEANUP_JOB} deleted={deleted} failed={} finished={}",
        clean.failed.len(),
        !clean.unfinished
    );
    let stored = prerender(env, store, &bucket, id, version, b, pre).await;
    console_log!(
        "{prefix} job.run {PRERENDER_JOB} stored={stored} failed={} finished={}",
        pre.failed.len(),
        !pre.unfinished
    );
    if !pre.unfinished && !clean.unfinished {
        crate::subrequests::count();
        if let Err(e) = bucket.put(done.clone(), Vec::<u8>::new()).execute().await {
            clean.fail(&done, &e);
        }
    }
}

/// Stores the list's missing pages; returns how many it stored.
async fn prerender(
    env: &Env,
    store: &D1Store,
    bucket: &Bucket,
    id: &str,
    version: u64,
    b: &mut Budget,
    report: &mut Report,
) -> usize {
    if !b.take(LIST_COST) {
        report.unfinished = true;
        return 0;
    }
    if let Err(e) = cache::mark_seen(bucket, id).await {
        report.fail(&cache::seen_key(id), &e);
    }
    let db = Db(store);
    let posts = match db.settings().await {
        Ok(s) => match db.feed(&s.lang, PRERENDER_POSTS).await {
            Ok(posts) => posts
                .iter()
                .map(|p| routes::post_in(&p.lang, &s.lang, &p.slug))
                .collect(),
            Err(e) => {
                report.fail("newest posts", &e);
                Vec::new()
            }
        },
        Err(e) => {
            report.fail("settings", &e);
            Vec::new()
        }
    };
    // Shared pages first: posts that keep failing to store can then never starve them.
    let mut paths = vec![
        "/".to_owned(),
        routes::feed(None, FeedFormat::Atom),
        routes::SITEMAP.to_owned(),
    ];
    paths.extend(posts);
    paths.truncate(PRERENDER_MAX);
    let plugins = crate::theme_runtime::current().map_or(0, |t| t.plugin_names().len());
    let plugin_reads = match u32::try_from(plugins) {
        Ok(0) => 0,
        Ok(n) => n.saturating_add(1),
        Err(_) => u32::MAX,
    };
    let tag = crate::theme_runtime::cache_tag();
    let skip = handled_prefix(id, version, tag);
    crate::subrequests::count();
    let handled: Vec<String> = match bucket.list().prefix(format!("{skip}/")).execute().await {
        Ok(o) => o
            .objects()
            .iter()
            .filter_map(|o| o.key().strip_prefix(&skip).map(str::to_owned))
            .collect(),
        Err(e) => {
            // Unfinished, so no done marker; the pages are only rendered once more.
            report.fail(&skip, &e);
            Vec::new()
        }
    };
    let mut stored = 0;
    for path in paths.iter().filter(|p| !handled.contains(p)) {
        // The head first, alone: a page a reader or an earlier run stored costs only that.
        if !b.take(1) {
            report.unfinished = true;
            break;
        }
        let key = cache::store_key(id, version, tag, path);
        crate::subrequests::count();
        match bucket.head(key.clone()).await {
            Ok(Some(_)) => continue,
            Ok(None) => {}
            Err(e) => {
                report.fail(path, &e);
                continue;
            }
        }
        if !b.take(page_cost(path, plugin_reads) - 1) {
            report.unfinished = true;
            break;
        }
        let mut page = match super::render_stored(env, store, id, version, path).await {
            Ok(Some(page)) => page,
            // Not storable for this version: marked handled, in the put this page's cost
            // already counts.
            Ok(None) => {
                let key = format!("{skip}{path}");
                crate::subrequests::count();
                if let Err(e) = bucket.put(key, Vec::<u8>::new()).execute().await {
                    report.fail(path, &e);
                }
                continue;
            }
            Err(e) => {
                report.fail(path, &e);
                continue;
            }
        };
        let body = std::mem::take(&mut page.body);
        match cache::put_r2(env, &page, body).await {
            Ok(()) => stored += 1,
            Err(e) => report.fail(path, &e),
        }
    }
    stored
}

/// List calls left to cleanup: [`CLEANUP_LISTS`], each also taken from the run's budget.
struct Lists(u32);

impl Lists {
    fn take(&mut self, b: &mut Budget) -> bool {
        let ok = self.0 > 0 && b.take(1);
        self.0 = self.0.saturating_sub(1);
        ok
    }
}

/// What cleanup deletes: every key under `drain`'s prefixes, and `keys`.
#[derive(Default)]
struct Plan {
    drain: Vec<String>,
    keys: Vec<String>,
}

/// Deletes retired keys within the bounds; returns how many it deleted.
async fn cleanup(
    env: &Env,
    bucket: &Bucket,
    current: &str,
    version: u64,
    b: &mut Budget,
    report: &mut Report,
) -> usize {
    let max = cleanup_limit(env);
    // The delete call is kept back first, so collected keys are always deleted.
    if !b.take(1) {
        report.unfinished = true;
        return 0;
    }
    let mut lists = Lists(CLEANUP_LISTS);
    let Some(Plan { drain, mut keys }) =
        plan(bucket, current, version, b, &mut lists, report).await
    else {
        return 0;
    };
    if keys.len() > max {
        report.unfinished = true;
        keys.truncate(max);
    }
    for p in &drain {
        let mut cursor: Option<String> = None;
        loop {
            if keys.len() >= max || !lists.take(b) {
                report.unfinished = true;
                break;
            }
            let room = u32::try_from(max - keys.len()).unwrap_or(1000).min(1000);
            let mut q = bucket.list().prefix(p.clone()).limit(room);
            if let Some(c) = cursor.take() {
                q = q.cursor(c);
            }
            crate::subrequests::count();
            let listed = match q.execute().await {
                Ok(o) => o,
                Err(e) => {
                    report.fail(p, &e);
                    break;
                }
            };
            keys.extend(listed.objects().iter().map(worker::Object::key));
            match listed.cursor().filter(|_| listed.truncated()) {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
    }
    if keys.is_empty() {
        return 0;
    }
    let n = keys.len();
    crate::subrequests::count();
    match bucket.delete_multiple(keys).await {
        Ok(()) => n,
        Err(e) => {
            report.fail("delete", &e);
            0
        }
    }
}

/// Lists the version ids and their groups, and decides what goes: dead content versions
/// under every id, and whole retired ids ([`retired`]). `None` after a list failure.
async fn plan(
    bucket: &Bucket,
    current: &str,
    version: u64,
    b: &mut Budget,
    lists: &mut Lists,
    report: &mut Report,
) -> Option<Plan> {
    if !lists.take(b) {
        report.unfinished = true;
        return None;
    }
    crate::subrequests::count();
    let top = match bucket
        .list()
        .prefix("pages/")
        .delimiter("/")
        .execute()
        .await
    {
        Ok(o) => o,
        Err(e) => {
            report.fail("pages/", &e);
            return None;
        }
    };
    report.unfinished |= top.truncated();
    let mut ids: Vec<String> = top
        .delimited_prefixes()
        .iter()
        .filter_map(|p| {
            let id = p.strip_prefix("pages/")?.trim_end_matches('/');
            (!id.is_empty()).then(|| id.to_owned())
        })
        .collect();
    // The running id first: its dead groups are the common case.
    // Moved, not sorted (a sort instance costs the wasm about 2 KB gzip); the rest keep their order.
    if let Some(i) = ids.iter().position(|id| id == current) {
        let id = ids.remove(i);
        ids.insert(0, id);
    }
    if ids.len() > INSPECT_MAX {
        report.unfinished = true;
        ids.truncate(INSPECT_MAX);
    }
    let mut plan = Plan::default();
    let mut seen: Vec<(String, Option<u64>)> = Vec::new();
    for id in &ids {
        if !lists.take(b) {
            report.unfinished = true;
            break;
        }
        let base = format!("pages/{id}/");
        crate::subrequests::count();
        let listed = match bucket
            .list()
            .prefix(base.clone())
            .delimiter("/")
            .execute()
            .await
        {
            Ok(o) => o,
            Err(e) => {
                report.fail(&base, &e);
                return None;
            }
        };
        // Groups past the first 1,000 entries wait for a later run.
        report.unfinished |= listed.truncated();
        let mut marker = None;
        for o in listed.objects() {
            let key = o.key();
            let rest = key.get(base.len()..).unwrap_or("");
            if rest == ".seen" {
                marker = Some(o.uploaded().as_millis());
            } else if rest_version(rest).is_some_and(|v| v < version) {
                plan.keys.push(key);
            }
        }
        for p in listed.delimited_prefixes() {
            let rest = p.get(base.len()..).unwrap_or("");
            if rest_version(rest).is_some_and(|v| v < version) {
                plan.drain.push(p);
            }
        }
        // `.done-v...` keys sort before `.seen`, so a cut listing may not have reached the
        // marker: such an id is not ranked (an unmarked id would be deleted) until a later
        // run, after its dead keys here are gone, lists it whole.
        if marker.is_some() || !listed.truncated() {
            seen.push((id.clone(), marker));
        }
    }
    let gone = retired(current, &seen);
    // A retired id's own dead groups go with it.
    let under_gone = |k: &String| gone.iter().any(|id| k.starts_with(&format!("pages/{id}/")));
    plan.drain.retain(|p| !under_gone(p));
    plan.keys.retain(|k| !under_gone(k));
    plan.drain
        .extend(gone.iter().map(|id| format!("pages/{id}/")));
    Some(plan)
}

/// [`CLEANUP_DELETES`], or in a test build a lower limit, so the bound is testable without
/// seeding a thousand keys.
fn cleanup_limit(env: &Env) -> usize {
    #[cfg(feature = "contract-tests")]
    if let Some(n) = env
        .var("PLUMBLOG_TEST_PAGE_DELETES")
        .ok()
        .and_then(|v| v.to_string().parse().ok())
    {
        return n;
    }
    let _ = env;
    CLEANUP_DELETES
}

/// For a failure: a Worker log line and one system log entry (kept back in the budget).
async fn log(store: &D1Store, scope: &Scope, job: &str, r: &Report) {
    let Some(error) = &r.error else { return };
    let code = if job == CLEANUP_JOB {
        ErrorCode::PageCleanupFailed
    } else {
        ErrorCode::PagePrerenderFailed
    };
    let prefix = scope.log_prefix();
    let failed = r.failed.join(",");
    console_error!(
        "{prefix} {} job={job} failed={failed} error={error}",
        code.as_str()
    );
    let now = worker::Date::now().as_millis();
    let event = scope
        .event(EventKind::ScheduledJob, now)
        .subject(job)
        .error(code)
        .field("failed", &failed)
        .field("error", error);
    plumblog_syslog::record(store, event, |m: &str| console_error!("{m}")).await;
}

#[cfg(test)]
mod tests {
    use super::{KEEP_OLD_IDS, done_key, page_cost, rest_version, retired};
    use crate::pages::routes;

    #[test]
    fn the_group_version_ends_at_the_tag_or_the_slash() {
        assert_eq!(rest_version("v12/"), Some(12));
        assert_eq!(rest_version("v120/"), Some(120));
        assert_eq!(rest_version("v12-t0123456789abcdef/"), Some(12));
        assert_eq!(rest_version(".done-v7-t0123456789abcdef"), Some(7));
        assert_eq!(rest_version(".skip-v7-t0123456789abcdef/"), Some(7));
        assert_eq!(rest_version(".seen"), None);
        assert_eq!(rest_version("x1/"), None);
        assert_eq!(done_key("id", 3, ""), "pages/id/.done-v3");
    }

    fn ids(list: &[(&str, Option<u64>)]) -> Vec<(String, Option<u64>)> {
        list.iter().map(|(i, s)| ((*i).to_owned(), *s)).collect()
    }

    #[test]
    fn the_running_id_and_the_newest_marked_are_kept() {
        let mut list: Vec<(&str, Option<u64>)> = vec![("cur", None), ("unmarked", None)];
        for (t, n) in (0u64..).zip(["a", "b", "c", "d", "e", "f", "g"]) {
            list.push((n, Some(t)));
        }
        let mut gone = retired("cur", &ids(&list));
        gone.sort();
        // Newest are g, f, e, d, c (5); b and a are older; the unmarked one counts as oldest.
        assert_eq!(KEEP_OLD_IDS, 5);
        assert_eq!(gone, ["a", "b", "unmarked"]);
        assert!(retired("cur", &ids(&[("cur", None)])).is_empty());
    }

    #[test]
    fn the_list_uses_the_reader_routes() {
        assert_eq!(routes::parse(routes::SITEMAP), routes::Route::Sitemap);
        assert_eq!(routes::feed(None, routes::FeedFormat::Atom), "/feed.xml");
        assert!(page_cost("/a-post", 0) > page_cost("/", 0));
    }
}
