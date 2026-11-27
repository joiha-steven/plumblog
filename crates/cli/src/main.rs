use clap::{CommandFactory, Parser, Subcommand};
use std::{fmt::Write, process::ExitCode};

include!(concat!(env!("OUT_DIR"), "/guides.rs"));

/// `(topic, summary, build)`.
type Generated = (&'static str, &'static str, fn() -> String);

/// References built from code at run time, so they can never drift from what the blog
/// accepts.
const GENERATED: &[Generated] = &[(
    "options",
    "Theme and plugin option schema (generated from code)",
    plumblog_render::options::reference,
)];

#[derive(Parser)]
#[command(name = "blog", version, about = "plumblog command line")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// List guides, or print one by topic
    Guide {
        /// Guide topic (file name without .md)
        topic: Option<String>,
    },
}

fn guide_list() -> String {
    let mut out = String::from("Guides (run `blog guide <topic>`):\n");
    for (name, summary, _) in GUIDES {
        writeln!(out, "  {name:<20} {summary}").unwrap();
    }
    for (name, summary, _) in GENERATED {
        writeln!(out, "  {name:<20} {summary}").unwrap();
    }
    out
}

fn guide_output(topic: Option<&str>) -> Result<String, String> {
    match topic {
        None => Ok(guide_list()),
        Some(t) => GUIDES
            .iter()
            .find(|(name, _, _)| *name == t)
            .map(|(_, _, text)| (*text).to_string())
            .or_else(|| {
                GENERATED
                    .iter()
                    .find(|(name, _, _)| *name == t)
                    .map(|(_, _, build)| build())
            })
            .ok_or_else(|| format!("unknown guide topic: {t}\n{}", guide_list())),
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Some(Command::Guide { topic }) => match guide_output(topic.as_deref()) {
            Ok(text) => {
                print!("{text}");
                ExitCode::SUCCESS
            }
            Err(msg) => {
                eprint!("error: {msg}");
                ExitCode::FAILURE
            }
        },
        None => {
            // Bare `blog` shows help instead of silently doing nothing.
            let _ = Cli::command().print_help();
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;
    use std::path::Path;

    #[test]
    fn unknown_topic_errors_and_lists() {
        let err = guide_output(Some("no-such-topic")).unwrap_err();
        assert!(err.contains("unknown guide topic: no-such-topic"));
        assert!(err.contains("getting-started"));
    }

    #[test]
    fn options_reference_is_generated_from_the_schema() {
        let text = guide_output(Some("options")).unwrap();
        assert_eq!(text, plumblog_render::options::reference());
        for t in plumblog_render::options::FieldType::ALL {
            assert!(
                text.contains(&format!("### `{}`", t.name())),
                "{}",
                t.name()
            );
        }
        assert!(guide_list().contains("options"));
    }

    #[test]
    fn known_topic_prints_guide() {
        assert!(
            guide_output(Some("getting-started"))
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
                        let accepted = match Cli::command().try_get_matches_from(&argv) {
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
}
