//! guides/*.md as Rust source, shared by the `blog` CLI's and the Worker's build scripts
//! (each includes this file with `#[path]`): `blog guide` and the MCP resources embed the same
//! files under the same checks, so they cannot disagree.
use std::fmt::Write;
use std::fs;
use std::path::{Path, PathBuf};

/// `pub const GUIDES: &[(&str, &str, &str)]`, one `(name, summary, text)` per `root/*.md` in
/// file-name order. Panics, failing the build, on a bad file name, a duplicate, or a guide
/// without a `# <title>` first line and a summary line after it.
pub fn source(root: &Path) -> String {
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files: Vec<PathBuf> = fs::read_dir(root)
        .expect("guides/ directory is missing")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .collect();
    files.sort();

    let mut seen = std::collections::HashSet::new();
    let mut out = String::from("pub const GUIDES: &[(&str, &str, &str)] = &[\n");
    for path in files {
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        assert!(
            !name.is_empty()
                && name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "guide file name {name:?} must match ^[a-z0-9-]+$"
        );
        assert!(seen.insert(name.clone()), "duplicate guide topic {name}");
        let text = fs::read_to_string(&path).unwrap();
        let title = text.lines().next().and_then(|l| l.strip_prefix("# "));
        assert!(
            title.is_some_and(|t| !t.trim().is_empty()),
            "guide {name}: first line must be `# <title>`"
        );
        // Line 1 is the title; the first non-empty line after it is the summary.
        let summary = text
            .lines()
            .skip(1)
            .find(|l| !l.trim().is_empty())
            .unwrap_or_else(|| panic!("guide {name}: missing summary line after the title"))
            .trim();
        let abs = path.canonicalize().unwrap();
        writeln!(
            out,
            "    ({name:?}, {summary:?}, include_str!({:?})),",
            abs.to_str().unwrap()
        )
        .unwrap();
    }
    out.push_str("];\n");
    out
}
