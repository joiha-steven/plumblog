use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use plumblog_cli::cloudflare::{Client, Credentials, TokenError, TokenKind, endpoint, token};
use plumblog_cli::help;
use plumblog_cli::release::{self, Version, fetch::Source};
use plumblog_cli::text::{Code, Text, pick_lang};
use plumblog_cli::{admin, backup, deploy, migrations, restore, setup, theme};
use plumblog_i18n::{Key, key};
use std::{fmt::Write, path::Path, process::ExitCode};

include!(concat!(env!("OUT_DIR"), "/guides.rs"));

/// `(topic, summary key, build)`.
type Generated = (&'static str, Key, fn(&Text) -> String);

/// References built from code at run time, so they can never drift from what the blog
/// accepts.
const GENERATED: &[Generated] = &[
    ("api", key!("cli.guide.api_summary"), |_| api_reference()),
    ("options", key!("cli.guide.options_summary"), |_| {
        plumblog_render::options::reference()
    }),
    (
        "token",
        key!("cli.guide.token_summary"),
        endpoint::token_guide,
    ),
];

/// `blog guide api`: the Worker's `/api/v1` route table, the same data that routes requests
/// and answers `GET /api/v1/reference`.
fn api_reference() -> String {
    use plumblog_model::api::{Access, MAX_LIMIT, ROUTES};
    let mut o = format!(
        "# HTTP API v1\n\n\
         JSON over HTTPS. Send `Authorization: Bearer plb_...` (an API token) or use the admin \
         session. Errors: `{{\"error\": \"<code>\", \"field\": \"<name>\"}}` (`field` only for input \
         errors). Lists page with an opaque `cursor` (`next_cursor` of the previous page) and \
         `limit` <= {MAX_LIMIT}. Integers only, no floats. `GET /api/v1/reference` serves this \
         table as JSON.\n"
    );
    for r in ROUTES {
        let access = match r.access {
            Access::Read => "read".to_owned(),
            Access::Require(p) => p.as_str().to_owned(),
        };
        let _ = write!(
            o,
            "\n## {} {}\n\naccess: {access}\n",
            r.method.as_str(),
            r.path
        );
        if !r.also.is_empty() {
            let also: Vec<&str> = r.also.iter().map(|p| p.as_str()).collect();
            let _ = writeln!(o, "also, when the input calls for it: {}", also.join(", "));
        }
        if !r.session_only_keys.is_empty() {
            let _ = writeln!(
                o,
                "session only (API tokens get 403) for keys: {}",
                r.session_only_keys.join(", ")
            );
        }
        let _ = writeln!(
            o,
            "body: {}, limit {} bytes\nanswer: {} {}",
            r.body.as_str(),
            r.max_body,
            r.status,
            r.response
        );
        if let Some(v) = r.variants {
            let _ = writeln!(o, "variants: {}", v.as_str());
        }
        for f in r.fields {
            let need = if f.required { "required" } else { "optional" };
            let _ = writeln!(
                o,
                "- `{}` ({}, {}, {need})",
                f.name,
                f.ty.as_str(),
                f.at.as_str()
            );
        }
    }
    o
}

// Help text is set from locale keys in `command()`, never from doc comments here: clap would
// print doc comments as English help.
// clap's help and version flags and help subcommand print English, so ours replace them.
#[derive(Parser)]
#[command(
    name = "blog",
    disable_help_flag = true,
    disable_version_flag = true,
    disable_help_subcommand = true
)]
struct Cli {
    #[arg(long, global = true)]
    lang: Option<String>,
    // Handled before parsing (see `main`); declared so help lists them.
    #[arg(short = 'h', long, global = true)]
    help: bool,
    #[arg(short = 'V', long)]
    version: bool,
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    Guide {
        topic: Option<String>,
    },
    Setup {
        #[arg(long, conflicts_with_all = ["name", "domain", "no_turnstile", "change_origin", "clear_failed_migration"])]
        check_token: bool,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        domain: Option<String>,
        #[arg(long)]
        no_turnstile: bool,
        #[arg(long, requires = "no_turnstile")]
        yes_i_understand: bool,
        #[arg(long)]
        change_origin: bool,
        #[arg(long)]
        clear_failed_migration: bool,
    },
    Deploy {
        #[arg(long)]
        export: bool,
        #[arg(long, conflicts_with = "build")]
        bundle: Option<std::path::PathBuf>,
        #[arg(long)]
        build: bool,
        #[arg(long)]
        allow_older: bool,
        #[arg(long)]
        theme_default: bool,
    },
    Backup {
        #[arg(long)]
        no_media: bool,
        #[arg(long)]
        out: Option<std::path::PathBuf>,
    },
    Restore {
        #[arg(required_unless_present = "reset_credentials")]
        file: Option<std::path::PathBuf>,
        #[arg(long)]
        yes: bool,
        #[arg(
            long,
            requires = "yes",
            conflicts_with_all = ["file", "bundle", "build", "allow_other_site"]
        )]
        reset_credentials: bool,
        #[arg(long, conflicts_with = "build")]
        bundle: Option<std::path::PathBuf>,
        #[arg(long)]
        build: bool,
        #[arg(long)]
        allow_other_site: bool,
    },
    Update {
        #[arg(long, conflicts_with = "check")]
        to: Option<String>,
        #[arg(long)]
        check: bool,
        #[arg(long)]
        allow_older: bool,
    },
    Rollback {
        #[arg(long, conflicts_with = "db")]
        to: Option<String>,
        #[arg(long)]
        db: Option<String>,
        #[arg(long, requires = "db")]
        yes: bool,
        #[arg(long, requires = "db")]
        clear_failed_migration: bool,
        #[arg(long, conflicts_with = "db")]
        allow_older: bool,
    },
    Theme {
        #[command(subcommand)]
        action: ThemeAction,
    },
    Plugin {
        #[command(subcommand)]
        action: PluginAction,
    },
    Admin {
        #[command(subcommand)]
        action: AdminAction,
    },
}

#[derive(Subcommand)]
enum AdminAction {
    Reset {
        #[arg(long)]
        login: Option<String>,
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        keep_tokens: bool,
    },
}

#[derive(Subcommand)]
enum ThemeAction {
    New {
        dir: std::path::PathBuf,
        #[arg(long)]
        name: Option<String>,
    },
    Check {
        dir: std::path::PathBuf,
    },
    Dev {
        dir: std::path::PathBuf,
        #[arg(long, default_value_t = 4321)]
        port: u16,
    },
    Push {
        #[arg(required_unless_present = "default", conflicts_with = "default")]
        dir: Option<std::path::PathBuf>,
        #[arg(long)]
        default: bool,
        #[arg(long, conflicts_with = "build")]
        bundle: Option<std::path::PathBuf>,
        #[arg(long)]
        build: bool,
    },
    Rollback {
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        allow_older: bool,
    },
    List,
}

#[derive(Subcommand)]
enum PluginAction {
    Check { dir: std::path::PathBuf },
}

/// clap takes value names only as `&'static str` without its `string` feature; a few bytes,
/// once per run.
fn leak(s: String) -> &'static str {
    s.leak()
}

/// `blog admin`'s help texts.
fn admin_help(c: clap::Command, text: &Text) -> clap::Command {
    let t = |k: Key| text.t(k, &[]);
    c.about(t(key!("cli.admin.about")))
        .mut_subcommand("reset", |c| {
            c.about(t(key!("cli.admin.reset_about")))
                .mut_arg("login", |a| {
                    a.help(t(key!("cli.admin.login_help")))
                        .value_name(leak(t(key!("cli.admin.login_value"))))
                })
                .mut_arg("yes", |a| a.help(t(key!("cli.admin.yes_help"))))
                .mut_arg("keep_tokens", |a| {
                    a.help(t(key!("cli.admin.keep_tokens_help")))
                })
        })
}

/// `blog restore`'s help texts.
fn restore_help(c: clap::Command, text: &Text) -> clap::Command {
    let t = |k: Key| text.t(k, &[]);
    c.about(t(key!("cli.restore.about")))
        .mut_arg("file", |a| {
            a.help(t(key!("cli.restore.file_help")))
                .value_name(leak(t(key!("cli.restore.file_value"))))
        })
        .mut_arg("yes", |a| a.help(t(key!("cli.restore.yes_help"))))
        .mut_arg("bundle", |a| {
            a.help(t(key!("cli.restore.bundle_help")))
                .value_name(leak(t(key!("cli.deploy.bundle_value"))))
        })
        .mut_arg("build", |a| a.help(t(key!("cli.restore.build_help"))))
        .mut_arg("allow_other_site", |a| {
            a.help(t(key!("cli.restore.allow_other_site_help")))
        })
        .mut_arg("reset_credentials", |a| {
            a.help(t(key!("cli.restore.reset_credentials_help")))
        })
}

/// `blog theme`'s help texts.
fn theme_help(c: clap::Command, text: &Text) -> clap::Command {
    let t = |k: Key| text.t(k, &[]);

    c.about(t(key!("cli.theme.about")))
        .mut_subcommand("new", |c| {
            c.about(t(key!("cli.theme.new.about")))
                .mut_arg("dir", |a| {
                    a.help(t(key!("cli.theme.new.dir_help")))
                        .value_name(leak(t(key!("cli.theme.dir_value"))))
                })
                .mut_arg("name", |a| {
                    a.help(t(key!("cli.theme.new.name_help")))
                        .value_name(leak(t(key!("cli.theme.name_value"))))
                })
        })
        .mut_subcommand("check", |c| {
            c.about(t(key!("cli.theme.check.about")))
                .mut_arg("dir", |a| {
                    a.help(t(key!("cli.theme.check.dir_help")))
                        .value_name(leak(t(key!("cli.theme.dir_value"))))
                })
        })
        .mut_subcommand("dev", |c| {
            c.about(t(key!("cli.theme.dev.about")))
                .mut_arg("dir", |a| {
                    a.help(t(key!("cli.theme.dev.dir_help")))
                        .value_name(leak(t(key!("cli.theme.dir_value"))))
                })
                .mut_arg("port", |a| {
                    a.help(t(key!("cli.theme.dev.port_help")))
                        .value_name(leak(t(key!("cli.theme.port_value"))))
                })
        })
        .mut_subcommand("push", |c| {
            c.about(t(key!("cli.theme.push.about")))
                .mut_arg("dir", |a| {
                    a.help(t(key!("cli.theme.push.dir_help")))
                        .value_name(leak(t(key!("cli.theme.dir_value"))))
                })
                .mut_arg("default", |a| {
                    a.help(t(key!("cli.theme.push.default_help")))
                })
                .mut_arg("bundle", |a| {
                    a.help(t(key!("cli.deploy.bundle_help")))
                        .value_name(leak(t(key!("cli.deploy.bundle_value"))))
                })
                .mut_arg("build", |a| a.help(t(key!("cli.deploy.build_help"))))
        })
        .mut_subcommand("rollback", |c| {
            c.about(t(key!("cli.theme.rollback.about")))
                .mut_arg("to", |a| {
                    a.help(t(key!("cli.theme.rollback.to_help")))
                        .value_name(leak(t(key!("cli.rollback.to_value"))))
                })
                .mut_arg("allow_older", |a| {
                    a.help(t(key!("cli.rollback.allow_older_help")))
                })
        })
        .mut_subcommand("list", |c| c.about(t(key!("cli.theme.list.about"))))
}

/// `blog plugin`'s help texts.
fn plugin_help(c: clap::Command, text: &Text) -> clap::Command {
    let t = |k: Key| text.t(k, &[]);

    c.about(t(key!("cli.plugin.about")))
        .mut_subcommand("check", |c| {
            c.about(t(key!("cli.plugin.check.about")))
                .mut_arg("dir", |a| {
                    a.help(t(key!("cli.plugin.check.dir_help")))
                        .value_name(leak(t(key!("cli.plugin.dir_value"))))
                })
        })
}

/// The clap command with every help text in `text`'s language.
fn command(text: &Text) -> clap::Command {
    let t = |k: Key| text.t(k, &[]);
    Cli::command()
        .about(t(key!("cli.about")))
        .mut_arg("lang", |a| {
            a.help(t(key!("cli.lang_help")))
                .value_name(leak(t(key!("cli.lang_value"))))
        })
        .mut_arg("help", |a| a.help(t(key!("cli.help.help_help"))))
        .mut_arg("version", |a| a.help(t(key!("cli.help.version_help"))))
        .mut_subcommand("guide", |c| {
            c.about(t(key!("cli.guide.about"))).mut_arg("topic", |a| {
                a.help(t(key!("cli.guide.topic_help")))
                    .value_name(leak(t(key!("cli.guide.topic_value"))))
            })
        })
        .mut_subcommand("setup", |c| {
            c.about(t(key!("cli.setup.about")))
                .mut_arg("check_token", |a| {
                    a.help(t(key!("cli.setup.check_token_help")))
                })
                .mut_arg("name", |a| {
                    a.help(t(key!("cli.setup.name_help")))
                        .value_name(leak(t(key!("cli.setup.name_value"))))
                })
                .mut_arg("domain", |a| {
                    a.help(t(key!("cli.setup.domain_help")))
                        .value_name(leak(t(key!("cli.setup.domain_value"))))
                })
                .mut_arg("no_turnstile", |a| {
                    a.help(t(key!("cli.setup.no_turnstile_help")))
                })
                .mut_arg("yes_i_understand", |a| {
                    a.help(t(key!("cli.setup.yes_help")))
                })
                .mut_arg("change_origin", |a| {
                    a.help(t(key!("cli.setup.change_origin_help")))
                })
                .mut_arg("clear_failed_migration", |a| {
                    a.help(t(key!("cli.setup.clear_failed_migration_help")))
                })
        })
        .mut_subcommand("deploy", |c| {
            c.about(t(key!("cli.deploy.about")))
                .mut_arg("export", |a| a.help(t(key!("cli.deploy.export_help"))))
                .mut_arg("bundle", |a| {
                    a.help(t(key!("cli.deploy.bundle_help")))
                        .value_name(leak(t(key!("cli.deploy.bundle_value"))))
                })
                .mut_arg("build", |a| a.help(t(key!("cli.deploy.build_help"))))
                .mut_arg("allow_older", |a| {
                    a.help(t(key!("cli.deploy.allow_older_help")))
                })
                .mut_arg("theme_default", |a| {
                    a.help(t(key!("cli.deploy.theme_default_help")))
                })
        })
        .mut_subcommand("backup", |c| {
            c.about(t(key!("cli.backup.about")))
                .mut_arg("no_media", |a| a.help(t(key!("cli.backup.no_media_help"))))
                .mut_arg("out", |a| {
                    a.help(t(key!("cli.backup.out_help")))
                        .value_name(leak(t(key!("cli.backup.out_value"))))
                })
        })
        .mut_subcommand("restore", |c| restore_help(c, text))
        .mut_subcommand("update", |c| {
            c.about(t(key!("cli.update.about")))
                .mut_arg("to", |a| {
                    a.help(t(key!("cli.update.to_help")))
                        .value_name(leak(t(key!("cli.update.to_value"))))
                })
                .mut_arg("check", |a| a.help(t(key!("cli.update.check_help"))))
                .mut_arg("allow_older", |a| {
                    a.help(t(key!("cli.update.allow_older_help")))
                })
        })
        .mut_subcommand("theme", |c| theme_help(c, text))
        .mut_subcommand("plugin", |c| plugin_help(c, text))
        .mut_subcommand("admin", |c| admin_help(c, text))
        .mut_subcommand("rollback", |c| {
            c.about(t(key!("cli.rollback.about")))
                .mut_arg("to", |a| {
                    a.help(t(key!("cli.rollback.to_help")))
                        .value_name(leak(t(key!("cli.rollback.to_value"))))
                })
                .mut_arg("db", |a| {
                    a.help(t(key!("cli.rollback.db_help")))
                        .value_name(leak(t(key!("cli.rollback.db_value"))))
                })
                .mut_arg("yes", |a| a.help(t(key!("cli.rollback.yes_help"))))
                .mut_arg("clear_failed_migration", |a| {
                    a.help(t(key!("cli.rollback.clear_failed_help")))
                })
                .mut_arg("allow_older", |a| {
                    a.help(t(key!("cli.rollback.allow_older_help")))
                })
        })
}

/// `--lang` from the raw arguments, read before clap so its own messages use the language.
fn lang_arg(args: &[String]) -> Option<String> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--lang" {
            return it.next().cloned();
        }
        if let Some(v) = a.strip_prefix("--lang=") {
            return Some(v.to_string());
        }
    }
    None
}

fn guide_list(text: &Text) -> String {
    let mut out = text.t(key!("cli.guide.list_head"), &[]);
    out.push('\n');
    for (name, summary, _) in GUIDES {
        writeln!(out, "  {name:<20} {summary}").unwrap();
    }
    for (name, summary, _) in GENERATED {
        writeln!(out, "  {name:<20} {}", text.t(*summary, &[])).unwrap();
    }
    out
}

/// Guides (`guides/*.md`) are AI instructions and stay English; generated ones follow `text`.
fn guide_output(text: &Text, topic: Option<&str>) -> Result<String, (Code, String)> {
    match topic {
        None => Ok(guide_list(text)),
        Some(t) => GUIDES
            .iter()
            .find(|(name, _, _)| *name == t)
            .map(|(_, _, body)| (*body).to_string())
            .or_else(|| {
                GENERATED
                    .iter()
                    .find(|(name, _, _)| *name == t)
                    .map(|(_, _, build)| build(text))
            })
            .ok_or_else(|| {
                let msg = text.t(key!("cli.error.guide_unknown"), &[("topic", t.to_string())]);
                (Code::GuideUnknown, format!("{msg}\n{}", guide_list(text)))
            }),
    }
}

/// `blog setup --check-token`: credentials from env or `.env` in the current folder.
fn check_token(text: &Text) -> Result<String, (Code, String)> {
    let fail = |e: &TokenError| (e.code(), e.message(text));
    let creds = Credentials::load(Path::new(".")).map_err(|e| fail(&e))?;
    let mut client = Client::from_env(creds.token).map_err(|e| (e.code(), e.message(text)))?;
    let v = token::verify(&mut client, creds.account_id.as_deref()).map_err(|e| fail(&e))?;
    let kind = text.t(
        match v.kind {
            TokenKind::User => key!("cli.setup.kind_user"),
            TokenKind::Account => key!("cli.setup.kind_account"),
        },
        &[],
    );
    let ok = match v.expires_on {
        Some(date) => text.t(
            key!("cli.setup.token_ok_expires"),
            &[("kind", kind), ("date", date)],
        ),
        None => text.t(key!("cli.setup.token_ok"), &[("kind", kind)]),
    };
    // Names come from Cloudflare; no terminal control characters.
    let name: String = v.account.name.chars().filter(|c| !c.is_control()).collect();
    let account = text.t(
        key!("cli.setup.account"),
        &[("name", name), ("id", v.account.id)],
    );
    Ok(format!("{ok}\n{account}\n"))
}

/// `blog setup`: progress goes to stdout as each step finishes, so a failure shows what
/// was already done.
fn setup_site(text: &Text, opts: &setup::Options) -> Result<String, (Code, String)> {
    let fail = |f: setup::Failure| (f.code, f.message);
    setup::check_options(opts, text).map_err(fail)?;
    let creds = Credentials::load(Path::new(".")).map_err(|e| (e.code(), e.message(text)))?;
    let mut client = Client::from_env(creds.token).map_err(|e| (e.code(), e.message(text)))?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let migrations = migrations::bundled();
    let cx = setup::Context {
        dir: Path::new("."),
        migrations: &migrations,
        now_ms,
        text,
    };
    let mut out = std::io::stdout().lock();
    setup::run(
        &mut client,
        creds.account_id.as_deref(),
        opts,
        &cx,
        &mut out,
    )
    .map_err(fail)?;
    Ok(String::new())
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// The commands that make or switch Worker versions.
enum Versions {
    Deploy(deploy::Options),
    Rollback(deploy::RollbackOptions),
    ThemePush(theme::push::PushOptions),
    ThemeRollback(theme::push::RollbackOptions),
}

/// `blog deploy`, `blog rollback`, `blog theme push | rollback`: progress to stdout as each
/// step finishes.
fn deploy_or_rollback(text: &Text, action: Versions) -> Result<String, (Code, String)> {
    let creds = Credentials::load(Path::new(".")).map_err(|e| (e.code(), e.message(text)))?;
    let mut client = Client::from_env(creds.token).map_err(|e| (e.code(), e.message(text)))?;
    let migrations = migrations::bundled();
    let cx = setup::Context {
        dir: Path::new("."),
        migrations: &migrations,
        now_ms: now_ms(),
        text,
    };
    let clock = deploy::RealClock::new();
    let rt = deploy::Runtime {
        clock: &clock,
        smoke_base: std::env::var(deploy::SMOKE_BASE_ENV)
            .ok()
            .filter(|v| !v.is_empty()),
        smoke_budget: deploy::SMOKE_BUDGET,
    };
    let mut out = std::io::stdout().lock();
    let account = creds.account_id.as_deref();
    let baked = |opts: &mut deploy::Options| -> Result<(), (Code, String)> {
        if opts.bundle.is_none() && !opts.build {
            opts.release = release::baked(text).map_err(|f| (f.code, f.message))?;
        }
        Ok(())
    };
    match action {
        Versions::Deploy(mut opts) => {
            baked(&mut opts)?;
            deploy::run(&mut client, account, &opts, &cx, &rt, &mut out)
        }
        Versions::Rollback(opts) => {
            deploy::rollback(&mut client, account, &opts, &cx, &rt, &mut out)
        }
        Versions::ThemePush(mut opts) => {
            baked(&mut opts.source)?;
            theme::push::push(&mut client, account, &opts, &cx, &rt, &mut out)
        }
        Versions::ThemeRollback(opts) => {
            theme::push::rollback(&mut client, account, &opts, &cx, &rt, &mut out)
        }
    }
    .map_err(|f| (f.code, f.message))?;
    Ok(String::new())
}

/// `blog restore`: progress to stdout as each step finishes.
fn restore(text: &Text, mut opts: restore::Options) -> Result<String, (Code, String)> {
    let creds = Credentials::load(Path::new(".")).map_err(|e| (e.code(), e.message(text)))?;
    let mut client = Client::from_env(creds.token).map_err(|e| (e.code(), e.message(text)))?;
    let migrations = migrations::bundled();
    let cx = setup::Context {
        dir: Path::new("."),
        migrations: &migrations,
        now_ms: now_ms(),
        text,
    };
    if opts.bundle.is_none() && !opts.build {
        opts.release = release::baked(text).map_err(|f| (f.code, f.message))?;
    }
    let clock = deploy::RealClock::new();
    let mut out = std::io::stdout().lock();
    restore::run(
        &mut client,
        creds.account_id.as_deref(),
        &opts,
        &cx,
        &clock,
        &mut out,
    )
    .map_err(|f| (f.code, f.message))?;
    Ok(String::new())
}

/// `blog restore --reset-credentials --yes`: only the restore's credential reset.
fn restore_reset(text: &Text) -> Result<String, (Code, String)> {
    let creds = Credentials::load(Path::new(".")).map_err(|e| (e.code(), e.message(text)))?;
    let mut client = Client::from_env(creds.token).map_err(|e| (e.code(), e.message(text)))?;
    let migrations = migrations::bundled();
    let cx = setup::Context {
        dir: Path::new("."),
        migrations: &migrations,
        now_ms: now_ms(),
        text,
    };
    let mut out = std::io::stdout().lock();
    restore::reset_credentials(&mut client, creds.account_id.as_deref(), &cx, &mut out)
        .map_err(|f| (f.code, f.message))?;
    Ok(String::new())
}

/// `blog admin reset`: the plan, then the code, to stdout.
fn admin_reset(text: &Text, action: AdminAction) -> Result<String, (Code, String)> {
    let AdminAction::Reset {
        login,
        yes,
        keep_tokens,
    } = action;
    let opts = &admin::Options {
        login,
        yes,
        keep_tokens,
    };
    let creds = Credentials::load(Path::new(".")).map_err(|e| (e.code(), e.message(text)))?;
    let mut client = Client::from_env(creds.token).map_err(|e| (e.code(), e.message(text)))?;
    let migrations = migrations::bundled();
    let cx = setup::Context {
        dir: Path::new("."),
        migrations: &migrations,
        now_ms: now_ms(),
        text,
    };
    let mut out = std::io::stdout().lock();
    admin::run(
        &mut client,
        creds.account_id.as_deref(),
        opts,
        &cx,
        &mut out,
    )
    .map_err(|f| (f.code, f.message))?;
    Ok(String::new())
}

/// `blog backup`: progress to stdout as each step finishes.
fn backup(text: &Text, opts: &backup::Options) -> Result<String, (Code, String)> {
    let creds = Credentials::load(Path::new(".")).map_err(|e| (e.code(), e.message(text)))?;
    let mut client = Client::from_env(creds.token).map_err(|e| (e.code(), e.message(text)))?;
    let migrations = migrations::bundled();
    let cx = setup::Context {
        dir: Path::new("."),
        migrations: &migrations,
        now_ms: now_ms(),
        text,
    };
    let mut out = std::io::stdout().lock();
    backup::run(
        &mut client,
        creds.account_id.as_deref(),
        opts,
        &cx,
        &mut out,
    )
    .map_err(|f| (f.code, f.message))?;
    Ok(String::new())
}

/// The running executable with symlinks resolved: the file update replaces and `clean_old`
/// looks beside.
fn real_exe() -> std::io::Result<std::path::PathBuf> {
    std::env::current_exe().and_then(std::fs::canonicalize)
}

/// `blog update`: the running executable, the embedded key, GitHub (or the loopback test base).
fn update(
    text: &Text,
    to: Option<&str>,
    check: bool,
    allow_older: bool,
) -> Result<String, (Code, String)> {
    let to = match to {
        Some(v) => Some(Version::parse(v).ok_or_else(|| {
            let msg = text.t(
                key!("cli.error.update_version_invalid"),
                &[("value", v.chars().filter(|c| !c.is_control()).collect())],
            );
            (Code::UpdateVersionInvalid, msg)
        })?),
        None => None,
    };
    let source = Source::from_env().map_err(|e| {
        let f = release::fetch_failure(&e, text);
        (f.code, f.message)
    })?;
    // The real file, not a symlink to it: that is what must be replaced.
    let exe = real_exe().map_err(|e| {
        let msg = text.t(key!("cli.error.internal"), &[("detail", e.to_string())]);
        (Code::Internal, msg)
    })?;
    let u = release::update::Update {
        current: Version::current(),
        target: release::TARGET.into(),
        key: release::PUBLIC_KEY.into(),
        exe,
        source,
        to,
        check,
        allow_older,
    };
    let mut out = std::io::stdout().lock();
    release::update::run(&u, text, &mut out).map_err(|f| (f.code, f.message))?;
    Ok(String::new())
}

/// Parses `args`; a clap error becomes our own line (code + sentence + the argument).
fn parse(text: &Text, args: &[String]) -> Result<Cli, (Code, String)> {
    command(text)
        .try_get_matches_from(args)
        .and_then(|m| Cli::from_arg_matches(&m))
        .map_err(|e| help::args_error(&e, text))
}

/// What a run prints, and how it ends.
enum Outcome {
    Out(String),
    /// Bare `blog`: help, but a failure, since nothing was done.
    Help(String),
    Fail(Code, String),
}

fn deploy_command(text: &Text, command: Command) -> Result<String, (Code, String)> {
    let Command::Deploy {
        export,
        bundle,
        build,
        allow_older,
        theme_default,
    } = command
    else {
        return Ok(String::new());
    };
    let theme = if theme_default {
        deploy::ThemeCarry::Drop
    } else {
        deploy::ThemeCarry::Keep
    };
    deploy_or_rollback(
        text,
        Versions::Deploy(deploy::Options {
            export,
            bundle,
            build,
            allow_older,
            release: None,
            theme,
        }),
    )
}

fn theme_command(text: &Text, command: Command) -> Result<String, (Code, String)> {
    match command {
        Command::Theme {
            action: ThemeAction::New { dir, name },
        } => theme::new(text, &dir, name.as_deref()),
        Command::Theme {
            action: ThemeAction::Check { dir },
        } => theme::check_theme(text, &dir),
        Command::Theme {
            action: ThemeAction::Dev { dir, port },
        } => theme::dev(text, &dir, port),
        Command::Theme {
            action:
                ThemeAction::Push {
                    dir,
                    default: _,
                    bundle,
                    build,
                },
        } => deploy_or_rollback(
            text,
            Versions::ThemePush(theme::push::PushOptions {
                dir,
                source: deploy::Options {
                    bundle,
                    build,
                    ..deploy::Options::default()
                },
            }),
        ),
        Command::Theme {
            action: ThemeAction::Rollback { to, allow_older },
        } => deploy_or_rollback(
            text,
            Versions::ThemeRollback(theme::push::RollbackOptions { to, allow_older }),
        ),
        Command::Theme {
            action: ThemeAction::List,
        } => theme::push::list(text, Path::new(".")),
        Command::Plugin {
            action: PluginAction::Check { dir },
        } => theme::check_plugin(text, &dir),
        _ => Ok(String::new()),
    }
}

fn run(text: &Text, args: &[String]) -> Outcome {
    let has = |flags: &[&str]| args.iter().skip(1).any(|a| flags.contains(&a.as_str()));
    let cmd = command(text);
    // Before parsing: `--help` must show help even next to an incomplete command line.
    if has(&["-h", "--help"]) {
        return Outcome::Out(help::render(&cmd, &help::target(&cmd, args), text));
    }
    if has(&["-V", "--version"]) {
        return Outcome::Out(format!(
            "{} {}\n",
            cmd.get_name(),
            env!("CARGO_PKG_VERSION")
        ));
    }
    let cli = match parse(text, args) {
        Ok(cli) => cli,
        Err((code, msg)) => return Outcome::Fail(code, msg),
    };
    let result = match cli.command {
        Some(Command::Guide { topic }) => guide_output(text, topic.as_deref()),
        Some(Command::Setup {
            check_token: true, ..
        }) => check_token(text),
        Some(Command::Setup {
            name,
            domain,
            no_turnstile,
            yes_i_understand,
            change_origin,
            clear_failed_migration,
            ..
        }) => setup_site(
            text,
            &setup::Options {
                name,
                domain,
                turnstile: setup::Turnstile::from_flags(no_turnstile, yes_i_understand),
                change_origin,
                clear_failed_migration,
            },
        ),
        Some(c @ Command::Deploy { .. }) => deploy_command(text, c),
        Some(Command::Backup { no_media, out }) => backup(text, &backup::Options { no_media, out }),
        Some(Command::Restore {
            reset_credentials: true,
            ..
        }) => restore_reset(text),
        Some(Command::Restore {
            file,
            yes,
            bundle,
            build,
            allow_other_site,
            reset_credentials: false,
        }) => restore(
            text,
            restore::Options {
                file: file.unwrap_or_default(),
                yes,
                bundle,
                build,
                release: None,
                allow_other_site,
            },
        ),
        Some(Command::Update {
            to,
            check,
            allow_older,
        }) => update(text, to.as_deref(), check, allow_older),
        Some(Command::Rollback {
            to,
            db,
            yes,
            clear_failed_migration,
            allow_older,
        }) => deploy_or_rollback(
            text,
            Versions::Rollback(deploy::RollbackOptions {
                to,
                db,
                yes,
                clear_failed_migration,
                allow_older,
            }),
        ),
        Some(c @ (Command::Theme { .. } | Command::Plugin { .. })) => theme_command(text, c),
        Some(Command::Admin { action }) => admin_reset(text, action),
        None => return Outcome::Help(help::render(&cmd, &[], text)),
    };
    match result {
        Ok(out) => Outcome::Out(out),
        Err((code, msg)) => Outcome::Fail(code, msg),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if let Ok(exe) = real_exe().or_else(|_| std::env::current_exe()) {
        release::update::clean_old(&exe);
    }
    let lang = pick_lang(lang_arg(&args).as_deref(), &|k| std::env::var(k).ok());
    let text = Text::new(&lang);
    match run(&text, &args) {
        Outcome::Out(out) => {
            print!("{out}");
            ExitCode::SUCCESS
        }
        Outcome::Help(out) => {
            print!("{out}");
            ExitCode::FAILURE
        }
        // Exit 2 for bad arguments, as clap and most CLIs do.
        Outcome::Fail(Code::ArgsInvalid, msg) => {
            eprintln!("{}", text.error_line(Code::ArgsInvalid, &msg));
            ExitCode::from(2)
        }
        Outcome::Fail(code, msg) => {
            eprintln!("{}", text.error_line(code, &msg));
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;
    use std::path::Path;

    fn en() -> Text {
        Text::new("en")
    }

    #[test]
    fn unknown_topic_errors_and_lists() {
        let (code, err) = guide_output(&en(), Some("no-such-topic")).unwrap_err();
        assert_eq!(code, Code::GuideUnknown);
        assert!(err.contains("Unknown guide topic: no-such-topic"));
        assert!(err.contains("getting-started"));
    }

    #[test]
    fn api_reference_lists_every_route() {
        let text = guide_output(&en(), Some("api")).unwrap();
        assert_eq!(
            text,
            guide_output(&en(), Some("api")).unwrap(),
            "deterministic"
        );
        for r in plumblog_model::api::ROUTES {
            let head = format!("\n## {} {}\n", r.method.as_str(), r.path);
            assert_eq!(text.matches(&head).count(), 1, "{head}");
            for f in r.fields {
                assert!(text.contains(&format!("- `{}` (", f.name)), "{}", f.name);
            }
        }
        assert_eq!(
            text.matches("\n## ").count(),
            plumblog_model::api::ROUTES.len()
        );
        assert!(text.contains(
            "body: original, limit 20971520 bytes\nanswer: 201 media\nvariants: admin_encoder\n"
        ));
        assert_eq!(
            text.matches(
                "session only (API tokens get 403) for keys: origin, turnstile_site_key\n"
            )
            .count(),
            2
        );
        assert!(guide_list(&en()).contains("api"));
    }

    #[test]
    fn options_reference_is_generated_from_the_schema() {
        let text = guide_output(&en(), Some("options")).unwrap();
        assert_eq!(text, plumblog_render::options::reference());
        for t in plumblog_render::options::FieldType::ALL {
            assert!(
                text.contains(&format!("### `{}`", t.name())),
                "{}",
                t.name()
            );
        }
        assert!(guide_list(&en()).contains("options"));
    }

    #[test]
    fn token_guide_is_generated_from_the_endpoint_table() {
        use plumblog_cli::cloudflare::{Endpoint, Permission};
        let text = guide_output(&en(), Some("token")).unwrap();
        assert_eq!(text, endpoint::token_guide(&en()));
        assert!(guide_list(&en()).contains("token"));
        assert!(text.contains("Never use the Global API Key"));
        // Every endpoint has a complete row, and every permission it needs is in the guide.
        for e in Endpoint::ALL {
            let s = e.spec();
            assert!(s.path.starts_with('/'), "{e:?}");
            assert!(
                s.doc.starts_with("https://developers.cloudflare.com/"),
                "{e:?}"
            );
            let summary = en().t(s.summary, &[]);
            assert!(!summary.starts_with("cli."), "{e:?}: missing locale text");
            if let Some(p) = s.permission {
                let (scope, group, access) = p.parts();
                let row = format!("| {} | {group} | {access} | ", scope.label());
                assert!(text.contains(&row), "{e:?}: {row}");
                assert!(text.contains(&summary), "{e:?}");
            }
        }
        // The labels the owner must pick, exactly as the dashboard shows them.
        for (p, label) in [
            (Permission::D1Edit, "Account | D1 | Edit"),
            (
                Permission::WorkersScriptsEdit,
                "Account | Workers Scripts | Edit",
            ),
            (
                Permission::WorkersR2StorageEdit,
                "Account | Workers R2 Storage | Edit",
            ),
            (Permission::TurnstileEdit, "Account | Turnstile | Edit"),
            (Permission::ZoneRead, "Zone | Zone | Read"),
        ] {
            assert_eq!(p.label(), label);
            assert!(text.contains(&format!("| {label} |")), "{label}");
        }
        // Account rows come before the custom-domain-only Zone rows.
        assert!(text.find("| Account |").unwrap() < text.find("| Zone |").unwrap());
    }

    #[test]
    fn known_topic_prints_guide() {
        assert!(
            guide_output(&en(), Some("getting-started"))
                .unwrap()
                .starts_with("# ")
        );
    }

    /// Command text of a guide line, if it is a `blog` example (optional `$ ` prompt).
    fn command_of(line: &str) -> Option<&str> {
        let t = line.trim_start();
        let t = t.strip_prefix("$ ").unwrap_or(t).trim_end();
        let rest = t.strip_prefix("blog")?;
        (rest.is_empty() || rest.starts_with(' ')).then_some(t)
    }

    /// Every `blog ...` line in a guide's fenced blocks must be a command clap accepts,
    /// and every guide must show at least one.
    #[test]
    fn guide_commands_are_real() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../guides");
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_none_or(|x| x != "md") {
                continue;
            }
            let text = std::fs::read_to_string(&path).unwrap();
            let mut fence: Option<&str> = None;
            let mut checked = 0;
            for line in text.lines() {
                let t = line.trim_start();
                let marker = ["```", "~~~"].into_iter().find(|m| t.starts_with(m));
                match (fence, marker) {
                    (None, Some(m)) => fence = Some(m),
                    (Some(open), Some(m)) if open == m => fence = None,
                    (Some(_), _) => {
                        let Some(cmd) = command_of(line) else {
                            continue;
                        };
                        let argv: Vec<&str> = cmd.split_whitespace().collect();
                        assert_eq!(argv[0], "blog");
                        let accepted = match command(&en()).try_get_matches_from(&argv) {
                            Ok(_) => true,
                            Err(e) => matches!(
                                e.kind(),
                                ErrorKind::DisplayVersion | ErrorKind::DisplayHelp
                            ),
                        };
                        assert!(accepted, "{}: not a valid command: {cmd}", path.display());
                        if let ["blog", "guide", topic] = argv[..] {
                            // A `<placeholder>` stands for any topic; real names must exist,
                            // as a file or a generated reference.
                            assert!(
                                topic.starts_with('<')
                                    || GUIDES.iter().any(|g| g.0 == topic)
                                    || GENERATED.iter().any(|g| g.0 == topic),
                                "{}: unknown guide topic in example: {cmd}",
                                path.display()
                            );
                        }
                        checked += 1;
                    }
                    _ => {}
                }
            }
            assert!(checked > 0, "{}: no `blog` command example", path.display());
        }
    }

    /// Every help text comes from the locale files, in every language: nothing clap could
    /// take from a doc comment, and nothing left as a bare key.
    #[test]
    fn help_text_comes_from_locale_keys() {
        for lang in ["en", "vi"] {
            let text = Text::new(lang);
            let mut cmd = command(&text);
            cmd.build();
            let mut seen = 0;
            let mut stack = vec![cmd];
            while let Some(c) = stack.pop() {
                if c.get_name() != "help" {
                    let about = c.get_about().map(ToString::to_string).unwrap_or_default();
                    assert!(
                        !about.is_empty() && !about.starts_with("cli."),
                        "{lang}: {}",
                        c.get_name()
                    );
                    for a in c.get_arguments() {
                        let help = a.get_help().map(ToString::to_string).unwrap_or_default();
                        assert!(
                            !help.is_empty() && !help.starts_with("cli."),
                            "{lang}: {}",
                            a.get_id()
                        );
                        seen += 1;
                    }
                }
                stack.extend(c.get_subcommands().cloned());
            }
            assert!(seen >= 3, "{lang}: {seen}");
        }
        assert_ne!(
            command(&Text::new("vi")).get_about().unwrap().to_string(),
            command(&en()).get_about().unwrap().to_string()
        );
    }

    #[test]
    fn lang_flag_is_read_before_clap() {
        let a = |v: &[&str]| v.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(
            lang_arg(&a(&["blog", "--lang", "vi", "guide"])).as_deref(),
            Some("vi")
        );
        assert_eq!(
            lang_arg(&a(&["blog", "guide", "--lang=vi"])).as_deref(),
            Some("vi")
        );
        assert_eq!(lang_arg(&a(&["blog", "guide"])), None);
        // The token guide follows the language; permission names stay the dashboard's.
        let vi = guide_output(&Text::new("vi"), Some("token")).unwrap();
        assert!(vi.contains("Global API Key") && vi.contains("| Account | D1 | Edit |"));
        assert!(vi.contains("API token Cloudflare"));
    }

    fn argv(v: &[&str]) -> Vec<String> {
        v.iter().map(ToString::to_string).collect()
    }

    /// Unknown flags and missing values give our line with the argument as data, in each
    /// language, with no clap prose; the run ends with code 2.
    #[test]
    fn bad_arguments_get_our_message_in_each_language() {
        let mut seen = Vec::new();
        for lang in ["en", "vi"] {
            let text = Text::new(lang);
            for (args, sentence, arg) in [
                (
                    &["blog", "--bogus"][..],
                    key!("cli.args.unknown"),
                    "--bogus",
                ),
                (
                    &["blog", "guide", "--lang"][..],
                    key!("cli.args.missing_value"),
                    "--lang",
                ),
                (
                    &["blog", "setup", "--yes-i-understand"][..],
                    key!("cli.args.missing_required"),
                    "--no-turnstile",
                ),
            ] {
                let Outcome::Fail(code, msg) = run(&text, &argv(args)) else {
                    panic!("{lang} {args:?}: not a failure");
                };
                assert_eq!(code, Code::ArgsInvalid);
                let head = text.t(sentence, &[("arg", String::new())]);
                let head = head
                    .split("{arg}")
                    .next()
                    .unwrap_or(&head)
                    .trim_end_matches('.');
                assert!(msg.starts_with(head.trim_end()), "{lang}: {msg}");
                assert!(msg.contains(arg), "{lang}: {msg}");
                for prose in [
                    "unexpected argument",
                    "a value is required",
                    "tip:",
                    "Usage:",
                ] {
                    assert!(!msg.contains(prose), "{lang}: clap prose in {msg}");
                }
                seen.push(msg);
            }
        }
        // English and Vietnamese differ for every case.
        assert!((0..3).all(|i| seen[i] != seen[i + 3]));
    }

    #[test]
    fn help_and_version_are_ours() {
        for lang in ["en", "vi"] {
            let text = Text::new(lang);
            let Outcome::Out(h) = run(&text, &argv(&["blog", "--help"])) else {
                panic!("--help");
            };
            for k in [
                key!("cli.about"),
                key!("cli.help.commands"),
                key!("cli.help.options"),
            ] {
                assert!(h.contains(&text.t(k, &[])), "{lang}: {h}");
            }
            assert!(h.contains("  guide  ") && h.contains("--lang") && h.contains("-h, --help"));
            // Help for a subcommand works even without its required flag.
            let Outcome::Out(h) = run(&text, &argv(&["blog", "--lang", lang, "setup", "-h"]))
            else {
                panic!("setup -h");
            };
            assert!(
                h.contains("--check-token")
                    && h.contains("--no-turnstile")
                    && h.contains(&text.t(key!("cli.setup.about"), &[]))
            );
            assert!(matches!(run(&text, &argv(&["blog"])), Outcome::Help(_)));
        }
        let Outcome::Out(v) = run(&en(), &argv(&["blog", "-V"])) else {
            panic!("-V");
        };
        assert_eq!(v.trim(), format!("blog {}", env!("CARGO_PKG_VERSION")));
    }
}
