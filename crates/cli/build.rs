//! Embeds guides/*.md, locales/*.toml and migrations/*.sql into the binary so `blog` works offline, with no
//! extra crate. Also bakes the release trust root (docs/ARCHITECTURE.md, Release): the bundle hash
//! from `PLUMBLOG_BUNDLE_SHA256` and the build target.
use std::{env, fmt::Write, fs, path::PathBuf};

fn main() {
    release_env();
    embed_theme();
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

    // D1 migrations `(file name, sql)` in file-name order, the order wrangler applies them in;
    // users get no repo, so `blog setup` carries the schema.
    let migrations =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../migrations");
    println!("cargo:rerun-if-changed={}", migrations.display());
    let mut files: Vec<PathBuf> = fs::read_dir(&migrations)
        .expect("migrations/ directory is missing")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "sql"))
        .collect();
    files.sort();
    let mut out = String::from("pub const MIGRATIONS: &[(&str, &str)] = &[\n");
    for path in files {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let abs = path.canonicalize().unwrap();
        writeln!(
            out,
            "    ({name:?}, include_str!({:?})),",
            abs.to_str().unwrap()
        )
        .unwrap();
    }
    out.push_str("];\n");
    fs::write(
        PathBuf::from(env::var("OUT_DIR").unwrap()).join("migrations.rs"),
        out,
    )
    .unwrap();
}

/// `PLUMBLOG_BUNDLE_SHA256` (64 hex) becomes `PLUMBLOG_BAKED_BUNDLE_SHA256`, lowercase; unset or
/// empty bakes "" (a dev build). Anything else fails the build: a release must never ship with
/// a hash `blog deploy` would silently ignore.
fn release_env() {
    println!("cargo:rerun-if-env-changed=PLUMBLOG_BUNDLE_SHA256");
    let hash = env::var("PLUMBLOG_BUNDLE_SHA256").unwrap_or_default();
    let hash = hash.trim();
    assert!(
        hash.is_empty() || (hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())),
        "PLUMBLOG_BUNDLE_SHA256 must be 64 hex characters (the bundle's SHA-256), got {hash:?}"
    );
    println!(
        "cargo:rustc-env=PLUMBLOG_BAKED_BUNDLE_SHA256={}",
        hash.to_ascii_lowercase()
    );
    println!(
        "cargo:rustc-env=PLUMBLOG_TARGET={}",
        env::var("TARGET").unwrap()
    );
    // include_str! tracks the file itself; this also reruns when it is created.
    let key = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../../release");
    println!("cargo:rerun-if-changed={}", key.display());
}

/// The default theme (every file, fonts included: `blog theme new` copies it for users, who
/// get no repo) and the sample posts `blog theme dev` and `blog theme check` render, as
/// `(path, bytes)` and `(file name, text)`. The Worker embeds the same files for its own use.
fn embed_theme() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap()).join("../..");
    let theme = root.join("themes/minimal");
    let sample = root.join("crates/worker/sample");
    for dir in [&theme, &sample] {
        println!("cargo:rerun-if-changed={}", dir.display());
    }
    let mut out = String::from("pub const THEME_FILES: &[(&str, &[u8])] = &[\n");
    let mut files = Vec::new();
    walk(&theme, &mut files);
    files.sort();
    for p in &files {
        let rel = p
            .strip_prefix(&theme)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let abs = p.canonicalize().unwrap();
        writeln!(
            out,
            "    ({rel:?}, include_bytes!({:?})),",
            abs.to_str().unwrap()
        )
        .unwrap();
    }
    out.push_str("];\npub const SAMPLE_FILES: &[(&str, &str)] = &[\n");
    let mut files = Vec::new();
    walk(&sample, &mut files);
    files.sort();
    for p in files
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "txt"))
    {
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        let abs = p.canonicalize().unwrap();
        writeln!(
            out,
            "    ({name:?}, include_str!({:?})),",
            abs.to_str().unwrap()
        )
        .unwrap();
    }
    out.push_str("];\n");
    fs::write(
        PathBuf::from(env::var("OUT_DIR").unwrap()).join("theme_embed.rs"),
        out,
    )
    .unwrap();
}

fn walk(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}
