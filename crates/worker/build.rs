//! Embeds the default theme's text files, bundled plugins and the sample posts, so the Worker renders pages
//! with no storage at all. Fonts and JS are served as static assets, not embedded (they would
//! only grow the wasm).
use std::fmt::Write;
use std::path::{Path, PathBuf};
use std::{env, fs};

fn main() {
    let crate_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let root = crate_dir.join("../..");
    let theme = root.join("themes/minimal");
    let sample = crate_dir.join("sample");
    for dir in [&theme, &sample, &root.join("locales")] {
        println!("cargo:rerun-if-changed={}", dir.display());
    }

    let mut out = String::new();

    let mut files = Vec::new();
    walk(&theme, &mut files);
    files.retain(|p| {
        let rel = rel(&theme, p);
        let text = matches!(
            p.extension().and_then(|e| e.to_str()),
            Some("html" | "toml" | "css")
        );
        text && (!rel.starts_with("assets/") || rel == "assets/fonts/faces.css")
    });
    files.sort();
    out.push_str("pub static THEME_FILES: &[(&str, &str)] = &[\n");
    for p in &files {
        writeln!(
            out,
            "    ({:?}, include_str!({:?})),",
            rel(&theme, p),
            p.display()
        )
        .unwrap();
    }
    out.push_str("];\n");

    embed_plugins(&root.join("plugins"), &mut out);

    embed_locales(&root.join("locales"), &mut out);

    let mut posts = Vec::new();
    walk(&sample, &mut posts);
    posts.retain(|p| p.extension().is_some_and(|e| e == "txt") && !p.ends_with("site.txt"));
    posts.sort();
    out.push_str("pub static SAMPLE_POSTS: &[SamplePost] = &[\n");
    for p in &posts {
        let text = fs::read_to_string(p).unwrap();
        let (head, body) = text
            .split_once("\n\n")
            .unwrap_or_else(|| panic!("{}: header, blank line, then Markdown", p.display()));
        let field = |name: &str| -> String {
            head.lines()
                .find_map(|l| l.strip_prefix(name).and_then(|r| r.strip_prefix(':')))
                .map(|v| v.trim().to_owned())
                .unwrap_or_default()
        };
        let slug = p.file_stem().unwrap().to_string_lossy();
        let tags: Vec<String> = field("tags")
            .split(',')
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_owned)
            .collect();
        writeln!(
            out,
            "    SamplePost {{ slug: {slug:?}, title: {:?}, date: {:?}, pinned: {}, excerpt: {:?}, tags: &{tags:?}, markdown: {body:?} }},",
            field("title"),
            field("date"),
            field("pinned") == "yes",
            field("excerpt"),
        )
        .unwrap();
    }
    out.push_str("];\n");

    let site = fs::read_to_string(sample.join("site.txt")).unwrap();
    let get = |name: &str| -> String {
        site.lines()
            .find_map(|l| l.strip_prefix(name).and_then(|r| r.strip_prefix(':')))
            .map(|v| v.trim().to_owned())
            .unwrap_or_default()
    };
    writeln!(
        out,
        "pub static SAMPLE_SITE: SampleSite = SampleSite {{ title: {:?}, description: {:?}, footer: {:?}, lang: {:?}, per_page: {}, origin: {:?}, updated: {:?} }};",
        get("title"),
        get("description"),
        get("footer"),
        get("lang"),
        get("per_page").parse::<usize>().expect("sample/site.txt: per_page must be a number"),
        get("origin"),
        Some(get("updated"))
            .filter(|d| d.len() == 10)
            .expect("sample/site.txt: updated must be a YYYY-MM-DD date"),
    )
    .unwrap();

    fs::write(
        PathBuf::from(env::var("OUT_DIR").unwrap()).join("embedded.rs"),
        out,
    )
    .unwrap();
}

/// Bundled plugins (docs/ARCHITECTURE.md, Plugins and options). Text files are embedded;
/// other assets are served as static files, listed with empty text so the load check sees
/// that they exist.
fn embed_plugins(plugins_dir: &Path, out: &mut String) {
    println!("cargo:rerun-if-changed={}", plugins_dir.display());
    let mut names: Vec<PathBuf> = fs::read_dir(plugins_dir)
        .map(|d| {
            d.map(|e| e.unwrap().path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    out.push_str("pub static PLUGIN_FILES: &[(&str, &[(&str, &str)])] = &[\n");
    for dir in &names {
        let mut files = Vec::new();
        walk(dir, &mut files);
        files.sort();
        writeln!(
            out,
            "    ({:?}, &[",
            dir.file_name().unwrap().to_string_lossy()
        )
        .unwrap();
        for p in &files {
            let rel = rel(dir, p);
            let embed = !rel.starts_with("assets/") || p.extension().is_some_and(|e| e == "css");
            if embed {
                writeln!(out, "        ({rel:?}, include_str!({:?})),", p.display()).unwrap();
            } else {
                writeln!(out, "        ({rel:?}, \"\"),").unwrap();
            }
        }
        out.push_str("    ]),\n");
    }
    out.push_str("];\n");
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in fs::read_dir(dir).unwrap_or_else(|e| panic!("{}: {e}", dir.display())) {
        let p = e.unwrap().path();
        if p.is_dir() {
            walk(&p, out);
        } else {
            out.push(p);
        }
    }
}

fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/")
}

/// A locale file without its `[admin.<name>]` sections (each runs to the next `[` header).
/// The bare `[admin]` section stays: the Worker's shell uses its title and noscript text.
fn without_admin_client_text(src: &str) -> String {
    let mut keep = true;
    let mut out = String::with_capacity(src.len());
    for line in src.lines() {
        if line.starts_with('[') {
            keep = !line.starts_with("[admin.");
        }
        if keep {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

fn embed_locales(dir: &Path, out: &mut String) {
    // Core UI text, for the few strings shown when the theme itself cannot render. The admin
    // client's text (`[admin.*]` subsections) is built into its bundle by scripts/admin-build,
    // so it is left out here: it would only grow the wasm.
    let mut locales = Vec::new();
    walk(dir, &mut locales);
    locales.sort();
    out.push_str("pub static CORE_LOCALES: &[(&str, &str)] = &[\n");
    for p in &locales {
        let lang = p.file_stem().unwrap().to_string_lossy();
        let text = without_admin_client_text(&fs::read_to_string(p).unwrap());
        writeln!(out, "    ({lang:?}, {text:?}),").unwrap();
    }
    out.push_str("];\n");
}
