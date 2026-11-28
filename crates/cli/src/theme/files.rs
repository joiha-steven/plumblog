//! A theme or plugin folder read from disk as the render crate's [`ThemeFiles`]. Text files
//! carry their text; every other file is listed with empty text, so the loader sees that it
//! exists (the Worker does the same for fonts and images).

use plumblog_render::ThemeFiles;
use std::collections::BTreeMap;
use std::path::Path;

/// More files than this is not a theme: stop reading instead of walking a wrong folder.
const MAX_FILES: usize = 2_000;
/// Text files larger than this are listed but not read (nothing in a theme is that big).
const MAX_TEXT: u64 = 4 * 1024 * 1024;

#[derive(Default)]
pub struct DirFiles {
    entries: Vec<(String, String)>,
    /// `path -> size in bytes`, for every file.
    pub sizes: BTreeMap<String, u64>,
    /// Paths (relative, `/`-separated) of symbolic links, which are never followed.
    pub symlinks: Vec<String>,
    pub too_many: bool,
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
        Ok(out)
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
                let text = if meta.len() <= MAX_TEXT {
                    String::from_utf8(std::fs::read(&path)?).unwrap_or_default()
                } else {
                    String::new()
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
}

impl ThemeFiles for DirFiles {
    fn read(&self, path: &str) -> Option<&str> {
        self.read_file(path)
    }
    fn paths(&self) -> Vec<&str> {
        self.entries.iter().map(|(p, _)| p.as_str()).collect()
    }
}

/// The default theme the binary carries (`themes/minimal`), text files only for the loader.
pub struct Embedded {
    entries: Vec<(&'static str, &'static str)>,
}

impl Embedded {
    #[must_use]
    pub fn default_theme() -> Self {
        Self {
            entries: super::embed::THEME_FILES
                .iter()
                .map(|(p, b)| (*p, std::str::from_utf8(b).unwrap_or_default()))
                .collect(),
        }
    }
}

impl ThemeFiles for Embedded {
    fn read(&self, path: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(p, _)| *p == path)
            .map(|(_, t)| *t)
    }
    fn paths(&self) -> Vec<&str> {
        self.entries.iter().map(|(p, _)| *p).collect()
    }
}
