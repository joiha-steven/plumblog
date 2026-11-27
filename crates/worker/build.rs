//! Embeds the default theme's text files and the sample posts, so the Worker renders pages
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

    // Core UI text, for the few strings shown when the theme itself cannot render.
    let mut locales = Vec::new();
    walk(&root.join("locales"), &mut locales);
    locales.sort();
    out.push_str("pub static CORE_LOCALES: &[(&str, &str)] = &[\n");
    for p in &locales {
        let lang = p.file_stem().unwrap().to_string_lossy();
        writeln!(out, "    ({lang:?}, include_str!({:?})),", p.display()).unwrap();
    }
    out.push_str("];\n");

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
        "pub static SAMPLE_SITE: SampleSite = SampleSite {{ title: {:?}, description: {:?}, footer: {:?}, lang: {:?}, per_page: {} }};",
        get("title"),
        get("description"),
        get("footer"),
        get("lang"),
        get("per_page").parse::<usize>().expect("sample/site.txt: per_page must be a number"),
    )
    .unwrap();

    fs::write(
        PathBuf::from(env::var("OUT_DIR").unwrap()).join("embedded.rs"),
        out,
    )
    .unwrap();
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
