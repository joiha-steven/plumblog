//! Help and argument errors from the clap command tree, in the owner's language. clap's own
//! help, version flag and error prose are English, so they are turned off and replaced here;
//! walking the tree keeps this help from drifting from what the parser accepts.

use crate::text::{Code, Text};
use clap::error::{ContextKind, ContextValue, ErrorKind};
use clap::{Arg, Command};
use plumblog_i18n::key;
use std::fmt::Write;

/// The command a help request is about: the subcommand names in `args`, skipping options
/// and the values they take. `args[0]` is the program.
#[must_use]
pub fn target(root: &Command, args: &[String]) -> Vec<String> {
    let mut root = root.clone();
    root.build();
    let mut path = Vec::new();
    let mut cmd = &root;
    let mut it = args.iter().skip(1);
    while let Some(a) = it.next() {
        if let Some(long) = a.strip_prefix("--") {
            let takes = cmd
                .get_arguments()
                .find(|x| x.get_long() == Some(long))
                .is_some_and(|x| x.get_action().takes_values());
            if takes {
                it.next();
            }
        } else if a.starts_with('-') {
            // A short flag or `--`: nothing to follow.
        } else if let Some(sub) = cmd.find_subcommand(a) {
            path.push(sub.get_name().to_string());
            cmd = sub;
        } else {
            break;
        }
    }
    path
}

/// `--lang <LANG>`, `-h, --help`, `[TOPIC]`.
fn arg_spec(a: &Arg) -> String {
    let value = a
        .get_value_names()
        .and_then(|v| v.first())
        .map_or_else(|| a.get_id().as_str().to_uppercase(), ToString::to_string);
    if a.is_positional() {
        return if a.is_required_set() {
            format!("<{value}>")
        } else {
            format!("[{value}]")
        };
    }
    let mut s = match a.get_short() {
        Some(c) => format!("-{c}, "),
        None => "    ".to_string(),
    };
    if let Some(long) = a.get_long() {
        let _ = write!(s, "--{long}");
    }
    if a.get_action().takes_values() {
        let _ = write!(s, " <{value}>");
    }
    s
}

fn rows(out: &mut String, heading: &str, rows: &[(String, String)]) {
    if rows.is_empty() {
        return;
    }
    // Characters, not bytes: value names may be Vietnamese.
    let width = rows
        .iter()
        .map(|(a, _)| a.chars().count())
        .max()
        .unwrap_or(0);
    let _ = write!(out, "\n{heading}\n");
    for (a, b) in rows {
        let _ = writeln!(out, "  {a:<width$}  {b}");
    }
}

/// Help for the command at `path` under `root` (`[]` is `blog` itself).
#[must_use]
pub fn render(root: &Command, path: &[String], text: &Text) -> String {
    let mut root = root.clone();
    root.build();
    let mut cmd = &root;
    let mut names = vec![root.get_name().to_string()];
    for p in path {
        if let Some(sub) = cmd.find_subcommand(p) {
            names.push(sub.get_name().to_string());
            cmd = sub;
        }
    }
    let visible: Vec<&Arg> = cmd.get_arguments().filter(|a| !a.is_hide_set()).collect();
    let mut usage = names.join(" ");
    if visible
        .iter()
        .any(|a| !a.is_positional() && !a.is_required_set())
    {
        let _ = write!(
            usage,
            " {}",
            text.t(key!("cli.help.options_placeholder"), &[])
        );
    }
    for a in visible
        .iter()
        .filter(|a| !a.is_positional() && a.is_required_set())
    {
        if let Some(long) = a.get_long() {
            let _ = write!(usage, " --{long}");
        }
    }
    for a in visible.iter().filter(|a| a.is_positional()) {
        let _ = write!(usage, " {}", arg_spec(a));
    }
    if cmd.has_subcommands() {
        let _ = write!(
            usage,
            " {}",
            text.t(key!("cli.help.command_placeholder"), &[])
        );
    }
    let mut out = cmd
        .get_about()
        .map(|a| format!("{a}\n"))
        .unwrap_or_default();
    let _ = write!(
        out,
        "\n{}\n",
        text.t(key!("cli.help.usage"), &[("usage", usage)])
    );
    let help_of = |a: &Arg| a.get_help().map(ToString::to_string).unwrap_or_default();
    let subs: Vec<(String, String)> = cmd
        .get_subcommands()
        .filter(|c| !c.is_hide_set())
        .map(|c| {
            (
                c.get_name().to_string(),
                c.get_about().map(ToString::to_string).unwrap_or_default(),
            )
        })
        .collect();
    rows(&mut out, &text.t(key!("cli.help.commands"), &[]), &subs);
    let positionals: Vec<_> = visible
        .iter()
        .filter(|a| a.is_positional())
        .map(|a| (arg_spec(a), help_of(a)))
        .collect();
    rows(
        &mut out,
        &text.t(key!("cli.help.arguments"), &[]),
        &positionals,
    );
    let options: Vec<_> = visible
        .iter()
        .filter(|a| !a.is_positional())
        .map(|a| (arg_spec(a), help_of(a)))
        .collect();
    rows(&mut out, &text.t(key!("cli.help.options"), &[]), &options);
    out
}

/// The argument clap complained about, as data: `--bogus`, `--lang <LANG>`.
fn offending(e: &clap::Error) -> String {
    for kind in [
        ContextKind::InvalidArg,
        ContextKind::InvalidSubcommand,
        ContextKind::InvalidValue,
    ] {
        match e.get(kind) {
            Some(ContextValue::String(s)) => return s.clone(),
            Some(ContextValue::Strings(v)) => return v.join(", "),
            _ => {}
        }
    }
    String::new()
}

/// A parse error as `(code, message)`: our sentence plus the offending argument, no clap prose.
#[must_use]
pub fn args_error(e: &clap::Error, text: &Text) -> (Code, String) {
    let arg = offending(e);
    let missing_value =
        matches!(e.get(ContextKind::InvalidValue), Some(ContextValue::String(v)) if v.is_empty());
    let key = match e.kind() {
        ErrorKind::UnknownArgument | ErrorKind::InvalidSubcommand => key!("cli.args.unknown"),
        ErrorKind::MissingRequiredArgument | ErrorKind::MissingSubcommand => {
            key!("cli.args.missing_required")
        }
        ErrorKind::InvalidValue if missing_value => key!("cli.args.missing_value"),
        ErrorKind::NoEquals | ErrorKind::TooFewValues | ErrorKind::WrongNumberOfValues => {
            key!("cli.args.missing_value")
        }
        ErrorKind::InvalidValue | ErrorKind::ValueValidation => key!("cli.args.invalid_value"),
        _ => key!("cli.args.not_usable"),
    };
    let sentence = text.t(key, &[("arg", arg)]);
    let hint = text.t(key!("cli.args.hint"), &[]);
    (Code::ArgsInvalid, format!("{sentence} {hint}"))
}
