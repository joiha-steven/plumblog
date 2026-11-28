//! `blog deploy` and `blog rollback` (issue #7): the REST calls wrangler's `deploy` and
//! `d1 time-travel restore` make, in the order docs/ARCHITECTURE.md (Site-specific state)
//! gives, with a deploy record in `site.local.toml` written before each step changes anything.
//!
//! Request shapes not spelled out in the API docs come from wrangler 4.160.0
//! (`wrangler-dist/cli.js`), named at each call.

use crate::bundle::{self, Bundle, Part};
use crate::cloudflare::{ApiError, ApiErrorKind, ApiToken, Call, Client, Endpoint, token};
use crate::d1::Database;
use crate::migrations;
use crate::setup::{self, Context, Failure, Run, printable, same_or_mismatch, state_failure};
use crate::state::{self, DeployRecord, RollbackRecord, SiteState, ThemeRecord, push_capped};
use crate::text::{Code, Text};
use crate::theme::store;
use plumblog_i18n::key;
use plumblog_model::RequestId;
use plumblog_syslog::{Event, EventKind};
use serde_json::{Value, json};
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

/// The Workers runtime date every upload uses; tools/wrangler.dev.toml has the same (a test
/// checks).
pub const COMPATIBILITY_DATE: &str = "2026-11-01";
/// The one cron trigger (docs/ARCHITECTURE.md Platform, tools/wrangler.dev.toml; a test
/// checks both).
pub const CRON: &str = "*/10 * * * *";
/// Sent by the Worker on `/healthz` with the running version's id (crates/worker lib.rs).
pub const VERSION_HEADER: &str = "x-plumblog-version";
/// The version metadata binding the Worker reads its id from.
pub const VERSION_BINDING: &str = "CF_VERSION_METADATA";
/// Plain-text binding naming the active pushed theme (crates/worker `theme_runtime`).
pub const THEME_BINDING: &str = "PLUMBLOG_THEME";
/// `x-plumblog-theme` of the pushed theme, for the Worker's JS cache fast path.
pub const THEME_TAG_BINDING: &str = "PLUMBLOG_THEME_TAG";
/// Test-only smoke target instead of the site's origin; loopback only, like the API base.
/// Requests go to `<base>/<host><path>`, so a test server tells the hosts apart.
pub const SMOKE_BASE_ENV: &str = "PLUMBLOG_SMOKE_BASE";
/// Where `blog deploy` builds the bundle in the repo.
pub const BUILD_DIR: &str = "target/deploy-bundle";
/// The build script, relative to the repo root.
pub const BUILD_SCRIPT: &str = "scripts/build-bundle";
/// Exports, relative to the site folder; git-ignored.
pub const BACKUP_DIR: &str = "backups";
/// The whole smoke check, retries included: a fresh workers.dev route can take a moment.
pub const SMOKE_BUDGET: Duration = Duration::from_secs(60);
const SMOKE_FIRST_WAIT: Duration = Duration::from_secs(1);
const SMOKE_MAX_WAIT: Duration = Duration::from_secs(15);
const SMOKE_CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// wrangler `versions upload` / `deploy` keep secrets with `keepSecrets: true`, sent as
/// `keep_bindings: ["secret_text", "secret_key"]` (`createWorkerUploadForm`): secret bindings
/// absent from the metadata are inherited from the previous version.
const KEEP_BINDINGS: &[&str] = &["secret_text", "secret_key"];

/// Time, so tests can run a 60 s smoke budget without waiting.
pub trait Clock {
    /// Time since an arbitrary start.
    fn elapsed(&self) -> Duration;
    fn sleep(&self, d: Duration);
}

pub struct RealClock(Instant);

impl RealClock {
    #[must_use]
    pub fn new() -> Self {
        Self(Instant::now())
    }
}

impl Default for RealClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for RealClock {
    fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }
    fn sleep(&self, d: Duration) {
        std::thread::sleep(d);
    }
}

/// What the owner asked for.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Export the database even with no pending migration.
    pub export: bool,
    /// A release bundle folder (`--bundle`).
    pub bundle: Option<PathBuf>,
    /// Build the bundle in the repo (`--build`, for contributors).
    pub build: bool,
    /// The release this binary was made for (default when neither of the above): downloaded,
    /// checked against the baked hash and cached. `None` in a dev build.
    pub release: Option<crate::release::Release>,
    /// Deploy a bundle that lacks migrations the database already has.
    pub allow_older: bool,
    /// What happens to the serving version's pushed theme.
    pub theme: ThemeCarry,
}

/// `blog deploy`'s choice for the pushed theme the site serves.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemeCarry {
    /// Upload it again with the new version (refuse if it cannot be recovered).
    #[default]
    Keep,
    /// `--theme-default`: the new version uses the embedded default theme.
    Drop,
}

/// `blog rollback`.
#[derive(Clone, Debug, Default)]
pub struct RollbackOptions {
    pub to: Option<String>,
    pub db: Option<String>,
    pub yes: bool,
    /// With `--db`: forget the recorded failed migration although `db` is not its bookmark.
    pub clear_failed_migration: bool,
    /// Go back to a version that lacks migrations the database ran, or whose migrations are
    /// unknown (not deployed by `blog` from this folder).
    pub allow_older: bool,
}

/// What deploy needs besides the API and the setup context.
pub struct Runtime<'a> {
    pub clock: &'a dyn Clock,
    /// [`SMOKE_BASE_ENV`], if set.
    pub smoke_base: Option<String>,
    /// [`SMOKE_BUDGET`] outside tests.
    pub smoke_budget: Duration,
}

/// `2026-11-28T10:15:00Z` and `20261128T101500Z` for Unix milliseconds.
#[must_use]
pub fn utc(ms: i64) -> (String, String) {
    let secs = ms.div_euclid(1000);
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (hour, min, sec) = (rem / 3600, rem % 3600 / 60, rem % 60);
    // Howard Hinnant's civil_from_days.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let doe = shifted.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (
        format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z"),
        format!("{year:04}{month:02}{day:02}T{hour:02}{min:02}{sec:02}Z"),
    )
}

fn malformed(endpoint: Endpoint, detail: &str) -> ApiError {
    ApiError {
        kind: ApiErrorKind::Malformed,
        endpoint: Some(endpoint),
        status: None,
        errors: Vec::new(),
        detail: detail.to_string(),
        tries: 1,
    }
}

pub(crate) fn str_at<'v>(v: &'v Value, key: &str) -> Option<&'v str> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// The site as `blog setup` left it.
pub(crate) struct Site {
    pub(crate) account: String,
    pub(crate) worker: String,
    pub(crate) origin: String,
    pub(crate) db_name: String,
    pub(crate) db_id: String,
    pub(crate) bucket: String,
    pub(crate) turnstile: bool,
    /// Custom domain host, if any.
    pub(crate) domain: Option<String>,
}

pub(crate) fn site(state: Option<&SiteState>, cx: &Context) -> Result<Site, Failure> {
    let not_set_up = || {
        Failure::new(
            Code::NotSetUp,
            cx.text,
            key!("cli.error.not_set_up"),
            &[("path", state::path(cx.dir).display().to_string())],
        )
    };
    let st = state.ok_or_else(not_set_up)?;
    let (Some(account), Some(worker), Some(origin), Some(d1), Some(r2)) =
        (&st.account_id, &st.worker, &st.origin, &st.d1, &st.r2)
    else {
        return Err(not_set_up());
    };
    Ok(Site {
        account: account.clone(),
        worker: worker.clone(),
        origin: origin.clone(),
        db_name: d1.name.clone(),
        db_id: d1.id.clone(),
        bucket: r2.bucket.clone(),
        turnstile: st.turnstile_site_key.is_some(),
        domain: st.domain.clone(),
    })
}

/// Loads the state, verifies the token and checks the account is the site's.
pub(crate) fn start(
    client: &mut Client,
    account_id: Option<&str>,
    cx: &Context,
) -> Result<(SiteState, Site), Failure> {
    let saved = state::load(cx.dir).map_err(|e| state_failure(&e, cx.text))?;
    let site = site(saved.as_ref(), cx)?;
    let st = saved.unwrap_or_default();
    let verified = token::verify(client, account_id.or(Some(site.account.as_str())))
        .map_err(|e| Failure::token(&e, cx.text))?;
    same_or_mismatch(
        Some(&site.account),
        Some(&verified.account.id),
        "CLOUDFLARE_ACCOUNT_ID",
        cx,
    )?;
    Ok((st, site))
}

fn restore_pending(st: &SiteState, cx: &Context) -> Result<(), Failure> {
    if let Some(f) = &st.migrations.failed {
        let restore = setup::ROLLBACK_COMMAND.replace("{bookmark}", &f.bookmark);
        return Err(Failure::new(
            Code::MigrationRestorePending,
            cx.text,
            key!("cli.error.migration_restore_pending"),
            &[
                ("name", f.name.clone()),
                ("bookmark", f.bookmark.clone()),
                ("restore", restore),
                ("path", state::path(cx.dir).display().to_string()),
            ],
        ));
    }
    Ok(())
}

/// A dev build has no release to download: it deploys only what the owner names.
pub(crate) fn check_source(opts: &Options, text: &Text) -> Result<(), Failure> {
    if opts.bundle.is_none() && !opts.build && opts.release.is_none() {
        return Err(Failure::new(
            Code::ReleaseDevBuild,
            text,
            key!("cli.error.release_dev_build"),
            &[("page", crate::release::fetch::RELEASES_PAGE.into())],
        ));
    }
    Ok(())
}

/// The bundle: `--bundle`, else built in the repo with [`BUILD_SCRIPT`] (`--build`), else
/// this binary's release bundle.
pub(crate) fn bundle(run: &mut Run, opts: &Options) -> Result<(PathBuf, Bundle), Failure> {
    let text = run.cx.text;
    let dir = if let Some(b) = &opts.bundle {
        b.clone()
    } else if let (false, Some(r)) = (opts.build, &opts.release) {
        crate::release::bundle(r, run)?
    } else {
        let script = run.cx.dir.join(BUILD_SCRIPT);
        if !script.is_file() {
            return Err(Failure::new(
                Code::BundleMissing,
                text,
                key!("cli.error.bundle_missing"),
                &[],
            ));
        }
        run.line(
            key!("cli.deploy.building"),
            &[("command", format!("{BUILD_SCRIPT} {BUILD_DIR}"))],
        );
        let status = Command::new(&script)
            .arg(BUILD_DIR)
            .current_dir(run.cx.dir)
            .status();
        let failed = match status {
            Ok(s) if s.success() => None,
            Ok(s) => Some(s.to_string()),
            Err(e) => Some(e.to_string()),
        };
        if let Some(detail) = failed {
            return Err(Failure::new(
                Code::BuildFailed,
                text,
                key!("cli.error.build_failed"),
                &[("detail", detail)],
            ));
        }
        run.cx.dir.join(BUILD_DIR)
    };
    let b = bundle::load(&dir).map_err(|e| {
        let (code, k) = if e.symlink {
            (Code::BundleSymlink, key!("cli.error.bundle_symlink"))
        } else {
            (Code::BundleInvalid, key!("cli.error.bundle_invalid"))
        };
        Failure::new(code, text, k, &[("path", e.path), ("reason", e.reason)])
    })?;
    run.line(
        key!("cli.deploy.bundle"),
        &[
            ("dir", dir.display().to_string()),
            ("modules", b.modules.len().to_string()),
            ("assets", b.assets.len().to_string()),
        ],
    );
    Ok((dir, b))
}

/// Replaces this run's deploy record (the newest one with its time, pushed by [`run`]) in
/// the file as it is now, and saves.
fn put_record(run: &mut Run, rec: &DeployRecord) -> Result<(), Failure> {
    run.update(
        |s| match s.deploys.iter_mut().rev().find(|d| d.at == rec.at) {
            Some(mine) => *mine = rec.clone(),
            None => push_capped(&mut s.deploys, rec.clone()),
        },
    )
}

/// One setup, deploy or rollback at a time per site folder: two runs would race on the
/// migrations and the state file. A lock left by a crash is named in the error, to be
/// removed by hand.
pub(crate) struct Lock(PathBuf);

pub const LOCK_FILE: &str = ".blog-deploy.lock";

impl Lock {
    pub(crate) fn take(cx: &Context) -> Result<Self, Failure> {
        let p = cx.dir.join(LOCK_FILE);
        match fs::OpenOptions::new().write(true).create_new(true).open(&p) {
            Ok(mut f) => {
                let _ = writeln!(f, "{}", std::process::id());
                Ok(Self(p))
            }
            Err(e) => Err(Failure::new(
                Code::DeployLocked,
                cx.text,
                key!("cli.error.deploy_locked"),
                &[("path", p.display().to_string()), ("reason", e.to_string())],
            )),
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// What the export is for: only the wording of its failures differs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Purpose {
    Deploy,
    Backup,
}

/// No token, ever: these go to the site and to presigned URLs.
pub(crate) fn plain_agent(local: bool, timeout: Duration, redirects: u32) -> ureq::Agent {
    let mut config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(redirects)
        .timeout_global(Some(timeout))
        .user_agent(concat!("plumblog-cli/", env!("CARGO_PKG_VERSION")))
        .https_only(!local);
    if local {
        config = config.proxy(None);
    }
    config.build().into()
}

/// Assets upload session, then only the buckets Cloudflare asks for, each as multipart of
/// base64 files under the session JWT (wrangler 4.160.0 `syncAssets`: part name and file
/// name = hash, `?base64=true`). Returns the completion JWT (the session JWT itself when
/// nothing is missing) and the number of files uploaded.
/// <https://developers.cloudflare.com/workers/static-assets/direct-upload/>
fn upload_assets(run: &mut Run, site: &Site, b: &Bundle) -> Result<ApiToken, Failure> {
    let mut manifest = serde_json::Map::new();
    for a in &b.assets {
        manifest.insert(a.path.clone(), json!({"hash": a.hash, "size": a.size}));
    }
    let call = Call::new(Endpoint::AssetsSession)
        .param("script_name", site.worker.clone())
        .json(json!({"manifest": manifest}));
    let r = run.client.call(&call).map_err(|e| run.api(&e))?;
    let jwt = |v: &Value, e: Endpoint| -> Result<ApiToken, Failure> {
        str_at(v, "jwt")
            .and_then(|j| ApiToken::new(j).ok())
            .ok_or_else(|| run.api(&malformed(e, "no jwt")))
    };
    let session = jwt(&r, Endpoint::AssetsSession)?;
    let buckets: Vec<Vec<String>> = r
        .get("buckets")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|_| run.api(&malformed(Endpoint::AssetsSession, "buckets")))?
        .unwrap_or_default();
    let missing: usize = buckets.iter().map(Vec::len).sum();
    let mut completion = None;
    for bucket in buckets.iter().filter(|b| !b.is_empty()) {
        let mut files = Vec::new();
        for hash in bucket {
            let a = b
                .assets
                .iter()
                .find(|a| &a.hash == hash)
                .ok_or_else(|| run.api(&malformed(Endpoint::AssetsSession, "unknown hash")))?;
            let bytes = fs::read(&a.file).map_err(|e| {
                Failure::new(
                    Code::BundleInvalid,
                    run.cx.text,
                    key!("cli.error.bundle_invalid"),
                    &[
                        ("path", a.file.display().to_string()),
                        ("reason", e.to_string()),
                    ],
                )
            })?;
            files.push((
                hash.clone(),
                bundle::content_type(&a.path),
                setup::base64_std(&bytes),
            ));
        }
        let parts: Vec<Part> = files
            .iter()
            .map(|(h, ct, data)| Part {
                name: h,
                filename: Some(h),
                content_type: ct,
                bytes: data.as_bytes(),
            })
            .collect();
        let (ctype, body) = bundle::multipart(&parts);
        let upload_jwt = ApiToken::new(session.expose())
            .map_err(|_| run.api(&malformed(Endpoint::AssetsSession, "jwt")))?;
        let call = Call::new(Endpoint::AssetsUpload)
            .query("base64", "true")
            .upload_jwt(upload_jwt)
            .raw(ctype, body);
        let r = run.client.call(&call).map_err(|e| run.api(&e))?;
        if let Some(j) = str_at(&r, "jwt") {
            completion = Some(j.to_string());
        }
    }
    run.line(
        key!("cli.deploy.assets"),
        &[
            ("uploaded", missing.to_string()),
            ("kept", (b.assets.len().saturating_sub(missing)).to_string()),
        ],
    );
    match completion {
        _ if missing == 0 => Ok(session),
        Some(j) => {
            ApiToken::new(&j).map_err(|_| run.api(&malformed(Endpoint::AssetsUpload, "jwt")))
        }
        None => Err(run.api(&malformed(Endpoint::AssetsUpload, "no completion jwt"))),
    }
}

/// A new version, not deployed: multipart with `metadata` and every module (part name and
/// file name = module name, types as wrangler's `moduleTypeMimeType`). Secrets are kept by
/// `keep_bindings`, never re-sent.
/// <https://developers.cloudflare.com/api/resources/workers/subresources/scripts/subresources/versions/methods/create/>
fn upload_version(
    run: &mut Run,
    site: &Site,
    b: &Bundle,
    assets_jwt: &ApiToken,
    theme: Option<&ThemeRecord>,
) -> Result<String, Failure> {
    // Every version carries the marker, so setup keeps recognising the script; a site set up
    // before markers gets one now.
    let marker = if let Some(m) = run.state.script_marker.clone() {
        m
    } else {
        let m = run.random(setup::new_marker())?;
        run.update(|s| s.script_marker = Some(m.clone()))?;
        m
    };
    let mut assets = json!({"jwt": assets_jwt.expose()});
    if let Some(h) = &b.headers {
        // wrangler sends `_headers` text as `assets.config._headers`.
        assets["config"] = json!({"_headers": h});
    }
    let mut metadata = json!({
        "main_module": b.main_module,
        "compatibility_date": COMPATIBILITY_DATE,
        "bindings": [
            {"type": "d1", "name": "DB", "id": site.db_id},
            {"type": "r2_bucket", "name": "MEDIA", "bucket_name": site.bucket},
            {"type": "assets", "name": "ASSETS"},
            {"type": "version_metadata", "name": VERSION_BINDING},
            {"type": "plain_text", "name": setup::MARKER_BINDING, "text": marker},
        ],
        "keep_bindings": KEEP_BINDINGS,
        "assets": assets,
    });
    // Secrets the Worker needs, inherited by name: with `bindings_inherit=strict` a missing
    // one fails the upload instead of shipping a version without it.
    let mut secrets = vec![setup::WEBHOOK_KEY];
    if site.turnstile {
        secrets.push(setup::TURNSTILE_SECRET);
    }
    if let Some(list) = metadata["bindings"].as_array_mut() {
        // The Worker renders this pushed theme (its files are among this version's assets).
        if let Some(t) = theme {
            list.push(json!({"type": "plain_text", "name": THEME_BINDING, "text": t.name}));
            let tag = plumblog_render::pushed::theme_tag(&t.hash);
            list.push(json!({"type": "plain_text", "name": THEME_TAG_BINDING, "text": tag}));
        }
        for name in secrets {
            list.push(json!({"type": "inherit", "name": name}));
        }
    }
    let metadata = metadata.to_string();
    let mut parts = vec![Part {
        name: "metadata",
        filename: None,
        content_type: "application/json",
        bytes: metadata.as_bytes(),
    }];
    for m in &b.modules {
        parts.push(Part {
            name: &m.name,
            filename: Some(&m.name),
            content_type: m.content_type,
            bytes: &m.bytes,
        });
    }
    let (ctype, body) = bundle::multipart(&parts);
    let hidden = ApiToken::new(assets_jwt.expose())
        .map_err(|_| run.api(&malformed(Endpoint::VersionUpload, "jwt")))?;
    // `?bindings_inherit=strict` as wrangler 4.160.0 sends it with `keep_bindings`
    // (cli.js `versions upload` and `deploy`).
    let call = Call::new(Endpoint::VersionUpload)
        .param("script_name", site.worker.clone())
        .query("bindings_inherit", "strict")
        .raw(ctype, body)
        .secret(hidden);
    let r = run.client.call(&call).map_err(|e| run.api(&e))?;
    let id = str_at(&r, "id")
        .map(str::to_string)
        .ok_or_else(|| run.api(&malformed(Endpoint::VersionUpload, "no id")))?;
    run.line(key!("cli.deploy.version"), &[("version", id.clone())]);
    Ok(id)
}

/// The version serving most traffic now: first deployment (wrangler
/// `fetchLatestDeployment`), its largest share. After setup there is always one (the
/// placeholder), so a reply without it is malformed, never "no version".
///
/// # Errors
/// The API error, or `Malformed`.
pub(crate) fn active_version(client: &Client, worker: &str) -> Result<String, ApiError> {
    let r = client
        .call(&Call::new(Endpoint::DeploymentList).param("script_name", worker.to_string()))?;
    let best = r
        .get("deployments")
        .and_then(Value::as_array)
        .and_then(|d| d.first())
        .and_then(|d| d.get("versions"))
        .and_then(Value::as_array)
        .and_then(|vs| {
            vs.iter().max_by(|a, b| {
                let p = |v: &Value| v.get("percentage").and_then(Value::as_f64).unwrap_or(0.0);
                p(a).total_cmp(&p(b))
            })
        })
        .and_then(|v| str_at(v, "version_id"))
        .map(str::to_string);
    best.ok_or_else(|| {
        malformed(
            Endpoint::DeploymentList,
            "no deployments[0].versions[].version_id",
        )
    })
}

/// `{strategy: "percentage", versions: [{version_id, percentage: 100}]}`.
/// <https://developers.cloudflare.com/api/resources/workers/subresources/scripts/subresources/deployments/methods/create/>
fn deploy_version(client: &Client, worker: &str, version: &str) -> Result<(), ApiError> {
    let call = Call::new(Endpoint::DeploymentCreate)
        .param("script_name", worker.to_string())
        .json(json!({"strategy": "percentage",
                     "versions": [{"version_id": version, "percentage": 100}]}));
    client.call(&call).map(drop)
}

/// The theme `/` must render with, read from its `x-plumblog-theme` header: the Worker
/// falls back to the default theme when it cannot use a pushed one, and `/` still answers 200.
#[derive(Clone, Copy)]
pub(crate) enum ThemeWant<'a> {
    /// Not checked (a rollback to a version whose theme is not on record).
    Any,
    /// The embedded default: no header.
    Default,
    /// The pushed theme with this full hash.
    Hash(&'a str),
}

impl<'a> ThemeWant<'a> {
    pub(crate) fn of(theme: Option<&'a ThemeRecord>) -> Self {
        theme.map_or(Self::Default, |t| Self::Hash(&t.hash))
    }

    /// `Err(wanted tag)` when `got` (the header, if any) is not what was asked for.
    fn check(self, got: Option<&str>) -> Result<(), Option<String>> {
        let want = match self {
            Self::Any => return Ok(()),
            Self::Default => None,
            Self::Hash(h) => Some(plumblog_render::pushed::theme_tag(h)),
        };
        if got == want {
            Ok(())
        } else {
            Err(want.map(str::to_owned))
        }
    }
}

/// What a failed smoke check saw: data for the record, words for the owner.
enum Seen {
    NoReply(&'static str, String),
    Status(&'static str, u16),
    Version(String),
    /// `/` rendered with another theme than wanted: `(got, want)`, `None` = the default.
    Theme(Option<String>, Option<String>),
}

impl Seen {
    fn record(&self) -> String {
        match self {
            Self::NoReply(p, _) => format!("{p} no-reply"),
            Self::Status(p, s) => format!("{p} http-{s}"),
            Self::Version(v) => format!("/healthz version {v}"),
            Self::Theme(got, _) => format!("/ theme {}", got.as_deref().unwrap_or("-")),
        }
    }

    fn words(&self, text: &Text, want: &str) -> String {
        match self {
            Self::NoReply(p, d) => text.t(
                key!("cli.deploy.seen_no_reply"),
                &[("path", (*p).to_string()), ("detail", d.clone())],
            ),
            Self::Status(p, s) => text.t(
                key!("cli.deploy.seen_status"),
                &[("path", (*p).to_string()), ("status", s.to_string())],
            ),
            Self::Version(v) => text.t(
                key!("cli.deploy.seen_version"),
                &[("got", v.clone()), ("want", want.to_string())],
            ),
            Self::Theme(None, Some(w)) => text.t(
                key!("cli.deploy.seen_theme_default"),
                &[("want", w.clone())],
            ),
            Self::Theme(Some(g), None) => {
                text.t(key!("cli.deploy.seen_theme_still"), &[("got", g.clone())])
            }
            Self::Theme(got, w) => text.t(
                key!("cli.deploy.seen_theme_other"),
                &[
                    ("got", got.clone().unwrap_or_default()),
                    ("want", w.clone().unwrap_or_default()),
                ],
            ),
        }
    }
}

/// One try. Each call's timeout is capped by `left()`, so the check never outlives its budget.
fn smoke_once(
    agent: &ureq::Agent,
    base: &str,
    version: &str,
    theme: ThemeWant,
    left: &dyn Fn() -> Duration,
) -> Result<(), Seen> {
    let get = |path: &'static str| -> Result<ureq::http::Response<ureq::Body>, Seen> {
        let time = left().min(SMOKE_CALL_TIMEOUT);
        if time.is_zero() {
            return Err(Seen::NoReply(path, "0 s left".into()));
        }
        let r = agent
            .get(&format!("{base}{path}"))
            .config()
            .timeout_global(Some(time))
            .build()
            .call()
            .map_err(|e| Seen::NoReply(path, e.to_string()))?;
        let s = r.status().as_u16();
        if s == 200 {
            Ok(r)
        } else {
            Err(Seen::Status(path, s))
        }
    };
    let health = get("/healthz")?;
    let got = health
        .headers()
        .get(VERSION_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if got != version {
        return Err(Seen::Version(if got.is_empty() {
            "-".into()
        } else {
            printable(&got)
        }));
    }
    let home = get("/")?;
    let got = home
        .headers()
        .get(plumblog_render::pushed::THEME_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(printable);
    theme
        .check(got.as_deref())
        .map_err(|want| Seen::Theme(got, want))
}

/// `GET <base>/healthz` until it answers 200 with `x-plumblog-version: <version>`, then
/// `GET <base>/` for 200 rendered with the theme `theme` names; retried with backoff. Calls
/// and waits together end within `rt.smoke_budget`.
fn smoke(
    run: &mut Run,
    t: &Targets,
    version: &str,
    theme: ThemeWant,
    rt: &Runtime,
) -> Result<(), Seen> {
    let base = t.main.as_str();
    run.line(
        key!("cli.deploy.smoke_wait"),
        &[
            ("origin", t.main_shown.clone()),
            ("seconds", rt.smoke_budget.as_secs().to_string()),
        ],
    );
    let agent = plain_agent(rt.smoke_base.is_some(), SMOKE_CALL_TIMEOUT, 3);
    let start = rt.clock.elapsed();
    let spent = || rt.clock.elapsed().saturating_sub(start);
    let left = || rt.smoke_budget.saturating_sub(spent());
    let mut wait = SMOKE_FIRST_WAIT;
    loop {
        let seen = match smoke_once(&agent, base, version, theme, &left) {
            Ok(()) => {
                run.line(
                    key!("cli.deploy.smoke_ok"),
                    &[("version", version.to_string())],
                );
                return Ok(());
            }
            Err(s) => s,
        };
        if spent() + wait >= rt.smoke_budget {
            return Err(seen);
        }
        rt.clock.sleep(wait);
        wait = (wait * 2).min(SMOKE_MAX_WAIT);
    }
}

/// Where a smoke check goes for `origin`: the origin itself, or with [`SMOKE_BASE_ENV`] (a
/// loopback test server) `<base>/<host>`.
fn smoke_url(origin: &str, rt: &Runtime, text: &Text) -> Result<String, Failure> {
    let origin = origin.trim_end_matches('/');
    match &rt.smoke_base {
        Some(b) if crate::cloudflare::client::base_override_ok(b) => Ok(format!(
            "{}/{}",
            b.trim_end_matches('/'),
            origin.trim_start_matches("https://")
        )),
        Some(b) => Err(Failure::new(
            Code::SmokeBaseRefused,
            text,
            key!("cli.error.smoke_base_refused"),
            &[("var", SMOKE_BASE_ENV.to_string()), ("value", printable(b))],
        )),
        None => Ok(origin.to_string()),
    }
}

/// What the smoke check looks at.
pub(crate) struct Targets {
    /// Pass/fail: its failure rolls back.
    pub(crate) main: String,
    /// `main` as the owner knows it.
    main_shown: String,
    /// A custom domain that has never served a version: one try after `main` passed, a
    /// warning if it does not serve the version yet.
    domain: Option<(String, String)>,
    /// `main` is a custom domain that has not passed yet.
    domain_is_main: bool,
}

/// The origin, except for a custom domain that has not passed a smoke check yet: its
/// certificate may take longer than the check, so workers.dev decides and the domain is
/// only warned about. Without a workers.dev subdomain the domain decides, with a warning.
pub(crate) fn targets(run: &mut Run, site: &Site, rt: &Runtime) -> Result<Targets, Failure> {
    let text = run.cx.text;
    let origin = site.origin.trim_end_matches('/').to_string();
    let new_domain = site.domain.is_some() && !run.state.domain_smoke_passed;
    let sub = if new_domain {
        setup::workers_subdomain(run.client).map_err(|e| run.api(&e))?
    } else {
        None
    };
    if let Some(sub) = sub {
        let dev = format!("https://{}.{sub}.workers.dev", site.worker);
        return Ok(Targets {
            main: smoke_url(&dev, rt, text)?,
            main_shown: dev,
            domain: Some((smoke_url(&origin, rt, text)?, origin)),
            domain_is_main: false,
        });
    }
    if new_domain {
        run.line(
            key!("cli.deploy.domain_cert_wait"),
            &[("origin", origin.clone())],
        );
    }
    Ok(Targets {
        main: smoke_url(&origin, rt, text)?,
        main_shown: origin,
        domain: None,
        domain_is_main: new_domain,
    })
}

/// After the main check passed: one try on a new custom domain. Not serving the version yet
/// is a warning; serving it (here or as the main target) makes the domain the smoke target
/// from now on.
fn check_domain(
    run: &mut Run,
    t: &Targets,
    version: &str,
    theme: ThemeWant,
    rt: &Runtime,
) -> Result<(), Failure> {
    let passed = match &t.domain {
        None => t.domain_is_main,
        Some((url, shown)) => {
            let agent = plain_agent(rt.smoke_base.is_some(), SMOKE_CALL_TIMEOUT, 3);
            let left = || SMOKE_CALL_TIMEOUT;
            match smoke_once(&agent, url, version, theme, &left) {
                Ok(()) => {
                    run.line(key!("cli.deploy.domain_ok"), &[("origin", shown.clone())]);
                    true
                }
                Err(seen) => {
                    let words = seen.words(run.cx.text, version);
                    run.line(
                        key!("cli.deploy.domain_pending"),
                        &[("origin", shown.clone()), ("seen", words)],
                    );
                    false
                }
            }
        }
    };
    if passed {
        run.update(|s| s.domain_smoke_passed = true)?;
    }
    Ok(())
}

/// One system log row per event in the site's own D1, as the Worker writes them
/// (crates/worker d1/log.rs). A failure is reported, never fatal: the deploy is done.
pub(crate) fn log_events(run: &mut Run, db: &Database, events: &[Event]) {
    const SQL: &str = "INSERT INTO system_log (at_ms, request_id, kind, actor_id, error_code, \
                       detail) VALUES (?, ?, ?, NULL, NULL, ?)";
    let rows: Vec<[String; 4]> = events
        .iter()
        .map(|e| {
            [
                e.at_ms().to_string(),
                e.request_id().as_str().to_string(),
                e.kind().as_str().to_string(),
                e.detail(),
            ]
        })
        .collect();
    let refs: Vec<Vec<&str>> = rows
        .iter()
        .map(|r| r.iter().map(String::as_str).collect())
        .collect();
    let stmts: Vec<(&str, &[&str])> = refs.iter().map(|r| (SQL, r.as_slice())).collect();
    if let Err(e) = db.batch(&stmts) {
        let msg = e.message(run.cx.text);
        run.line(key!("cli.deploy.log_failed"), &[("error", msg)]);
    }
}

pub(crate) fn request_id(kind: &str, now_ms: i64) -> RequestId {
    RequestId::parse(&format!("cli-{kind}-{now_ms}")).unwrap_or_else(RequestId::unknown)
}

pub(crate) fn at_ms(cx: &Context) -> u64 {
    u64::try_from(cx.now_ms).unwrap_or(0)
}

/// Assets, the new version and the version serving now. Nothing is switched yet.
fn ship(
    run: &mut Run,
    site: &Site,
    b: &Bundle,
    rec: &mut DeployRecord,
) -> Result<(String, String), Failure> {
    let assets_jwt = upload_assets(run, site, b)?;
    let theme = rec.theme.clone();
    let version = upload_version(run, site, b, &assets_jwt, theme.as_ref())?;
    rec.version = Some(version.clone());
    put_record(run, rec)?;
    let previous = active_version(run.client, &site.worker).map_err(|e| run.api(&e))?;
    rec.previous = Some(previous.clone());
    put_record(run, rec)?;
    run.line(
        key!("cli.deploy.previous"),
        &[("version", previous.clone())],
    );
    Ok((version, previous))
}

/// Switches to `version`. A lost reply may still have switched, so the active version is
/// read back: only that says whether the new version serves.
fn switch(
    run: &mut Run,
    site: &Site,
    rec: &mut DeployRecord,
    version: &str,
) -> Result<(), Failure> {
    let Err(sent) = deploy_version(run.client, &site.worker, version) else {
        run.line(
            key!("cli.deploy.deployed"),
            &[("version", version.to_string())],
        );
        return Ok(());
    };
    match active_version(run.client, &site.worker) {
        Ok(active) if active == version => {
            run.line(
                key!("cli.deploy.deployed_anyway"),
                &[("version", version.to_string())],
            );
            Ok(())
        }
        Ok(_) => {
            rec.outcome = "failed".into();
            Err(run.api(&sent))
        }
        Err(_) => {
            rec.outcome = "deploy_uncertain".into();
            Err(run.api(&sent))
        }
    }
}

/// Records the smoke result; on failure deploys `previous` again.
fn settle(
    run: &mut Run,
    site: &Site,
    rec: &mut DeployRecord,
    smoked: Result<(), Seen>,
    version: &str,
    previous: &str,
) -> Result<(), Failure> {
    let cx = run.cx;
    let seen = match smoked {
        Ok(()) => {
            rec.outcome = "deployed".into();
            rec.smoke = Some("passed".into());
            return Ok(());
        }
        Err(seen) => seen,
    };
    rec.smoke = Some(seen.record());
    let words = seen.words(cx.text, version);
    if let Err(e) = deploy_version(run.client, &site.worker, previous) {
        rec.outcome = "deploy_uncertain".into();
        return Err(Failure::new(
            Code::SmokeFailed,
            cx.text,
            key!("cli.error.smoke_rollback_failed"),
            &[
                ("version", version.to_string()),
                ("seen", words),
                ("previous", previous.to_string()),
                ("error", e.message(cx.text)),
            ],
        ));
    }
    rec.outcome = "rolled_back".into();
    Err(Failure::new(
        Code::SmokeFailed,
        cx.text,
        key!("cli.error.smoke_rolled_back"),
        &[
            ("version", version.to_string()),
            ("seen", words),
            ("previous", previous.to_string()),
            ("path", state::path(cx.dir).display().to_string()),
        ],
    ))
}

/// The cron, after a deploy that stands. PUT is idempotent, so a failure only asks for
/// another `blog deploy`.
/// <https://developers.cloudflare.com/api/resources/workers/subresources/schedules/methods/update/>
fn cron(run: &mut Run, site: &Site, rec: &mut DeployRecord) -> Result<(), Failure> {
    let call = Call::new(Endpoint::SchedulesPut)
        .param("script_name", site.worker.clone())
        .json(json!([{"cron": CRON}]));
    match run.client.call(&call) {
        Ok(_) => {
            run.line(key!("cli.deploy.cron"), &[("cron", CRON.to_string())]);
            Ok(())
        }
        Err(e) => {
            rec.outcome = "deployed_cron_failed".into();
            Err(Failure::new(
                Code::CronFailed,
                run.cx.text,
                key!("cli.error.cron_failed"),
                &[
                    ("version", rec.version.clone().unwrap_or_default()),
                    ("error", e.message(run.cx.text)),
                ],
            ))
        }
    }
}

/// A bundle missing a migration the database already ran is older code than the database.
fn check_not_older(run: &Run, b: &Bundle, recorded: &[String], allow: bool) -> Result<(), Failure> {
    let missing: Vec<&String> = recorded
        .iter()
        .filter(|r| !b.migrations.iter().any(|m| &m.name == *r))
        .collect();
    if missing.is_empty() || allow {
        return Ok(());
    }
    let list: Vec<String> = missing.iter().map(|m| printable(m)).collect();
    Err(Failure::new(
        Code::BundleOlder,
        run.cx.text,
        key!("cli.error.bundle_older"),
        &[("list", list.join(", "))],
    ))
}

/// System log rows for this run: migrations it applied, the one that failed, and the deploy
/// itself whatever its outcome. Never fails the command.
fn log_run(run: &mut Run, db: &Database, rec: &DeployRecord) {
    let cx = run.cx;
    let rid = request_id("deploy", cx.now_ms);
    let mut events: Vec<Event> = rec
        .migrations
        .iter()
        .map(|m| Event::new(EventKind::Migration, rid.clone(), at_ms(cx)).subject(m))
        .collect();
    if rec.outcome == "migration_failed"
        && let Some(f) = &run.state.migrations.failed
    {
        events.push(
            Event::new(EventKind::Migration, rid.clone(), at_ms(cx)).subject(&format!(
                "{} failed (restore: bookmark {})",
                f.name, f.bookmark
            )),
        );
    }
    let verb = match rec.kind.as_deref() {
        Some("theme_push") => "theme push",
        Some("theme_default") => "theme default",
        _ => "deploy",
    };
    let theme = rec.theme.as_ref().map_or_else(
        || "default".to_string(),
        |t| format!("{} {}", t.name, store::short(&t.hash)),
    );
    events.push(
        Event::new(EventKind::Deploy, rid, at_ms(cx))
            .subject(&format!(
                "{verb} {} {} (previous {})",
                rec.version.as_deref().unwrap_or("-"),
                rec.outcome,
                rec.previous.as_deref().unwrap_or("-")
            ))
            .field("smoke", rec.smoke.as_deref().unwrap_or(""))
            .field("migrations", &rec.migrations.len().to_string())
            .field("theme", &theme),
    );
    log_events(run, db, &events);
}

/// Everything after the deploy record exists. Sets `rec.outcome` on every path.
fn steps(
    run: &mut Run,
    site: &Site,
    db: &Database,
    b: &Bundle,
    rec: &mut DeployRecord,
    flow: &Flow,
) -> Result<(), Failure> {
    if flow.pending || flow.export {
        let mark = rec.bookmark.clone().unwrap_or_default();
        rec.export = Some(crate::dump::export(
            run,
            site,
            db,
            &mark,
            &flow.stamp,
            BACKUP_DIR,
            Purpose::Deploy,
        )?);
        put_record(run, rec)?;
    }
    // Migrations; a failure stops before any code is uploaded.
    let migrated = setup::apply_migrations(run, db, false);
    rec.migrations = run
        .state
        .migrations
        .applied
        .iter()
        .filter(|m| !flow.before.contains(m))
        .cloned()
        .collect();
    if let Err(f) = migrated {
        rec.outcome = "migration_failed".into();
        return Err(f);
    }
    put_record(run, rec)?;
    release(run, site, b, rec, flow)
}

/// Upload, switch, smoke check (rolling back on failure), cron: what every new version goes
/// through, from `blog deploy` and `blog theme push` alike.
pub(crate) fn release(
    run: &mut Run,
    site: &Site,
    b: &Bundle,
    rec: &mut DeployRecord,
    flow: &Flow,
) -> Result<(), Failure> {
    let (version, previous) = ship(run, site, b, rec)?;
    switch(run, site, rec, &version)?;
    // The theme on record is the one this version must serve: a push's, the one a deploy
    // carries, or none (the default).
    let theme = rec.theme.clone();
    let want = ThemeWant::of(theme.as_ref());
    let smoked = smoke(run, &flow.targets, &version, want, flow.rt);
    settle(run, site, rec, smoked, &version, &previous)?;
    check_domain(run, &flow.targets, &version, want, flow.rt)?;
    cron(run, site, rec)?;
    run.line(
        key!("cli.deploy.done"),
        &[("version", version), ("origin", site.origin.clone())],
    );
    Ok(())
}

/// What [`steps`] needs beyond the run.
pub(crate) struct Flow<'a> {
    pub(crate) rt: &'a Runtime<'a>,
    pub(crate) targets: Targets,
    pub(crate) stamp: String,
    pub(crate) before: Vec<String>,
    pub(crate) pending: bool,
    pub(crate) export: bool,
}

/// What a new version is made from, read once the folder is locked.
pub(crate) struct Ready<'r> {
    pub(crate) site: Site,
    pub(crate) db: Database<'r>,
    pub(crate) b: Bundle,
    /// Migrations the database recorded before this run.
    pub(crate) before: Vec<String>,
    /// The bundle has migrations the database has not run.
    pub(crate) pending: bool,
}

/// The part `blog deploy` and `blog theme push` share before anything changes: lock, token,
/// state, the bundle, and the guards on the database's migrations. Then `body`.
pub(crate) fn prepared(
    client: &mut Client,
    account_id: Option<&str>,
    opts: &Options,
    cx: &Context,
    rt: &Runtime,
    out: &mut dyn Write,
    body: &mut dyn FnMut(&mut Run, Ready) -> Result<(), Failure>,
) -> Result<(), Failure> {
    check_source(opts, cx.text)?;
    let _lock = Lock::take(cx)?;
    let (st, site) = start(client, account_id, cx)?;
    restore_pending(&st, cx)?;
    // Refuses a bad test override before anything is built.
    smoke_url(&site.origin, rt, cx.text)?;
    let client: &Client = client;
    let mut first = Run {
        client,
        cx,
        out,
        state: st,
        secrets: Vec::new(),
    };
    let (_, b) = bundle(&mut first, opts)?;
    // The migrations come with the code they belong to.
    let bcx = Context {
        dir: cx.dir,
        migrations: &b.migrations,
        now_ms: cx.now_ms,
        text: cx.text,
    };
    let Run { out, state, .. } = first;
    let mut run = Run {
        client,
        cx: &bcx,
        out,
        state,
        secrets: Vec::new(),
    };
    let db = Database::new(client, site.db_id.clone());
    migrations::check_ours(&db).map_err(|e| setup::migration_error(&run, e))?;
    let before = migrations::recorded(&db).map_err(|e| run.api(&e))?;
    check_not_older(&run, &b, &before, opts.allow_older)?;
    let pending = b.migrations.iter().any(|m| !before.contains(&m.name));
    let b = b.clone();
    body(
        &mut run,
        Ready {
            site,
            db,
            b,
            before,
            pending,
        },
    )
}

/// The record's final outcome, saved, and the system log rows; `result` is the run's.
pub(crate) fn finish(
    run: &mut Run,
    db: &Database,
    rec: &mut DeployRecord,
    result: Result<(), Failure>,
) -> Result<(), Failure> {
    if result.is_err() && rec.outcome == "started" {
        rec.outcome = "failed".into();
    }
    let saved = put_record(run, rec);
    log_run(run, db, rec);
    result?;
    saved
}

/// `blog deploy`. Progress goes to `out` as each step finishes.
///
/// # Errors
/// [`Failure`] with a stable code; the deploy record in `site.local.toml` says how far it
/// got.
pub fn run(
    client: &mut Client,
    account_id: Option<&str>,
    opts: &Options,
    cx: &Context,
    rt: &Runtime,
    out: &mut dyn Write,
) -> Result<(), Failure> {
    prepared(client, account_id, opts, cx, rt, out, &mut |run, r| {
        let Ready {
            site,
            db,
            mut b,
            before,
            pending,
        } = r;
        let targets = targets(run, &site, rt)?;

        // Backup, recorded before anything changes.
        let (at, stamp) = utc(cx.now_ms);
        let mut rec = DeployRecord {
            at,
            outcome: "started".into(),
            bundle_migrations: Some(b.migrations.iter().map(|m| m.name.clone()).collect()),
            ..DeployRecord::default()
        };
        let mark = migrations::bookmark(&db, run.client).map_err(|e| run.api(&e))?;
        rec.bookmark = Some(mark.clone());
        run.update(|s| push_capped(&mut s.deploys, rec.clone()))?;
        run.line(key!("cli.deploy.bookmark"), &[("bookmark", mark)]);
        let flow = Flow {
            rt,
            targets,
            stamp,
            before,
            pending,
            export: opts.export,
        };
        // Every version uploads all its static files: the pushed theme must come along,
        // found before the database or anything on Cloudflare changes.
        let result = carry_theme(run, &site, &flow.targets.main, opts.theme, rt)
            .and_then(|t| t.map(|t| attach_theme(run, &mut b, &t)).transpose())
            .and_then(|theme| {
                rec.theme = theme;
                put_record(run, &rec)?;
                steps(run, &site, &db, &b, &mut rec, &flow)
            });
        finish(run, &db, &mut rec, result)
    })
}

/// The active version's pushed theme name, from its `PLUMBLOG_THEME` binding: `(version,
/// name)`.
pub(crate) fn active_theme(run: &Run, site: &Site) -> Result<(String, Option<String>), Failure> {
    let active = active_version(run.client, &site.worker).map_err(|e| run.api(&e))?;
    let name = theme_binding(run.client, &site.worker, &active).map_err(|e| run.api(&e))?;
    Ok((active, name))
}

/// `PLUMBLOG_THEME` of `version`, if set.
pub(crate) fn theme_binding(
    client: &Client,
    worker: &str,
    version: &str,
) -> Result<Option<String>, ApiError> {
    Ok(version_bindings(client, worker, version)?
        .iter()
        .find(|b| {
            b.get("type").and_then(Value::as_str) == Some("plain_text")
                && b.get("name").and_then(Value::as_str) == Some(THEME_BINDING)
        })
        .and_then(|b| str_at(b, "text"))
        .map(str::to_string))
}

/// The theme the serving version has, to upload again with the new one: the local copy when
/// the deploy record of the serving version names its hash, else fetched from the site and
/// verified. `None`: no pushed theme, or [`ThemeCarry::Drop`].
fn carry_theme(
    run: &mut Run,
    site: &Site,
    base: &str,
    carry: ThemeCarry,
    rt: &Runtime,
) -> Result<Option<store::Built>, Failure> {
    let (active, name) = active_theme(run, site)?;
    let Some(name) = name else {
        return Ok(None);
    };
    if carry == ThemeCarry::Drop {
        run.line(key!("cli.deploy.theme_dropped"), &[("name", name)]);
        return Ok(None);
    }
    let expected = run
        .state
        .deploys
        .iter()
        .rev()
        .find(|d| d.version.as_deref() == Some(active.as_str()))
        .and_then(|d| d.theme.clone())
        .filter(|t| t.name == name)
        .map(|t| t.hash);
    if let Some(h) = &expected {
        match store::load(run.cx.dir, &name, h) {
            Ok(b) => {
                run.line(
                    key!("cli.deploy.theme_local"),
                    &[("name", name), ("hash", store::short(h).to_string())],
                );
                return Ok(Some(b));
            }
            Err(why) => run.line(
                key!("cli.deploy.theme_local_unusable"),
                &[("name", name.clone()), ("reason", why.words(run.cx.text))],
            ),
        }
    }
    // From workers.dev when the site has a custom domain: a cache rule there ("Cache
    // Everything") may serve an older theme.json that still matches its own hash.
    let sub = match &site.domain {
        Some(_) => setup::workers_subdomain(run.client).map_err(|e| run.api(&e))?,
        None => None,
    };
    let base = match sub {
        Some(sub) => smoke_url(
            &format!("https://{}.{sub}.workers.dev", site.worker),
            rt,
            run.cx.text,
        )?,
        None => base.to_owned(),
    };
    let agent = plain_agent(rt.smoke_base.is_some(), THEME_FETCH_TIMEOUT, 3);
    let fresh = run.cx.now_ms.to_string();
    let fetched = store::fetch(&agent, &base, &name, &fresh).and_then(|b| match &expected {
        Some(h) if *h != b.doc.hash => Err(store::Why::Said(
            key!("cli.theme.why.served_other"),
            vec![
                ("served", store::short(&b.doc.hash).to_string()),
                ("recorded", store::short(h).to_string()),
            ],
        )),
        _ => Ok(b),
    });
    let b = fetched.map_err(|why| {
        Failure::new(
            Code::ThemeUnrecoverable,
            run.cx.text,
            key!("cli.error.theme_unrecoverable"),
            &[("name", name.clone()), ("reason", why.words(run.cx.text))],
        )
    })?;
    store::save(run.cx.dir, &b).map_err(|(path, reason)| theme_unwritable(run, path, reason))?;
    run.line(
        key!("cli.deploy.theme_fetched"),
        &[
            ("name", name),
            ("hash", store::short(&b.doc.hash).to_string()),
        ],
    );
    Ok(Some(b))
}

/// Per request while fetching a theme back from the site (assets may be 25 MB).
const THEME_FETCH_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) fn theme_unwritable(run: &Run, path: String, reason: String) -> Failure {
    Failure::new(
        Code::ThemeDirUnreadable,
        run.cx.text,
        key!("cli.theme.dir_unreadable"),
        &[("dir", path), ("reason", reason)],
    )
}

/// Adds the theme's files (from its local copy) to the bundle's assets; its record.
pub(crate) fn attach_theme(
    run: &Run,
    b: &mut Bundle,
    t: &store::Built,
) -> Result<ThemeRecord, Failure> {
    let extra = store::assets(run.cx.dir, t)
        .map_err(|(path, reason)| theme_unwritable(run, path, reason))?;
    b.assets.retain(|a| !a.path.starts_with("/_theme/"));
    b.assets.extend(extra);
    Ok(t.record())
}

/// Deployable versions, newest first (wrangler `fetchDeployableVersions`:
/// `GET .../versions?deployable=true`, `result.items[].id`).
fn recent_versions(client: &Client, worker: &str) -> Result<Vec<String>, ApiError> {
    let r = client.call(
        &Call::new(Endpoint::VersionList)
            .param("script_name", worker.to_string())
            .query("deployable", "true"),
    )?;
    Ok(r.get("items")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|i| str_at(i, "id").map(str::to_string))
                .collect()
        })
        .unwrap_or_default())
}

/// `blog rollback`: a code rollback (`--to`, else the last deploy's previous version) or a
/// database restore (`--db <bookmark> --yes`).
///
/// # Errors
/// [`Failure`] with a stable code.
pub fn rollback(
    client: &mut Client,
    account_id: Option<&str>,
    opts: &RollbackOptions,
    cx: &Context,
    rt: &Runtime,
    out: &mut dyn Write,
) -> Result<(), Failure> {
    if let Some(b) = &opts.db
        && !opts.yes
    {
        return Err(Failure::new(
            Code::RollbackDbUnconfirmed,
            cx.text,
            key!("cli.error.rollback_db_unconfirmed"),
            &[("bookmark", printable(b))],
        ));
    }
    let _lock = Lock::take(cx)?;
    let (st, site) = start(client, account_id, cx)?;
    let client: &Client = client;
    let mut run = Run {
        client,
        cx,
        out,
        state: st,
        secrets: Vec::new(),
    };
    match &opts.db {
        Some(b) => restore_db(&mut run, &site, b, opts.clear_failed_migration),
        None => rollback_code(&mut run, &site, opts, rt),
    }
}

/// Records that may be what serves now: their `previous` is the default rollback target.
pub(crate) const CURRENT_OUTCOMES: &[&str] =
    &["deployed", "deployed_cron_failed", "deploy_uncertain"];

/// `--to` (one of the script's recent versions) or the previous version of the newest record
/// that may be serving; never the setup placeholder.
fn rollback_target(run: &Run, site: &Site, to: Option<&str>) -> Result<String, Failure> {
    let cx = run.cx;
    let client = run.client;
    let target = match to {
        Some(v) => {
            let recent = recent_versions(client, &site.worker).map_err(|e| run.api(&e))?;
            if !recent.iter().any(|r| r == v) {
                let shown: Vec<String> = recent.iter().take(5).cloned().collect();
                return Err(Failure::new(
                    Code::RollbackVersionUnknown,
                    cx.text,
                    key!("cli.error.rollback_version_unknown"),
                    &[("version", printable(v)), ("list", shown.join(", "))],
                ));
            }
            v.to_string()
        }
        None => run
            .state
            .deploys
            .iter()
            .rev()
            .find(|d| CURRENT_OUTCOMES.contains(&d.outcome.as_str()) && d.previous.is_some())
            .and_then(|d| d.previous.clone())
            .ok_or_else(|| {
                Failure::new(
                    Code::RollbackNoRecord,
                    cx.text,
                    key!("cli.error.rollback_no_record"),
                    &[("path", state::path(cx.dir).display().to_string())],
                )
            })?,
    };
    refuse_placeholder(run, site, &target)?;
    Ok(target)
}

/// Never the setup placeholder: every secret put or delete makes a new version of its code,
/// so the recorded id is not enough; any version without `DB` is a placeholder.
pub(crate) fn refuse_placeholder(run: &Run, site: &Site, target: &str) -> Result<(), Failure> {
    let placeholder = run.state.placeholder_version.as_deref() == Some(target)
        || !has_db_binding(run.client, &site.worker, target).map_err(|e| run.api(&e))?;
    if placeholder {
        return Err(Failure::new(
            Code::RollbackPlaceholder,
            run.cx.text,
            key!("cli.error.rollback_placeholder"),
            &[("version", target.to_string())],
        ));
    }
    Ok(())
}

/// Whether the version binds the database: every version `blog deploy` uploads does, the
/// setup placeholder and the versions secret changes make from it do not. Shape from
/// wrangler 4.160.0 (`fetchUploadedVersion`; `versions view` reads `resources.bindings`).
fn has_db_binding(client: &Client, worker: &str, version: &str) -> Result<bool, ApiError> {
    Ok(version_bindings(client, worker, version)?.iter().any(|b| {
        b.get("type").and_then(Value::as_str) == Some("d1")
            && b.get("name").and_then(Value::as_str) == Some("DB")
    }))
}

/// A version's bindings (`result.resources.bindings`).
fn version_bindings(client: &Client, worker: &str, version: &str) -> Result<Vec<Value>, ApiError> {
    let v = client.call(
        &Call::new(Endpoint::VersionGet)
            .param("script_name", worker.to_string())
            .param("version_id", version.to_string()),
    )?;
    v.get("resources")
        .and_then(|r| r.get("bindings"))
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| malformed(Endpoint::VersionGet, "no resources.bindings"))
}

/// deploy's `--allow-older` guard for a rollback target: the migrations its bundle had (from
/// its deploy record) must include every one the database ran. No record: unknown, so
/// refused too. With `allow`, a warning instead.
fn check_target_not_older(
    run: &mut Run,
    site: &Site,
    target: &str,
    allow: bool,
) -> Result<(), Failure> {
    let cx = run.cx;
    let known = run
        .state
        .deploys
        .iter()
        .rev()
        .find(|d| d.version.as_deref() == Some(target))
        .and_then(|d| d.bundle_migrations.clone());
    let (k, args): (_, Vec<(&str, String)>) = match known {
        None => (
            if allow {
                key!("cli.rollback.unknown_allowed")
            } else {
                key!("cli.error.rollback_unrecorded")
            },
            vec![("version", target.to_string())],
        ),
        Some(had) => {
            let db = Database::new(run.client, site.db_id.clone());
            let ran = migrations::recorded(&db).map_err(|e| run.api(&e))?;
            let missing: Vec<String> = ran
                .iter()
                .filter(|m| !had.contains(m))
                .map(|m| printable(m))
                .collect();
            if missing.is_empty() {
                return Ok(());
            }
            (
                if allow {
                    key!("cli.rollback.older_allowed")
                } else {
                    key!("cli.error.rollback_older")
                },
                vec![
                    ("version", target.to_string()),
                    ("list", missing.join(", ")),
                ],
            )
        }
    };
    if allow {
        run.line(k, &args);
        return Ok(());
    }
    Err(Failure::new(Code::RollbackOlder, cx.text, k, &args))
}

fn rollback_code(
    run: &mut Run,
    site: &Site,
    opts: &RollbackOptions,
    rt: &Runtime,
) -> Result<(), Failure> {
    smoke_url(&site.origin, rt, run.cx.text)?;
    let target = rollback_target(run, site, opts.to.as_deref())?;
    rollback_to(run, site, target, opts.allow_older, rt)
}

/// Deploys `target` (an earlier version, not a placeholder) after deploy's migration guard,
/// checks it, and goes back to the version from before if the check fails.
pub(crate) fn rollback_to(
    run: &mut Run,
    site: &Site,
    target: String,
    allow_older: bool,
    rt: &Runtime,
) -> Result<(), Failure> {
    let cx = run.cx;
    let client = run.client;
    check_target_not_older(run, site, &target, allow_older)?;
    let targets = targets(run, site, rt)?;
    let current = active_version(client, &site.worker).map_err(|e| run.api(&e))?;
    deploy_version(client, &site.worker, &target).map_err(|e| run.api(&e))?;
    run.line(
        key!("cli.rollback.deployed"),
        &[("version", target.clone()), ("previous", current.clone())],
    );
    let smoked = smoke(run, &targets, &target, ThemeWant::Any, rt);
    // A target that fails its check goes away again: the version from before serves.
    let restored = smoked
        .as_ref()
        .err()
        .map(|_| deploy_version(client, &site.worker, &current));
    let (at, _) = utc(cx.now_ms);
    let record = RollbackRecord {
        at,
        kind: "version".into(),
        to: target.clone(),
        previous: Some(current.clone()),
        smoke: Some(match (&smoked, &restored) {
            (Ok(()), _) => "passed".into(),
            (Err(s), Some(Ok(()))) => format!("{}; back to previous", s.record()),
            (Err(s), _) => s.record(),
        }),
        outcome: None,
    };
    let saved = run.update(|s| push_capped(&mut s.rollbacks, record));
    let db = Database::new(client, site.db_id.clone());
    let event = Event::new(
        EventKind::Deploy,
        request_id("rollback", cx.now_ms),
        at_ms(cx),
    )
    .subject(&format!("rollback to {target} (was {current})"))
    .field("smoke", if smoked.is_ok() { "passed" } else { "failed" });
    log_events(run, &db, &[event]);
    let Err(seen) = smoked else {
        return saved;
    };
    let words = seen.words(cx.text, &target);
    Err(match restored {
        Some(Ok(())) => Failure::new(
            Code::SmokeFailed,
            cx.text,
            key!("cli.error.rollback_smoke_restored"),
            &[("version", target), ("seen", words), ("previous", current)],
        ),
        Some(Err(e)) => Failure::new(
            Code::SmokeFailed,
            cx.text,
            key!("cli.error.rollback_smoke_failed"),
            &[
                ("version", target),
                ("seen", words),
                ("previous", current),
                ("error", e.message(cx.text)),
            ],
        ),
        // `restored` is None only when the check passed, which returned above.
        None => Failure::new(
            Code::Internal,
            cx.text,
            key!("cli.error.internal"),
            &[("detail", "rollback state".to_string())],
        ),
    })
}

/// Time Travel restore, bookmark as a query parameter (wrangler 4.160.0
/// `d1 time-travel restore`). Overwrites the database and cancels running queries.
/// <https://developers.cloudflare.com/api/resources/d1/subresources/database/subresources/time_travel/methods/restore/>
fn restore_db(run: &mut Run, site: &Site, bookmark: &str, clear: bool) -> Result<(), Failure> {
    let cx = run.cx;
    let client = run.client;
    let call = Call::new(Endpoint::D1Restore)
        .param("database_id", site.db_id.clone())
        .query("bookmark", bookmark.to_string());
    let r = client.call(&call).map_err(|e| run.api(&e))?;
    let previous = str_at(&r, "previous_bookmark").map(str::to_string);
    // Said before anything else can fail: the restore is done and must not be repeated.
    run.line(
        key!("cli.rollback.restored"),
        &[("bookmark", bookmark.to_string())],
    );
    match &previous {
        Some(p) => run.line(key!("cli.rollback.undo"), &[("previous", p.clone())]),
        None => run.line(key!("cli.rollback.undo_unknown"), &[]),
    }
    // Only the failed migration's own bookmark (or an explicit flag) undoes it.
    let mut clear_failed = false;
    if let Some(f) = run.state.migrations.failed.clone() {
        if f.bookmark == bookmark || clear {
            clear_failed = true;
            run.line(key!("cli.rollback.failed_cleared"), &[("name", f.name)]);
        } else {
            run.line(
                key!("cli.rollback.failed_kept"),
                &[("name", f.name), ("bookmark", f.bookmark)],
            );
        }
    }
    let (at, _) = utc(cx.now_ms);
    let record = RollbackRecord {
        at,
        kind: "database".into(),
        to: bookmark.to_string(),
        previous: previous.clone(),
        smoke: None,
        outcome: None,
    };
    let db = Database::new(client, site.db_id.clone());
    // What the restored database records; a failed read keeps the old list (setup and
    // deploy read the database again anyway).
    let names = migrations::recorded(&db).ok();
    let saved = run.update(|s| {
        if clear_failed {
            s.migrations.failed = None;
        }
        push_capped(&mut s.rollbacks, record);
        if let Some(n) = names {
            s.migrations.applied = n;
        }
    });
    if let Err(f) = saved {
        return Err(Failure::new(
            Code::StateUnwritable,
            cx.text,
            key!("cli.error.restore_unsaved"),
            &[("bookmark", bookmark.to_string()), ("error", f.message)],
        ));
    }
    let event = Event::new(
        EventKind::Migration,
        request_id("restore", cx.now_ms),
        at_ms(cx),
    )
    .subject(&format!(
        "database restored to {bookmark} (undo: {})",
        previous.as_deref().unwrap_or("-")
    ));
    log_events(run, &db, &[event]);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_formats() {
        assert_eq!(utc(0).0, "1970-01-01T00:00:00Z");
        assert_eq!(
            utc(1_790_000_000_000),
            (
                "2026-09-21T14:13:20Z".to_string(),
                "20260921T141320Z".to_string()
            )
        );
        assert_eq!(utc(951_782_400_000).0, "2000-02-29T00:00:00Z");
    }

    /// One value, three places: the code, the dev config and the architecture doc.
    #[test]
    fn cron_and_date_match_the_dev_config_and_docs() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let dev = fs::read_to_string(root.join("tools/wrangler.dev.toml")).unwrap();
        assert!(dev.contains(&format!("compatibility_date = \"{COMPATIBILITY_DATE}\"")));
        assert!(dev.contains(&format!("crons = [\"{CRON}\"]")));
        assert!(dev.contains(&format!("binding = \"{VERSION_BINDING}\"")));
        let arch = fs::read_to_string(root.join("docs/ARCHITECTURE.md")).unwrap();
        assert!(
            arch.lines()
                .any(|l| l.starts_with("| Scheduled jobs |") && l.contains(&format!("`{CRON}`"))),
            "ARCHITECTURE Platform row must name {CRON}"
        );
    }
}
