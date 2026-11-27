//! Embeds guides/*.md and locales/*.toml into the binary so `blog` works offline, with no
//! extra crate.
use std::{env, fmt::Write, fs, path::PathBuf};

fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../guides");
    println!("cargo:rerun-if-changed={}", root.display());

    let mut files: Vec<PathBuf> = fs::read_dir(&root)
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
    fs::write(
        PathBuf::from(env::var("OUT_DIR").unwrap()).join("guides.rs"),
        out,
    )
    .unwrap();

    // Core locale files: `(language, source)`. Every language the repo ships is available.
    let locales = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../locales");
    println!("cargo:rerun-if-changed={}", locales.display());
    let mut files: Vec<PathBuf> = fs::read_dir(&locales)
        .expect("locales/ directory is missing")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();
    let mut out = String::from("pub const LOCALES: &[(&str, &str)] = &[\n");
    for path in files {
        let lang = path.file_stem().unwrap().to_string_lossy().into_owned();
        let abs = path.canonicalize().unwrap();
        writeln!(
            out,
            "    ({lang:?}, include_str!({:?})),",
            abs.to_str().unwrap()
        )
        .unwrap();
    }
    out.push_str("];\n");
    fs::write(
        PathBuf::from(env::var("OUT_DIR").unwrap()).join("locales.rs"),
        out,
    )
    .unwrap();
}
