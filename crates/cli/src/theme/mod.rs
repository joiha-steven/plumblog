//! `blog theme new | check | dev | push | rollback | list` and `blog plugin check` (docs/ARCHITECTURE.md, Themes).
//! Validation is the render crate's own loader, so a folder that passes loads in the Worker.

pub mod check;
pub mod contrast;
pub mod dev;
mod embed;
pub mod files;
pub mod finding;
mod hardtext;
pub mod push;
pub mod sample;
mod secrets;
pub mod store;

use crate::text::{Code, Text};
use files::DirFiles;
use plumblog_i18n::key;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

pub type Failure = (Code, String);

fn dir_failure(text: &Text, dir: &Path, e: &std::io::Error) -> Failure {
    (
        Code::ThemeDirUnreadable,
        text.t(
            key!("cli.theme.dir_unreadable"),
            &[
                ("dir", dir.display().to_string()),
                ("reason", e.to_string()),
            ],
        ),
    )
}

/// Theme and plugin names: lowercase letters, digits and `-`, starting with a letter.
fn valid_name(n: &str) -> bool {
    n.starts_with(|c: char| c.is_ascii_lowercase())
        && n.len() <= 64
        && n.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Whether `dir` sits inside a checkout of the plumblog repository itself (its workspace
/// holds `crates/cli` and `themes/minimal`): files there are tracked, and a user never edits
/// them.
fn inside_repo(dir: &Path) -> bool {
    let abs = if dir.is_absolute() {
        dir.to_path_buf()
    } else {
        std::env::current_dir().map_or_else(|_| dir.to_path_buf(), |c| c.join(dir))
    };
    // The folder may not exist yet: resolve the nearest one that does (links, `..`).
    let mut tail = PathBuf::new();
    let mut base = abs.as_path();
    let real = loop {
        if let Ok(r) = base.canonicalize() {
            break r.join(&tail);
        }
        let (Some(parent), Some(name)) = (base.parent(), base.file_name()) else {
            break abs.clone();
        };
        tail = Path::new(name).join(&tail);
        base = parent;
    };
    real.ancestors().any(|a| {
        a.join("crates/cli/Cargo.toml").is_file() && a.join("themes/minimal/theme.toml").is_file()
    })
}

/// `blog theme new`: the default theme copied into `dir` under `name`.
///
/// # Errors
/// The folder is not empty or is inside the plumblog checkout, the name is invalid, or
/// writing fails.
pub fn new(text: &Text, dir: &Path, name: Option<&str>) -> Result<String, Failure> {
    let name = match name {
        Some(n) => n.to_owned(),
        None => dir
            .canonicalize()
            .unwrap_or_else(|_| dir.to_path_buf())
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    };
    if !valid_name(&name) {
        return Err((
            Code::ThemeNameInvalid,
            text.t(key!("cli.theme.name_invalid"), &[("name", name)]),
        ));
    }
    let shown = dir.display().to_string();
    if inside_repo(dir) {
        return Err((
            Code::ThemeDirTracked,
            text.t(key!("cli.theme.dir_tracked"), &[("dir", shown)]),
        ));
    }
    match std::fs::read_dir(dir) {
        Ok(mut it) => {
            if it.next().is_some() {
                return Err((
                    Code::ThemeDirNotEmpty,
                    text.t(key!("cli.theme.dir_not_empty"), &[("dir", shown)]),
                ));
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        // A file, or a folder that cannot be read: not a place to write.
        Err(_) => {
            return Err((
                Code::ThemeDirNotEmpty,
                text.t(key!("cli.theme.dir_not_empty"), &[("dir", shown)]),
            ));
        }
    }
    let mut count = 0;
    for (rel, bytes) in embed::THEME_FILES {
        let path = rel.split('/').fold(dir.to_path_buf(), |p, c| p.join(c));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| dir_failure(text, parent, &e))?;
        }
        let body = if *rel == "theme.toml" {
            rename(std::str::from_utf8(bytes).unwrap_or_default(), &name).into_bytes()
        } else {
            bytes.to_vec()
        };
        std::fs::write(&path, body).map_err(|e| dir_failure(text, &path, &e))?;
        count += 1;
    }
    Ok(text.t(
        key!("cli.theme.new.done"),
        &[("name", name), ("dir", shown), ("count", count.to_string())],
    ) + "\n")
}

/// `theme.toml` with its name (and the header comment naming it) replaced.
fn rename(src: &str, name: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut done = false;
    for (i, line) in src.lines().enumerate() {
        if i == 0 && line.starts_with('#') {
            let _ = writeln!(
                out,
                "# {name}: started from the minimal theme with `blog theme new`"
            );
        } else if !done && line.starts_with("name = ") {
            let _ = writeln!(out, "name = {name:?}");
            done = true;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// `blog theme check`.
///
/// # Errors
/// The folder is unreadable, or the check found errors (the message lists every finding).
pub fn check_theme(text: &Text, dir: &Path) -> Result<String, Failure> {
    let files = DirFiles::read(dir).map_err(|e| dir_failure(text, dir, &e))?;
    finish(
        text,
        dir,
        &check::check_theme(&files),
        Code::ThemeCheckFailed,
    )
}

/// `blog plugin check`.
///
/// # Errors
/// As [`check_theme`].
pub fn check_plugin(text: &Text, dir: &Path) -> Result<String, Failure> {
    let files = DirFiles::read(dir).map_err(|e| dir_failure(text, dir, &e))?;
    let name = dir
        .canonicalize()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default();
    finish(
        text,
        dir,
        &check::check_plugin(&files, &name),
        Code::PluginCheckFailed,
    )
}

fn finish(
    text: &Text,
    dir: &Path,
    findings: &[finding::Finding],
    code: Code,
) -> Result<String, Failure> {
    finding::report(text, &dir.display().to_string(), findings).map_err(|m| (code, m))
}

/// `blog theme dev`: prints where it listens, then serves until interrupted.
///
/// # Errors
/// The folder is unreadable or the port is not available.
pub fn dev(text: &Text, dir: &Path, port: u16) -> Result<String, Failure> {
    DirFiles::read(dir).map_err(|e| dir_failure(text, dir, &e))?;
    let listener = dev::bind(port).map_err(|e| {
        (
            Code::ThemeServeFailed,
            text.t(
                key!("cli.theme.dev.bind_failed"),
                &[("port", port.to_string()), ("reason", e.to_string())],
            ),
        )
    })?;
    let at = listener.local_addr().map_or(port, |a| a.port());
    let line = text.t(
        key!("cli.theme.dev.listening"),
        &[
            ("dir", dir.display().to_string()),
            ("url", format!("http://127.0.0.1:{at}/")),
        ],
    );
    println!("{line}");
    dev::serve(&listener, dir, text, None).map_err(|e| {
        (
            Code::ThemeServeFailed,
            text.t(
                key!("cli.theme.dev.serve_failed"),
                &[("reason", e.to_string())],
            ),
        )
    })?;
    Ok(String::new())
}
