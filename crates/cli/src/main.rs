use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use plumblog_cli::cloudflare::{Client, Credentials, TokenError, TokenKind, endpoint, token};
use plumblog_cli::help;
use plumblog_cli::text::{Code, Text, pick_lang};
use plumblog_i18n::{Key, key};
use std::{fmt::Write, path::Path, process::ExitCode};

include!(concat!(env!("OUT_DIR"), "/guides.rs"));

/// `(topic, summary key, build)`.
type Generated = (&'static str, Key, fn(&Text) -> String);

/// References built from code at run time, so they can never drift from what the blog
/// accepts.
const GENERATED: &[Generated] = &[
    ("options", key!("cli.guide.options_summary"), |_| {
        plumblog_render::options::reference()
    }),
    (
        "token",
        key!("cli.guide.token_summary"),
        endpoint::token_guide,
    ),
];

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
        #[arg(long, required = true)]
        check_token: bool,
    },
}

/// clap takes value names only as `&'static str` without its `string` feature; a few bytes,
/// once per run.
fn leak(s: String) -> &'static str {
    s.leak()
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

fn run(text: &Text, args: &[String]) -> Outcome {
    let has = |flags: &[&str]| args.iter().skip(1).any(|a| flags.contains(&a.as_str()));
    let cmd = command(text);
    // Before parsing: `blog setup --help` must show help, not "missing --check-token".
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
        Some(Command::Setup { check_token: _ }) => check_token(text),
        None => return Outcome::Help(help::render(&cmd, &[], text)),
    };
    match result {
        Ok(out) => Outcome::Out(out),
        Err((code, msg)) => Outcome::Fail(code, msg),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
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
                            // A `<placeholder>` stands for any topic; real names must exist.
                            assert!(
                                topic.starts_with('<') || GUIDES.iter().any(|g| g.0 == topic),
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
                    &["blog", "setup"][..],
                    key!("cli.args.missing_required"),
                    "--check-token",
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
                h.contains("--check-token") && h.contains(&text.t(key!("cli.setup.about"), &[]))
            );
            assert!(matches!(run(&text, &argv(&["blog"])), Outcome::Help(_)));
        }
        let Outcome::Out(v) = run(&en(), &argv(&["blog", "-V"])) else {
            panic!("-V");
        };
        assert_eq!(v.trim(), format!("blog {}", env!("CARGO_PKG_VERSION")));
    }
}
