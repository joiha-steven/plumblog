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
use crate::state::{self, DeployRecord, RollbackRecord, SiteState, push_capped};
use crate::text::{Code, Text};
use plumblog_i18n::key;
use plumblog_model::RequestId;
use plumblog_syslog::{Event, EventKind};
use serde_json::{Value, json};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
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
/// Test-only smoke target instead of the site's origin; loopback only, like the API base.
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
/// A D1 export of a free-plan database (500 MB at most) finishes well inside this.
const EXPORT_BUDGET: Duration = Duration::from_mins(15);
const EXPORT_POLL_WAIT: Duration = Duration::from_secs(1);
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
    /// A release bundle; without it the bundle is built in the repo.
    pub bundle: Option<PathBuf>,
    /// Deploy a bundle that lacks migrations the database already has.
    pub allow_older: bool,
}

/// `blog rollback`.
#[derive(Clone, Debug, Default)]
pub struct RollbackOptions {
    pub to: Option<String>,
    pub db: Option<String>,
    pub yes: bool,
    /// With `--db`: forget the recorded failed migration although `db` is not its bookmark.
    pub clear_failed_migration: bool,
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

fn str_at<'v>(v: &'v Value, key: &str) -> Option<&'v str> {
    v.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

/// The site as `blog setup` left it.
struct Site {
    account: String,
    worker: String,
    origin: String,
    db_name: String,
    db_id: String,
    bucket: String,
    turnstile: bool,
}

fn site(state: Option<&SiteState>, cx: &Context) -> Result<Site, Failure> {
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
    })
}

/// Loads the state, verifies the token and checks the account is the site's.
fn start(
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

/// The bundle: `--bundle`, else built in the repo with [`BUILD_SCRIPT`].
fn bundle(run: &mut Run, opts: &Options) -> Result<(PathBuf, Bundle), Failure> {
    let text = run.cx.text;
    let dir = if let Some(b) = &opts.bundle {
        b.clone()
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

/// Replaces the newest deploy record (this run's, pushed by [`run`]) and saves.
fn put_record(run: &mut Run, rec: &DeployRecord) -> Result<(), Failure> {
    match run.state.deploys.last_mut() {
        Some(last) => *last = rec.clone(),
        None => run.state.deploys.push(rec.clone()),
    }
    run.save()
}

/// One deploy or rollback at a time per site folder: two runs would race on the migrations
/// and the record. A lock left by a crash is named in the error, to be removed by hand.
struct Lock(PathBuf);

pub const LOCK_FILE: &str = ".blog-deploy.lock";

impl Lock {
    fn take(cx: &Context) -> Result<Self, Failure> {
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

/// D1 export into `backups/<utc>-<db>.sql` (0600). Polling as the docs say: re-POST with
/// `current_bookmark` until `status` is `complete` (an export nobody polls is cancelled),
/// then download `result.signed_url` (valid 1 h), without the API token.
/// <https://developers.cloudflare.com/api/resources/d1/subresources/database/methods/export/>
fn export(run: &mut Run, site: &Site, rt: &Runtime, stamp: &str) -> Result<String, Failure> {
    run.line(key!("cli.deploy.exporting"), &[]);
    let text = run.cx.text;
    let fail = |detail: String| {
        Failure::new(
            Code::ExportFailed,
            text,
            key!("cli.error.export_failed"),
            &[("detail", detail)],
        )
    };
    let started = rt.clock.elapsed();
    let mut current: Option<String> = None;
    let url = loop {
        let mut body = json!({"output_format": "polling"});
        if let Some(b) = &current {
            body["current_bookmark"] = json!(b);
        }
        let call = Call::new(Endpoint::D1Export)
            .param("database_id", site.db_id.clone())
            .json(body);
        let r = run.client.call(&call).map_err(|e| run.api(&e))?;
        match str_at(&r, "status") {
            Some("complete") => {
                break r
                    .get("result")
                    .and_then(|x| str_at(x, "signed_url"))
                    .map(str::to_string)
                    .ok_or_else(|| run.api(&malformed(Endpoint::D1Export, "no signed_url")))?;
            }
            Some("error") => {
                return Err(fail(printable(str_at(&r, "error").unwrap_or("error"))));
            }
            _ => {}
        }
        current = str_at(&r, "at_bookmark").map(str::to_string).or(current);
        if rt.clock.elapsed().saturating_sub(started) > EXPORT_BUDGET {
            return Err(fail(format!("{} s", EXPORT_BUDGET.as_secs())));
        }
        rt.clock.sleep(EXPORT_POLL_WAIT);
    };
    let rel = format!("{BACKUP_DIR}/{stamp}-{}.sql", site.db_name);
    let path = run.cx.dir.join(&rel);
    download(&url, &path, run.client.is_local()).map_err(|reason| {
        Failure::new(
            Code::BackupUnwritable,
            text,
            key!("cli.error.backup_unwritable"),
            &[("path", path.display().to_string()), ("reason", reason)],
        )
    })?;
    run.line(key!("cli.deploy.exported"), &[("file", rel.clone())]);
    Ok(rel)
}

/// No token, ever: these go to the site and to signed URLs.
fn plain_agent(local: bool, timeout: Duration, redirects: u32) -> ureq::Agent {
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

/// Downloads `url` into a new file at `path` (mode 0600, directory 0700). The URL is a
/// bearer link, so errors never include it. Plain HTTP only to a loopback test server.
fn download(url: &str, path: &Path, local: bool) -> Result<(), String> {
    let loopback =
        crate::cloudflare::client::base_override_ok(url.split(['?', '#']).next().unwrap_or(""));
    if !(url.starts_with("https://") || local && loopback) {
        return Err("the export link is not https".into());
    }
    if let Some(dir) = path.parent() {
        let mut b = fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        b.create(dir).map_err(|e| e.to_string())?;
    }
    let agent = plain_agent(local, Duration::from_mins(30), 3);
    let resp = agent.get(url).call().map_err(|e| {
        // ureq's error text can include the URL.
        let s = e.to_string();
        if s.contains(url) {
            "no reply".to_string()
        } else {
            s
        }
    })?;
    let status = resp.status().as_u16();
    if status != 200 {
        return Err(format!("HTTP {status}"));
    }
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path).map_err(|e| e.to_string())?;
    let copied =
        std::io::copy(&mut resp.into_body().into_reader(), &mut f).and_then(|_| f.sync_all());
    if let Err(e) = copied {
        drop(f);
        let _ = fs::remove_file(path);
        return Err(e.to_string());
    }
    Ok(())
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
) -> Result<String, Failure> {
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

/// What a failed smoke check saw: data for the record, words for the owner.
enum Seen {
    NoReply(&'static str, String),
    Status(&'static str, u16),
    Version(String),
}

impl Seen {
    fn record(&self) -> String {
        match self {
            Self::NoReply(p, _) => format!("{p} no-reply"),
            Self::Status(p, s) => format!("{p} http-{s}"),
            Self::Version(v) => format!("/healthz version {v}"),
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
        }
    }
}

/// One try. Each call's timeout is capped by `left()`, so the check never outlives its budget.
fn smoke_once(
    agent: &ureq::Agent,
    base: &str,
    version: &str,
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
    get("/").map(drop)
}

/// `GET <base>/healthz` until it answers 200 with `x-plumblog-version: <version>`, then
/// `GET <base>/` for 200; retried with backoff. Calls and waits together end within
/// `rt.smoke_budget`.
fn smoke(run: &mut Run, base: &str, version: &str, rt: &Runtime) -> Result<(), Seen> {
    run.line(
        key!("cli.deploy.smoke_wait"),
        &[
            ("origin", base.to_string()),
            ("seconds", rt.smoke_budget.as_secs().to_string()),
        ],
    );
    let agent = plain_agent(rt.smoke_base.is_some(), SMOKE_CALL_TIMEOUT, 3);
    let start = rt.clock.elapsed();
    let spent = || rt.clock.elapsed().saturating_sub(start);
    let left = || rt.smoke_budget.saturating_sub(spent());
    let mut wait = SMOKE_FIRST_WAIT;
    loop {
        let seen = match smoke_once(&agent, base, version, &left) {
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

/// The smoke target: [`SMOKE_BASE_ENV`] when it is a loopback test server, else the origin.
fn smoke_base(site: &Site, rt: &Runtime, text: &Text) -> Result<String, Failure> {
    match &rt.smoke_base {
        Some(b) if crate::cloudflare::client::base_override_ok(b) => {
            Ok(b.trim_end_matches('/').to_string())
        }
        Some(b) => Err(Failure::new(
            Code::SmokeBaseRefused,
            text,
            key!("cli.error.smoke_base_refused"),
            &[("var", SMOKE_BASE_ENV.to_string()), ("value", printable(b))],
        )),
        None => Ok(site.origin.trim_end_matches('/').to_string()),
    }
}

/// One system log row per event in the site's own D1, as the Worker writes them
/// (crates/worker d1/log.rs). A failure is reported, never fatal: the deploy is done.
fn log_events(run: &mut Run, db: &Database, events: &[Event]) {
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

fn request_id(kind: &str, now_ms: i64) -> RequestId {
    RequestId::parse(&format!("cli-{kind}-{now_ms}")).unwrap_or_else(RequestId::unknown)
}

fn at_ms(cx: &Context) -> u64 {
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
    let version = upload_version(run, site, b, &assets_jwt)?;
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
    events.push(
        Event::new(EventKind::Deploy, rid, at_ms(cx))
            .subject(&format!(
                "deploy {} {} (previous {})",
                rec.version.as_deref().unwrap_or("-"),
                rec.outcome,
                rec.previous.as_deref().unwrap_or("-")
            ))
            .field("smoke", rec.smoke.as_deref().unwrap_or(""))
            .field("migrations", &rec.migrations.len().to_string()),
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
        rec.export = Some(export(run, site, flow.rt, &flow.stamp)?);
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
    let (version, previous) = ship(run, site, b, rec)?;
    switch(run, site, rec, &version)?;
    let smoked = smoke(run, &flow.base, &version, flow.rt);
    settle(run, site, rec, smoked, &version, &previous)?;
    cron(run, site, rec)?;
    run.line(
        key!("cli.deploy.done"),
        &[("version", version), ("origin", site.origin.clone())],
    );
    Ok(())
}

/// What [`steps`] needs beyond the run.
struct Flow<'a> {
    rt: &'a Runtime<'a>,
    base: String,
    stamp: String,
    before: Vec<String>,
    pending: bool,
    export: bool,
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
    let _lock = Lock::take(cx)?;
    let (st, site) = start(client, account_id, cx)?;
    restore_pending(&st, cx)?;
    let base = smoke_base(&site, rt, cx.text)?;
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

    // Backup, recorded before anything changes.
    let (at, stamp) = utc(cx.now_ms);
    let mut rec = DeployRecord {
        at,
        outcome: "started".into(),
        ..DeployRecord::default()
    };
    let mark = migrations::bookmark(&db, client).map_err(|e| run.api(&e))?;
    rec.bookmark = Some(mark.clone());
    push_capped(&mut run.state.deploys, rec.clone());
    run.save()?;
    run.line(key!("cli.deploy.bookmark"), &[("bookmark", mark)]);
    let flow = Flow {
        rt,
        base,
        stamp,
        before,
        pending,
        export: opts.export,
    };
    let result = steps(&mut run, &site, &db, &b, &mut rec, &flow);
    if result.is_err() && rec.outcome == "started" {
        rec.outcome = "failed".into();
    }
    let saved = put_record(&mut run, &rec);
    log_run(&mut run, &db, &rec);
    result?;
    saved
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
        None => rollback_code(&mut run, &site, opts.to.as_deref(), rt),
    }
}

/// Records that may be what serves now: their `previous` is the default rollback target.
const CURRENT_OUTCOMES: &[&str] = &["deployed", "deployed_cron_failed", "deploy_uncertain"];

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
    if run.state.placeholder_version.as_deref() == Some(target.as_str()) {
        return Err(Failure::new(
            Code::RollbackPlaceholder,
            cx.text,
            key!("cli.error.rollback_placeholder"),
            &[("version", target)],
        ));
    }
    Ok(target)
}

fn rollback_code(
    run: &mut Run,
    site: &Site,
    to: Option<&str>,
    rt: &Runtime,
) -> Result<(), Failure> {
    let cx = run.cx;
    let base = smoke_base(site, rt, cx.text)?;
    let client = run.client;
    let target = rollback_target(run, site, to)?;
    let current = active_version(client, &site.worker).map_err(|e| run.api(&e))?;
    deploy_version(client, &site.worker, &target).map_err(|e| run.api(&e))?;
    run.line(
        key!("cli.rollback.deployed"),
        &[("version", target.clone()), ("previous", current.clone())],
    );
    let smoked = smoke(run, &base, &target, rt);
    // A target that fails its check goes away again: the version from before serves.
    let restored = smoked
        .as_ref()
        .err()
        .map(|_| deploy_version(client, &site.worker, &current));
    let (at, _) = utc(cx.now_ms);
    push_capped(
        &mut run.state.rollbacks,
        RollbackRecord {
            at,
            kind: "version".into(),
            to: target.clone(),
            previous: Some(current.clone()),
            smoke: Some(match (&smoked, &restored) {
                (Ok(()), _) => "passed".into(),
                (Err(s), Some(Ok(()))) => format!("{}; back to previous", s.record()),
                (Err(s), _) => s.record(),
            }),
        },
    );
    let saved = run.save();
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
    if let Some(f) = run.state.migrations.failed.clone() {
        if f.bookmark == bookmark || clear {
            run.state.migrations.failed = None;
            run.line(key!("cli.rollback.failed_cleared"), &[("name", f.name)]);
        } else {
            run.line(
                key!("cli.rollback.failed_kept"),
                &[("name", f.name), ("bookmark", f.bookmark)],
            );
        }
    }
    let (at, _) = utc(cx.now_ms);
    push_capped(
        &mut run.state.rollbacks,
        RollbackRecord {
            at,
            kind: "database".into(),
            to: bookmark.to_string(),
            previous: previous.clone(),
            smoke: None,
        },
    );
    let db = Database::new(client, site.db_id.clone());
    // What the restored database records; a failed read keeps the old list (setup and
    // deploy read the database again anyway).
    if let Ok(names) = migrations::recorded(&db) {
        run.state.migrations.applied = names;
    }
    if let Err(f) = run.save() {
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
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
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
