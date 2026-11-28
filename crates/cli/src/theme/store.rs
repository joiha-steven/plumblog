//! A pushed theme as files: built from a theme folder, kept in the site folder under
//! [`STORE_DIR`]`/<hash>/` (`theme.json` plus `assets/`), or fetched back from the site, and
//! always checked byte for byte against its hash before it is uploaded again.
//!
//! Every deploy uploads all static assets of the new version, so the active pushed theme must
//! travel with each one; this is where `blog deploy` gets it from.

use crate::bundle::{self, Asset};
use crate::state::ThemeRecord;
use plumblog_render::PushedTheme;
use plumblog_render::THEME_API;
use plumblog_render::pushed::{ASSETS_DIR, asset_path, document_path, valid_name};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Local copies of pushed themes, relative to the site folder (git-ignored).
pub const STORE_DIR: &str = ".blog/themes";
/// Copies kept; older ones are removed after a push.
pub const KEEP: usize = 5;
/// Largest file Workers static assets accept (as `bundle`).
const MAX_FILE: u64 = 25 * 1024 * 1024;

/// A theme's document and the bytes of each file below `assets/`.
pub struct Built {
    pub doc: PushedTheme,
    pub assets: BTreeMap<String, Vec<u8>>,
}

/// The hash as owners see it (the first 16 hex digits, as in page cache keys).
#[must_use]
pub fn short(hash: &str) -> &str {
    hash.get(..16).unwrap_or(hash)
}

#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    plumblog_auth::base64url::hex(&plumblog_auth::sha256::sha256(bytes))
}

impl Built {
    /// The record a deploy keeps.
    #[must_use]
    pub fn record(&self) -> ThemeRecord {
        ThemeRecord {
            name: self.doc.name.clone(),
            version: self.doc.version.clone(),
            hash: self.doc.hash.clone(),
        }
    }

    /// Hash and manifest agree with the content: the hash over the text files and asset
    /// list, and each asset's bytes against its listed SHA-256.
    ///
    /// # Errors
    /// What does not match.
    pub fn verify(&self) -> Result<(), String> {
        let got = sha256_hex(&self.doc.hash_input());
        if got != self.doc.hash {
            return Err(format!(
                "theme.json hash {} does not match its content ({got})",
                self.doc.hash
            ));
        }
        for (p, sha) in &self.doc.assets {
            let Some(bytes) = self.assets.get(p) else {
                return Err(format!("assets/{p} is missing"));
            };
            if &sha256_hex(bytes) != sha {
                return Err(format!(
                    "assets/{p} does not match its SHA-256 in theme.json"
                ));
            }
        }
        Ok(())
    }
}

/// Reads a theme folder (one that passed `blog theme check`) into a document: every file
/// outside `assets/` as UTF-8 text, every file below it as bytes.
///
/// # Errors
/// `(path, reason)`: an unreadable or non-UTF-8 file, or a `theme.toml` without a valid name.
pub fn build(dir: &Path) -> Result<Built, (String, String)> {
    let listed = super::files::DirFiles::read(dir)
        .map_err(|e| (dir.display().to_string(), e.to_string()))?;
    let mut files = BTreeMap::new();
    let mut assets = BTreeMap::new();
    let mut hashes = BTreeMap::new();
    for (rel, size) in &listed.sizes {
        let path = rel.split('/').fold(dir.to_path_buf(), |p, c| p.join(c));
        let shown = || path.display().to_string();
        if *size > MAX_FILE {
            return Err((shown(), format!("larger than {MAX_FILE} bytes")));
        }
        let bytes = fs::read(&path).map_err(|e| (shown(), e.to_string()))?;
        if let Some(a) = rel.strip_prefix(ASSETS_DIR) {
            hashes.insert(a.to_owned(), sha256_hex(&bytes));
            assets.insert(a.to_owned(), bytes);
        } else {
            let text = String::from_utf8(bytes).map_err(|_| (shown(), "not UTF-8 text".into()))?;
            files.insert(rel.clone(), text);
        }
    }
    let manifest: toml::Table = files
        .get("theme.toml")
        .map(|t| toml::from_str(t))
        .transpose()
        .map_err(|e| (dir.join("theme.toml").display().to_string(), e.to_string()))?
        .unwrap_or_default();
    let field = |k: &str| manifest.get(k).and_then(toml::Value::as_str).unwrap_or("");
    let name = field("name").to_owned();
    if !valid_name(&name) {
        return Err((
            dir.join("theme.toml").display().to_string(),
            format!("name {name:?} is not a theme name"),
        ));
    }
    let mut doc = PushedTheme {
        api: THEME_API,
        name,
        version: field("version").to_owned(),
        hash: String::new(),
        files,
        assets: hashes,
    };
    doc.hash = sha256_hex(&doc.hash_input());
    Ok(Built { doc, assets })
}

fn copy_dir(site: &Path, hash: &str) -> PathBuf {
    site.join(STORE_DIR).join(hash)
}

fn under(base: &Path, rel: &str) -> PathBuf {
    rel.split('/').fold(base.to_path_buf(), |p, c| p.join(c))
}

/// Writes the local copy (once per hash: written to a temporary folder, then renamed, so a
/// copy is complete or absent) and keeps the newest [`KEEP`].
///
/// # Errors
/// `(path, reason)` of the write that failed.
pub fn save(site: &Path, b: &Built) -> Result<(), (String, String)> {
    let target = copy_dir(site, &b.doc.hash);
    let fail = |p: &Path, e: std::io::Error| (p.display().to_string(), e.to_string());
    if load(site, &b.doc.name, &b.doc.hash).is_ok() {
        return Ok(());
    }
    let tmp = site
        .join(STORE_DIR)
        .join(format!(".{}.tmp{}", b.doc.hash, std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    let write = |rel: &str, bytes: &[u8]| -> Result<(), (String, String)> {
        let p = under(&tmp, rel);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).map_err(|e| fail(parent, e))?;
        }
        fs::write(&p, bytes).map_err(|e| fail(&p, e))
    };
    let json =
        serde_json::to_string(&b.doc).map_err(|e| (tmp.display().to_string(), e.to_string()))?;
    write("theme.json", json.as_bytes())?;
    for (rel, bytes) in &b.assets {
        write(&format!("{ASSETS_DIR}{rel}"), bytes)?;
    }
    // A damaged copy under the same name is replaced.
    let _ = fs::remove_dir_all(&target);
    fs::rename(&tmp, &target).map_err(|e| fail(&target, e))?;
    prune(site, &b.doc.hash);
    Ok(())
}

/// Removes all but the newest [`KEEP`] copies, never `keep`. Failures only leave a copy.
fn prune(site: &Path, keep: &str) {
    let Ok(entries) = fs::read_dir(site.join(STORE_DIR)) else {
        return;
    };
    let mut copies: Vec<(std::time::SystemTime, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().len() == 64)
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    copies.sort_by_key(|c| std::cmp::Reverse(c.0));
    for (_, p) in copies.into_iter().skip(KEEP) {
        if !p.ends_with(keep) {
            let _ = fs::remove_dir_all(p);
        }
    }
}

/// The local copy of `hash`, verified.
///
/// # Errors
/// Why it cannot be used (absent, damaged, another theme).
pub fn load(site: &Path, name: &str, hash: &str) -> Result<Built, String> {
    let dir = copy_dir(site, hash);
    let json = fs::read_to_string(dir.join("theme.json"))
        .map_err(|e| format!("{}: {e}", dir.join("theme.json").display()))?;
    let doc = PushedTheme::parse(&json, name)?;
    if doc.hash != hash {
        return Err(format!("{} holds theme {}", dir.display(), doc.hash));
    }
    let mut assets = BTreeMap::new();
    for rel in doc.assets.keys() {
        let p = under(&dir.join("assets"), rel);
        let bytes = fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        assets.insert(rel.clone(), bytes);
    }
    let b = Built { doc, assets };
    b.verify()?;
    Ok(b)
}

/// The theme as the site serves it: `theme.json`, then each asset, from `base` (the site's
/// origin, public, no token).
///
/// # Errors
/// A request that fails or answers other than 200, or content that does not verify.
pub fn fetch(agent: &ureq::Agent, base: &str, name: &str) -> Result<Built, String> {
    let get = |path: &str| -> Result<Vec<u8>, String> {
        let url = format!("{base}{path}");
        let r = agent.get(&url).call().map_err(|e| format!("{path}: {e}"))?;
        let status = r.status().as_u16();
        if status != 200 {
            return Err(format!("{path}: HTTP {status}"));
        }
        r.into_body()
            .into_with_config()
            .limit(MAX_FILE)
            .read_to_vec()
            .map_err(|e| format!("{path}: {e}"))
    };
    let path = document_path(name);
    let json = String::from_utf8(get(&path)?).map_err(|_| format!("{path}: not UTF-8"))?;
    let doc = PushedTheme::parse(&json, name)?;
    let mut assets = BTreeMap::new();
    for rel in doc.assets.keys() {
        assets.insert(rel.clone(), get(&asset_path(name, rel))?);
    }
    let b = Built { doc, assets };
    b.verify()?;
    Ok(b)
}

/// The static assets a version carries for this theme: `theme.json` and every asset, from
/// the local copy ([`save`] first), hashed as uploaded.
///
/// # Errors
/// `(path, reason)`: the copy's `theme.json` cannot be read.
pub fn assets(site: &Path, b: &Built) -> Result<Vec<Asset>, (String, String)> {
    let dir = copy_dir(site, &b.doc.hash);
    let name = &b.doc.name;
    let one = |path: String, file: PathBuf, bytes: &[u8]| Asset {
        hash: bundle::asset_hash(bytes, &path),
        size: bytes.len() as u64,
        path,
        file,
    };
    let doc = dir.join("theme.json");
    let json = fs::read(&doc).map_err(|e| (doc.display().to_string(), e.to_string()))?;
    let mut out = vec![one(document_path(name), doc, &json)];
    for (rel, bytes) in &b.assets {
        out.push(one(
            asset_path(name, rel),
            under(&dir.join("assets"), rel),
            bytes,
        ));
    }
    Ok(out)
}
