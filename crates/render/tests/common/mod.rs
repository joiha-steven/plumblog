//! Theme files as the loader takes them: the Worker links no TOML parser, so every `.toml`
//! file is converted to JSON under `<path>.json` first (`plumblog_render::convert`).

use plumblog_render::ThemeFiles;
use plumblog_render::convert::json_name;

pub struct Runtime(Vec<(String, String)>);

/// `files` as the runtime sees them. Only the syntax is converted, not the shape, so a wrong
/// key still reaches the loader.
pub fn runtime<A: AsRef<str>, B: AsRef<str>>(files: impl IntoIterator<Item = (A, B)>) -> Runtime {
    Runtime(
        files
            .into_iter()
            .map(|(p, t)| {
                let (p, t) = (p.as_ref(), t.as_ref());
                match json_name(p) {
                    Some(name) => (name, plumblog_i18n::toml_to_json(t).unwrap()),
                    None => (p.to_owned(), t.to_owned()),
                }
            })
            .collect(),
    )
}

impl ThemeFiles for Runtime {
    fn read(&self, path: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, t)| t.as_str())
    }
    fn paths(&self) -> Vec<&str> {
        self.0.iter().map(|(p, _)| p.as_str()).collect()
    }
}
