//! Writes the admin client's UI text as one ES module per language (`<lang>.js`), for
//! `scripts/admin-build`: `cargo run -q -p plumblog-i18n --example admin_locales -- LOCALES OUT`.
//! The text lives in the admin bundle, not the Worker (docs/ARCHITECTURE.md, Admin and editor).

use plumblog_i18n::admin::admin_module;
use std::path::Path;
use std::{env, fs, process};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let [locales, out] = args.as_slice() else {
        eprintln!("usage: admin_locales LOCALES_DIR OUT_DIR");
        process::exit(2);
    };
    if let Err(e) = run(Path::new(locales), Path::new(out)) {
        eprintln!("admin_locales: {e}");
        process::exit(1);
    }
}

fn run(locales: &Path, out: &Path) -> Result<(), String> {
    let read = |p: &Path| fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()));
    let en = read(&locales.join("en.toml"))?;
    fs::create_dir_all(out).map_err(|e| format!("{}: {e}", out.display()))?;
    let mut entries: Vec<_> = fs::read_dir(locales)
        .map_err(|e| format!("{}: {e}", locales.display()))?
        .filter_map(std::result::Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    entries.sort();
    for p in entries {
        let lang = p
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or("locale file name")?;
        let module = admin_module(&read(&p)?, &en, lang == "en")
            .map_err(|e| format!("{}: {e}", p.display()))?;
        let dest = out.join(format!("{lang}.js"));
        fs::write(&dest, module).map_err(|e| format!("{}: {e}", dest.display()))?;
    }
    Ok(())
}
