//! Makes a theme with `blog theme new` in an empty folder and prints the `theme.json` a push
//! of it would upload. `tools/worker-test` serves it from a real Worker.
//!
//! `cargo run -q -p plumblog-cli --example theme_document -- <empty folder> <theme name>`

use plumblog_cli::text::Text;
use plumblog_cli::theme;
use std::path::Path;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [dir, name] = args.as_slice() else {
        eprintln!("usage: theme_document <empty folder> <theme name>");
        std::process::exit(2);
    };
    if let Err(e) = theme::new(&Text::new("en"), Path::new(dir), Some(name)) {
        eprintln!("theme_document: theme new failed: {e:?}");
        std::process::exit(1);
    }
    match theme::store::build(Path::new(dir)) {
        Ok(built) => println!("{}", built.doc.to_json()),
        Err((path, why)) => {
            eprintln!("theme_document: {path}: {}", why.words(&Text::new("en")));
            std::process::exit(1);
        }
    }
}
