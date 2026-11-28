//! `blog theme push | rollback | list`: a theme becomes static assets of a new Worker version
//! (docs/ARCHITECTURE.md, Themes), made through deploy's own path, so pushing and going back
//! are Worker versions like any deploy.

use super::store;
use crate::cloudflare::Client;
use crate::deploy::{self, Flow, Options, Ready, Runtime};
use crate::setup::{Context, Failure, Run};
use crate::state::{self, DeployRecord, push_capped};
use crate::text::{Code, Text};
use plumblog_i18n::key;
use std::io::Write;
use std::path::PathBuf;

/// `blog theme push`.
#[derive(Clone, Debug, Default)]
pub struct PushOptions {
    /// The theme folder; `None` with `--default`: back to the embedded default theme.
    pub dir: Option<PathBuf>,
    /// Bundle source, as for `blog deploy` (`export`, `allow_older` and `theme` unused).
    pub source: Options,
}

/// `blog theme rollback`.
#[derive(Clone, Debug, Default)]
pub struct RollbackOptions {
    /// A version made by an earlier theme push (or `--default` push).
    pub to: Option<String>,
    pub allow_older: bool,
}

const PUSH: &str = "theme_push";
const DEFAULT: &str = "theme_default";

fn is_theme_record(d: &DeployRecord) -> bool {
    matches!(d.kind.as_deref(), Some(PUSH | DEFAULT))
}

/// `blog theme push <dir>` or `blog theme push --default`.
///
/// # Errors
/// [`Failure`] with a stable code: the check's errors (before any call), pending migrations,
/// or anything `blog deploy` can fail with.
pub fn push(
    client: &mut Client,
    account_id: Option<&str>,
    opts: &PushOptions,
    cx: &Context,
    rt: &Runtime,
    out: &mut dyn Write,
) -> Result<(), Failure> {
    let text = cx.text;
    // Checked and built before the lock and any call: a theme with errors never goes out.
    let built = match &opts.dir {
        Some(dir) => {
            let report = super::check_theme(text, dir)
                .map_err(|(code, message)| Failure { code, message })?;
            let _ = write!(out, "{report}");
            let b = store::build(dir).map_err(|(path, reason)| {
                Failure::new(
                    Code::ThemeInvalid,
                    text,
                    key!("cli.error.theme_invalid"),
                    &[("path", path), ("reason", reason)],
                )
            })?;
            Some(b)
        }
        None => None,
    };
    deploy::prepared(
        client,
        account_id,
        &opts.source,
        cx,
        rt,
        out,
        &mut |run, r| ship(run, r, built.as_ref(), rt),
    )
}

fn ship(
    run: &mut Run,
    r: Ready,
    built: Option<&store::Built>,
    rt: &Runtime,
) -> Result<(), Failure> {
    let cx = run.cx;
    let Ready {
        site,
        db,
        mut b,
        before,
        pending,
    } = r;
    if pending {
        let list: Vec<String> = b
            .migrations
            .iter()
            .filter(|m| !before.contains(&m.name))
            .map(|m| m.name.clone())
            .collect();
        return Err(Failure::new(
            Code::ThemePushPending,
            cx.text,
            key!("cli.error.theme_push_pending"),
            &[("list", list.join(", "))],
        ));
    }
    let targets = deploy::targets(run, &site, rt)?;
    let theme = match built {
        Some(t) => {
            store::save(cx.dir, t)
                .map_err(|(path, reason)| deploy::theme_unwritable(run, path, reason))?;
            Some(deploy::attach_theme(run, &mut b, t)?)
        }
        None => None,
    };
    match &theme {
        Some(t) => run.line(
            key!("cli.theme.push.pushing"),
            &[
                ("name", t.name.clone()),
                ("version", t.version.clone()),
                ("hash", store::short(&t.hash).to_string()),
            ],
        ),
        None => run.line(key!("cli.theme.push.default"), &[]),
    }
    let (at, stamp) = deploy::utc(cx.now_ms);
    let mut rec = DeployRecord {
        at,
        outcome: "started".into(),
        bundle_migrations: Some(b.migrations.iter().map(|m| m.name.clone()).collect()),
        kind: Some(if theme.is_some() { PUSH } else { DEFAULT }.into()),
        theme,
        ..DeployRecord::default()
    };
    run.update(|s| push_capped(&mut s.deploys, rec.clone()))?;
    let flow = Flow {
        rt,
        targets,
        stamp,
        before,
        pending: false,
        export: false,
    };
    let result = deploy::release(run, &site, &b, &mut rec, &flow);
    deploy::finish(run, &db, &mut rec, result)
}

/// `blog theme rollback [--to <version>]`: the version before the latest theme push, or a
/// version an earlier theme push made, through `blog rollback`'s path and guards.
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
    let _lock = deploy::Lock::take(cx)?;
    let (st, site) = deploy::start(client, account_id, cx)?;
    let client: &Client = client;
    let mut run = Run {
        client,
        cx,
        out,
        state: st,
        secrets: Vec::new(),
    };
    let target = target(&run, opts.to.as_deref())?;
    deploy::refuse_placeholder(&run, &site, &target)?;
    deploy::rollback_to(&mut run, &site, target.clone(), opts.allow_older, rt)?;
    let name = deploy::theme_binding(client, &site.worker, &target).map_err(|e| run.api(&e))?;
    let rec = run
        .state
        .deploys
        .iter()
        .rev()
        .find(|d| d.version.as_deref() == Some(target.as_str()))
        .and_then(|d| d.theme.clone());
    match name {
        Some(name) => {
            let (version, hash) = rec.filter(|t| t.name == name).map_or_else(
                || ("?".into(), "?".into()),
                |t| (t.version, store::short(&t.hash).to_string()),
            );
            run.line(
                key!("cli.theme.rollback.active"),
                &[("name", name), ("version", version), ("hash", hash)],
            );
        }
        None => run.line(key!("cli.theme.rollback.default"), &[]),
    }
    Ok(())
}

fn target(run: &Run, to: Option<&str>) -> Result<String, Failure> {
    let cx = run.cx;
    let deploys = &run.state.deploys;
    let current = |d: &&DeployRecord| deploy::CURRENT_OUTCOMES.contains(&d.outcome.as_str());
    match to {
        Some(v) => {
            let known = deploys.iter().filter(current).any(|d| {
                d.version.as_deref() == Some(v) && (is_theme_record(d) || d.theme.is_some())
            });
            if known {
                return Ok(v.to_string());
            }
            let list: Vec<String> = deploys
                .iter()
                .rev()
                .filter(current)
                .filter(|d| is_theme_record(d) || d.theme.is_some())
                .filter_map(|d| d.version.clone())
                .take(5)
                .collect();
            Err(Failure::new(
                Code::ThemeRollbackUnknown,
                cx.text,
                key!("cli.error.theme_rollback_unknown"),
                &[
                    ("version", crate::setup::printable(v)),
                    ("list", list.join(", ")),
                ],
            ))
        }
        None => deploys
            .iter()
            .rev()
            .filter(current)
            .find(|d| is_theme_record(d))
            .and_then(|d| d.previous.clone())
            .ok_or_else(|| {
                Failure::new(
                    Code::ThemeRollbackNoRecord,
                    cx.text,
                    key!("cli.error.theme_rollback_no_record"),
                    &[("path", state::path(cx.dir).display().to_string())],
                )
            }),
    }
}

/// `blog theme list`: theme pushes and the theme of each recorded version, newest first,
/// from `site.local.toml` (no call).
///
/// # Errors
/// The state file is unreadable.
pub fn list(text: &Text, dir: &std::path::Path) -> Result<String, (Code, String)> {
    let st = state::load(dir)
        .map_err(|e| {
            let f = crate::setup::state_failure(&e, text);
            (f.code, f.message)
        })?
        .unwrap_or_default();
    let mut out = String::new();
    for d in st.deploys.iter().rev() {
        let Some(version) = &d.version else {
            continue;
        };
        let theme = d.theme.as_ref().map_or_else(
            || text.t(key!("cli.theme.list.default"), &[]),
            |t| format!("{} {} ({})", t.name, t.version, store::short(&t.hash)),
        );
        let what = if is_theme_record(d) {
            key!("cli.theme.list.push")
        } else {
            key!("cli.theme.list.deploy")
        };
        out.push_str(&text.t(
            what,
            &[
                ("at", d.at.clone()),
                ("version", version.clone()),
                ("outcome", d.outcome.clone()),
                ("theme", theme),
            ],
        ));
        out.push('\n');
    }
    if out.is_empty() {
        out = text.t(key!("cli.theme.list.empty"), &[]) + "\n";
    }
    Ok(out)
}
