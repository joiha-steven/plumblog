//! A theme or plugin folder read from disk as the render crate's [`ThemeFiles`]. Text files
//! carry their text; every other file is listed with empty text, so the loader sees that it
//! exists (the Worker does the same for fonts and images). The loader sees TOML files the way
//! the Worker does, as JSON under `<path>.json` (`plumblog_render::convert`).

use plumblog_render::ThemeFiles;
use plumblog_render::convert;
use std::collections::BTreeMap;
use std::path::Path;

/// More files than this is not a theme: stop reading instead of walking a wrong folder.
const MAX_FILES: usize = 2_000;
/// Text files larger than this are not given to the loader (nothing in a theme is that big;
/// `blog theme check` refuses them).
pub const MAX_TEXT: u64 = 4 * 1024 * 1024;
/// Largest file a push uploads (Workers static assets); bigger files are not read at all.
pub const MAX_FILE: u64 = 25 * 1024 * 1024;

#[derive(Default)]
pub struct DirFiles {
    entries: Vec<(String, String)>,
    /// `path -> size in bytes`, for every file.
    pub sizes: BTreeMap<String, u64>,
    /// Paths (relative, `/`-separated) of symbolic links, which are never followed.
    pub symlinks: Vec<String>,
    /// What the secret scan reads for files whose bytes are not their loader text (not UTF-8,
    /// or over [`MAX_TEXT`]): the bytes as lossy UTF-8, since a push ships the real bytes.
    pub lossy: BTreeMap<String, String>,
    /// Files outside `assets/` that are not UTF-8 text (a push cannot carry them as text).
    pub not_text: Vec<String>,
    /// Every file that is not UTF-8, below `assets/` too.
    non_utf8: Vec<String>,
    pub too_many: bool,
    /// TOML files outside `assets/` as JSON, by [`convert::json_name`].
    converted: BTreeMap<String, String>,
    /// `(path, message)` of each TOML file that does not convert; the loader never sees it.
    pub convert_errors: Vec<(String, String)>,
}

impl DirFiles {
    /// Reads `dir`, skipping `.git` and `.DS_Store`.
    ///
    /// # Errors
    /// The folder or a file in it cannot be read.
    pub fn read(dir: &Path) -> std::io::Result<Self> {
        let mut out = Self::default();
        out.walk(dir, "")?;
        out.entries.sort();
        out.convert();
        Ok(out)
    }

    fn convert(&mut self) {
        for (path, text) in &self.entries {
            let Some(name) = convert::json_name(path).filter(|_| !path.starts_with("assets/"))
            else {
                continue;
            };
            match convert::file(path, text) {
                Ok(json) => {
                    self.converted.insert(name, json);
                }
                Err(e) => self.convert_errors.push((path.clone(), e)),
            }
        }
        // A carried file below assets/ must be text, as for any file the push puts in the
        // document (`store::build` refuses it).
        if let Some(m) = self.converted.get("theme.toml.json").cloned() {
            for p in plumblog_render::pushed::document_assets(&m) {
                if self.non_utf8.contains(&p) {
                    self.not_text.push(p);
                }
            }
        }
    }

    /// The files as a push carries them: text files outside `assets/` (TOML as its JSON form)
    /// plus the files below it the loader reads (`plumblog_render::pushed::document_assets`).
    #[must_use]
    pub fn document(&self) -> BTreeMap<&str, &str> {
        let own = self
            .entries
            .iter()
            .filter(|(p, _)| !p.starts_with("assets/") && convert::json_name(p).is_none())
            .map(|(p, t)| (p.as_str(), t.as_str()));
        let json = self.converted.iter().map(|(p, t)| (p.as_str(), t.as_str()));
        // Files below assets/ that the loader reads (`fonts.faces`).
        let carried = self
            .converted
            .get("theme.toml.json")
            .map(|m| plumblog_render::pushed::document_assets(m))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|p| {
                let i = self
                    .entries
                    .binary_search_by(|(e, _)| e.as_str().cmp(&p))
                    .ok()?;
                Some((self.entries[i].0.as_str(), self.entries[i].1.as_str()))
            });
        own.chain(json).chain(carried).collect()
    }

    fn walk(&mut self, dir: &Path, prefix: &str) -> std::io::Result<()> {
        let mut names: Vec<_> = std::fs::read_dir(dir)?
            .map(|e| e.map(|e| e.file_name()))
            .collect::<Result<_, _>>()?;
        names.sort();
        for name in names {
            let name = name.to_string_lossy().into_owned();
            if name == ".git" || name == ".DS_Store" {
                continue;
            }
            let rel = format!("{prefix}{name}");
            let path = dir.join(&name);
            let meta = std::fs::symlink_metadata(&path)?;
            if meta.file_type().is_symlink() {
                self.symlinks.push(rel);
            } else if meta.is_dir() {
                self.walk(&path, &format!("{rel}/"))?;
            } else if self.entries.len() >= MAX_FILES {
                self.too_many = true;
                return Ok(());
            } else {
                let bytes = if meta.len() <= MAX_FILE {
                    std::fs::read(&path)?
                } else {
                    Vec::new()
                };
                let text = match String::from_utf8(bytes) {
                    Ok(t) if meta.len() <= MAX_TEXT => t,
                    Ok(t) => {
                        self.lossy.insert(rel.clone(), t);
                        String::new()
                    }
                    Err(e) => {
                        self.non_utf8.push(rel.clone());
                        if !rel.starts_with("assets/") {
                            self.not_text.push(rel.clone());
                        }
                        let lossy = String::from_utf8_lossy(e.as_bytes()).into_owned();
                        self.lossy.insert(rel.clone(), lossy);
                        String::new()
                    }
                };
                self.sizes.insert(rel.clone(), meta.len());
                self.entries.push((rel, text));
            }
        }
        Ok(())
    }

    /// Text of `path`, or empty for binary and unread files.
    #[must_use]
    pub fn text(&self, path: &str) -> &str {
        self.read_file(path).unwrap_or_default()
    }

    fn read_file(&self, path: &str) -> Option<&str> {
        self.entries
            .binary_search_by(|(p, _)| p.as_str().cmp(path))
            .ok()
            .map(|i| self.entries[i].1.as_str())
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().map(|(p, t)| (p.as_str(), t.as_str()))
    }

    /// Every file as the secret scan reads it: its text, or its bytes as lossy UTF-8. Never
    /// empty for a file that has content, whatever its encoding or size up to [`MAX_FILE`].
    pub fn scanned(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries.iter().map(|(p, t)| {
            let t = self.lossy.get(p).map_or(t.as_str(), String::as_str);
            (p.as_str(), t)
        })
    }
}

impl ThemeFiles for DirFiles {
    fn read(&self, path: &str) -> Option<&str> {
        self.converted
            .get(path)
            .map(String::as_str)
            .or_else(|| self.read_file(path))
    }
    /// The TOML files are listed as their JSON form only, as the Worker lists them.
    fn paths(&self) -> Vec<&str> {
        let own = self
            .entries
            .iter()
            .filter(|(p, _)| p.starts_with("assets/") || convert::json_name(p).is_none())
            .map(|(p, _)| p.as_str());
        own.chain(self.converted.keys().map(String::as_str))
            .collect()
    }
}

/// The default theme the binary carries (`themes/minimal`), text files only for the loader,
/// TOML as JSON like everything the loader reads.
pub struct Embedded {
    entries: Vec<(String, String)>,
}

impl Embedded {
    /// # Panics
    /// The default theme's TOML does not convert; the Worker's build and the tests catch that.
    #[must_use]
    pub fn default_theme() -> Self {
        Self {
            entries: super::embed::THEME_FILES
                .iter()
                .map(|(p, b)| {
                    let text = std::str::from_utf8(b).unwrap_or_default();
                    match convert::json_name(p) {
                        Some(name) => (
                            name,
                            // The Worker's build converts the same files and fails if it cannot.
                            convert::file(p, text).expect("the default theme's TOML converts"),
                        ),
                        None => ((*p).to_owned(), text.to_owned()),
                    }
                })
                .collect(),
        }
    }
}

impl ThemeFiles for Embedded {
    fn read(&self, path: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, t)| t.as_str())
    }
    fn paths(&self) -> Vec<&str> {
        self.entries.iter().map(|(p, _)| p.as_str()).collect()
    }
}
