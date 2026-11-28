//! A pushed theme as files: built from a theme folder, kept in the site folder under
//! [`STORE_DIR`]`/<hash>/` (`theme.json` plus `assets/`), or fetched back from the site, and
//! always checked byte for byte against its hash before it is uploaded again.
//!
//! Every deploy uploads all static assets of the new version, so the active pushed theme must
//! travel with each one; this is where `blog deploy` gets it from.

use crate::bundle::{self, Asset};
use crate::state::ThemeRecord;
use crate::text::Text;
use plumblog_i18n::{Key, key};
use plumblog_render::PushedTheme;
use plumblog_render::THEME_API;
use plumblog_render::convert;
use plumblog_render::pushed::{
    ASSETS_DIR, MAX_DOCUMENT, asset_path, asset_url, document_path, valid_name,
};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// Local copies of pushed themes, relative to the site folder (git-ignored).
pub const STORE_DIR: &str = ".blog/themes";
/// Copies kept; older ones are removed after a push.
pub const KEEP: usize = 5;
/// Largest file Workers static assets accept (as `bundle`).
const MAX_FILE: u64 = 25 * 1024 * 1024;

/// Why a theme cannot be built, read back or verified: a locale key with its data, or data
/// alone (I/O and parse errors), so the owner reads it in their language.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Why {
    Said(Key, Vec<(&'static str, String)>),
    Data(String),
}

impl Why {
    fn said(k: Key, args: &[(&'static str, String)]) -> Self {
        Self::Said(k, args.to_vec())
    }

    #[must_use]
    pub fn words(&self, text: &Text) -> String {
        match self {
            Self::Said(k, args) => {
                let args: Vec<(&str, String)> = args.iter().map(|(n, v)| (*n, v.clone())).collect();
                text.t(*k, &args)
            }
            Self::Data(d) => d.clone(),
        }
    }
}

impl From<String> for Why {
    fn from(d: String) -> Self {
        Self::Data(d)
    }
}

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
    pub fn verify(&self) -> Result<(), Why> {
        let got = sha256_hex(&self.doc.hash_input());
        if got != self.doc.hash {
            return Err(Why::said(
                key!("cli.theme.why.hash_mismatch"),
                &[("stated", self.doc.hash.clone()), ("got", got)],
            ));
        }
        for (p, sha) in &self.doc.assets {
            let path = format!("{ASSETS_DIR}{p}");
            let Some(bytes) = self.assets.get(p) else {
                return Err(Why::said(
                    key!("cli.theme.why.asset_missing"),
                    &[("path", path)],
                ));
            };
            if &sha256_hex(bytes) != sha {
                return Err(Why::said(
                    key!("cli.theme.why.asset_changed"),
                    &[("path", path)],
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
pub fn build(dir: &Path) -> Result<Built, (String, Why)> {
    let listed = super::files::DirFiles::read(dir)
        .map_err(|e| (dir.display().to_string(), Why::Data(e.to_string())))?;
    let mut files = BTreeMap::new();
    let mut assets = BTreeMap::new();
    let mut hashes = BTreeMap::new();
    for (rel, size) in &listed.sizes {
        let path = rel.split('/').fold(dir.to_path_buf(), |p, c| p.join(c));
        let shown = || path.display().to_string();
        if *size > MAX_FILE {
            return Err((
                shown(),
                Why::said(
                    key!("cli.theme.why.too_large"),
                    &[("max", MAX_FILE.to_string())],
                ),
            ));
        }
        let bytes = fs::read(&path).map_err(|e| (shown(), Why::Data(e.to_string())))?;
        if let Some(a) = rel.strip_prefix(ASSETS_DIR) {
            hashes.insert(a.to_owned(), sha256_hex(&bytes));
            assets.insert(a.to_owned(), bytes);
        } else {
            let text = String::from_utf8(bytes)
                .map_err(|_| (shown(), Why::said(key!("cli.theme.why.not_utf8"), &[])))?;
            files.insert(rel.clone(), text);
        }
    }
    // The Worker reads TOML as JSON (`plumblog_render::convert`): the document carries that
    // form only, so the same text is hashed, stored and served.
    let toml_paths: Vec<String> = files
        .keys()
        .filter(|p| convert::json_name(p).is_some())
        .cloned()
        .collect();
    for path in toml_paths {
        let text = files.remove(&path).unwrap_or_default();
        let json = convert::file(&path, &text)
            .map_err(|e| (dir.join(&path).display().to_string(), Why::Data(e)))?;
        files.insert(convert::json_name(&path).unwrap_or_default(), json);
    }
    let manifest: serde_json::Value = files
        .get("theme.toml.json")
        .and_then(|t| serde_json::from_str(t).ok())
        .unwrap_or_default();
    let field = |k: &str| manifest.get(k).and_then(|v| v.as_str()).unwrap_or("");
    let name = field("name").to_owned();
    if !valid_name(&name) {
        return Err((
            dir.join("theme.toml").display().to_string(),
            Why::said(key!("cli.theme.why.bad_name"), &[("name", name)]),
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
    let size = doc.to_json().len();
    if size > MAX_DOCUMENT {
        return Err((
            dir.display().to_string(),
            Why::said(
                key!("cli.theme.why.document_too_large"),
                &[
                    ("size", size.to_string()),
                    ("max", MAX_DOCUMENT.to_string()),
                ],
            ),
        ));
    }
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
pub fn load(site: &Path, name: &str, hash: &str) -> Result<Built, Why> {
    let dir = copy_dir(site, hash);
    let json = fs::read_to_string(dir.join("theme.json"))
        .map_err(|e| format!("{}: {e}", dir.join("theme.json").display()))?;
    let doc = PushedTheme::parse(&json, name)?;
    if doc.hash != hash {
        return Err(Why::said(
            key!("cli.theme.why.copy_holds_other"),
            &[
                ("path", dir.display().to_string()),
                ("hash", short(&doc.hash).to_owned()),
            ],
        ));
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
/// origin, public, no token). Asset paths are percent-encoded per segment: a theme pushed
/// before names were checked may hold a space, `#`, `%` or `?`. Every request asks past any
/// cache (`Cache-Control: no-cache` and a `fresh=<fresh>` query no cache has seen), so a
/// stale copy that matches its own hash cannot pass for the served theme.
///
/// # Errors
/// A request that fails or answers other than 200, or content that does not verify.
pub fn fetch(agent: &ureq::Agent, base: &str, name: &str, fresh: &str) -> Result<Built, Why> {
    let get = |path: &str| -> Result<Vec<u8>, String> {
        let url = format!("{base}{path}?fresh={fresh}");
        let r = agent
            .get(&url)
            .header("cache-control", "no-cache")
            .call()
            .map_err(|e| format!("{path}: {e}"))?;
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
    let json = String::from_utf8(get(&path)?).map_err(|_| {
        Why::said(
            key!("cli.theme.why.served_not_utf8"),
            &[("path", path.clone())],
        )
    })?;
    let doc = PushedTheme::parse(&json, name)?;
    let mut assets = BTreeMap::new();
    for rel in doc.assets.keys() {
        assets.insert(rel.clone(), get(&asset_url(name, rel))?);
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
