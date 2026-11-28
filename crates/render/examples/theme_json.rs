//! Prints the JSON form of a theme folder's TOML files as one JSON object, `{ "<path>.json":
//! "<json text>" }`, converted exactly as `blog theme push` does (`plumblog_render::convert`).
//! `tools/worker-test` builds pushed-theme documents from it, since Node has no TOML parser.
//!
//! `cargo run -q -p plumblog-render --example theme_json -- themes/minimal`

use plumblog_render::convert::{file, json_name};
use std::collections::BTreeMap;
use std::path::Path;

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) -> Result<(), String> {
    for e in std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = e.map_err(|e| e.to_string())?.path();
        let rel = path
            .strip_prefix(root)
            .map_err(|e| e.to_string())?
            .to_string_lossy()
            .replace('\\', "/");
        if path.is_dir() {
            walk(root, &path, out)?;
        } else if let Some(name) = json_name(&rel).filter(|_| !rel.starts_with("assets/")) {
            let src = std::fs::read_to_string(&path).map_err(|e| format!("{rel}: {e}"))?;
            out.insert(name, file(&rel, &src)?);
        }
    }
    Ok(())
}

fn main() {
    let Some(dir) = std::env::args().nth(1) else {
        eprintln!("usage: theme_json <theme folder>");
        std::process::exit(2);
    };
    let mut out = BTreeMap::new();
    if let Err(e) = walk(Path::new(&dir), Path::new(&dir), &mut out) {
        eprintln!("theme_json: {e}");
        std::process::exit(1);
    }
    println!("{}", serde_json::to_string(&out).unwrap_or_default());
}
