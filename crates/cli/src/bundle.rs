//! A release bundle: what `blog deploy` uploads (layout in docs/ARCHITECTURE.md, Release
//! bundle). `scripts/build-bundle` makes one from the repo; releases ship one for owners
//! without Rust.
//!
//! - `worker/`: worker-build's output. `package.json` `main` names the main module (an ES
//!   module); every top-level `.js`/`.mjs` is a module, every `.wasm` a wasm module.
//! - `assets/`: static assets. `_headers` at its root is sent as assets config, not uploaded.
//! - `migrations/`: the D1 migrations this Worker expects (`*.sql`, file-name order).
//!
//! Symlinks are refused anywhere: a bundle is copied around and must hold its own files.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

/// Files at the assets root that configure assets instead of being served (wrangler 4.160.0
/// `createAssetsIgnoreFunction` ignores the same three).
const ASSET_META_FILES: &[&str] = &["_headers", "_redirects", ".assetsignore"];
/// Largest asset Workers static assets accept (wrangler's `MAX_ASSET_SIZE`).
const MAX_ASSET_BYTES: u64 = 25 * 1024 * 1024;

/// One Worker module part.
#[derive(Clone, Debug)]
pub struct Module {
    pub name: String,
    /// `application/javascript+module` or `application/wasm` (wrangler's
    /// `moduleTypeMimeType`).
    pub content_type: &'static str,
    pub bytes: Vec<u8>,
}

/// One static asset.
#[derive(Clone, Debug)]
pub struct Asset {
    /// `/theme/minimal/x.woff2`.
    pub path: String,
    pub file: PathBuf,
    pub size: u64,
    /// 32 lowercase hex characters, as the upload session's manifest requires.
    pub hash: String,
}

#[derive(Clone, Debug)]
pub struct Bundle {
    pub main_module: String,
    pub modules: Vec<Module>,
    pub assets: Vec<Asset>,
    /// `assets/_headers`, sent as `assets.config._headers`.
    pub headers: Option<String>,
    /// `migrations/*.sql`, in file-name order.
    pub migrations: Vec<crate::migrations::Migration>,
}

/// The asset hash: the first 32 hex characters of the SHA-256 of base64(content) followed
/// by the extension. The direct upload docs ask for 32 hex characters per file; wrangler
/// (`hashFile`) hashes the same input with BLAKE3, also cut to 32. Any stable content hash works: it is the key the upload session
/// compares to decide which files are missing.
/// <https://developers.cloudflare.com/workers/static-assets/direct-upload/>
#[must_use]
pub fn asset_hash(bytes: &[u8], path: &str) -> String {
    // As wrangler: base64 of the content plus the extension, so identical bytes served with
    // different types (an empty a.css and b.js) are different assets.
    let ext = Path::new(path)
        .extension()
        .map(|e| e.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut input = crate::setup::base64_std(bytes).into_bytes();
    input.extend_from_slice(ext.as_bytes());
    let mut h = plumblog_auth::base64url::hex(&plumblog_auth::sha256::sha256(&input));
    h.truncate(32);
    h
}

/// Content type for an uploaded asset, by extension; `application/null` (no header), as
/// wrangler sends for unknown types.
#[must_use]
pub fn content_type(path: &str) -> &'static str {
    let ext = path.rsplit_once('.').map_or("", |(_, e)| e);
    match ext.to_ascii_lowercase().as_str() {
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "application/javascript",
        "json" | "map" => "application/json",
        "webmanifest" => "application/manifest+json",
        "txt" => "text/plain",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "ttf" => "font/ttf",
        "wasm" => "application/wasm",
        _ => "application/null",
    }
}

/// Why a bundle cannot be used: the path and what is wrong (not translated).
#[derive(Debug)]
pub struct Invalid {
    pub path: String,
    pub reason: String,
    /// The path is a symlink.
    pub symlink: bool,
}

fn invalid(path: &Path, reason: impl Into<String>) -> Invalid {
    Invalid {
        path: path.display().to_string(),
        reason: reason.into(),
        symlink: false,
    }
}

fn no_symlink(path: &Path) -> Result<fs::Metadata, Invalid> {
    let meta = fs::symlink_metadata(path).map_err(|e| invalid(path, e.to_string()))?;
    if meta.file_type().is_symlink() {
        return Err(Invalid {
            symlink: true,
            ..invalid(path, "symlink")
        });
    }
    Ok(meta)
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<Asset>) -> Result<(), Invalid> {
    let mut entries: Vec<_> = fs::read_dir(dir)
        .map_err(|e| invalid(dir, e.to_string()))?
        .collect::<Result<_, _>>()
        .map_err(|e| invalid(dir, e.to_string()))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for e in entries {
        let p = e.path();
        let meta = no_symlink(&p)?;
        if meta.is_dir() {
            walk(root, &p, out)?;
            continue;
        }
        let rel = p
            .strip_prefix(root)
            .map_err(|err| invalid(&p, err.to_string()))?;
        let parts: Vec<String> = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect();
        if parts.len() == 1 && ASSET_META_FILES.contains(&parts[0].as_str()) {
            continue;
        }
        if meta.len() > MAX_ASSET_BYTES {
            return Err(invalid(&p, format!("larger than {MAX_ASSET_BYTES} bytes")));
        }
        let bytes = fs::read(&p).map_err(|err| invalid(&p, err.to_string()))?;
        out.push(Asset {
            path: format!("/{}", parts.join("/")),
            file: p.clone(),
            size: meta.len(),
            hash: asset_hash(&bytes, &format!("/{}", parts.join("/"))),
        });
    }
    Ok(())
}

/// Reads a bundle directory.
///
/// # Errors
/// [`Invalid`] when a part is missing or unreadable.
pub fn load(dir: &Path) -> Result<Bundle, Invalid> {
    let worker = dir.join("worker");
    let assets_dir = dir.join("assets");
    if !worker.is_dir() {
        return Err(invalid(&worker, "missing"));
    }
    no_symlink(&worker)?;
    if !assets_dir.is_dir() {
        return Err(invalid(&assets_dir, "missing"));
    }
    let pkg = worker.join("package.json");
    let main_module = match fs::read_to_string(&pkg) {
        Ok(text) => serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.get("main").and_then(|m| m.as_str()).map(str::to_string))
            .ok_or_else(|| invalid(&pkg, "no \"main\""))?,
        Err(_) => "index.js".to_string(),
    };
    let mut modules = Vec::new();
    let mut names: Vec<_> = fs::read_dir(&worker)
        .map_err(|e| invalid(&worker, e.to_string()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .collect();
    names.sort();
    for p in names {
        if !no_symlink(&p)?.is_file() {
            continue;
        }
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let content_type = match p.extension().and_then(|e| e.to_str()) {
            Some("js" | "mjs") => "application/javascript+module",
            Some("wasm") => "application/wasm",
            _ => continue,
        };
        let bytes = fs::read(&p).map_err(|e| invalid(&p, e.to_string()))?;
        modules.push(Module {
            name,
            content_type,
            bytes,
        });
    }
    if !modules.iter().any(|m| m.name == main_module) {
        return Err(invalid(&worker.join(&main_module), "main module missing"));
    }
    if !modules.iter().any(|m| m.content_type == "application/wasm") {
        return Err(invalid(&worker, "no .wasm module"));
    }
    let mut assets = Vec::new();
    no_symlink(&assets_dir)?;
    walk(&assets_dir, &assets_dir, &mut assets)?;
    let migrations = load_migrations(&dir.join("migrations"))?;
    let headers_file = assets_dir.join("_headers");
    let headers = if headers_file.is_file() {
        Some(fs::read_to_string(&headers_file).map_err(|e| invalid(&headers_file, e.to_string()))?)
    } else {
        None
    };
    Ok(Bundle {
        main_module,
        modules,
        assets,
        headers,
        migrations,
    })
}

fn load_migrations(dir: &Path) -> Result<Vec<crate::migrations::Migration>, Invalid> {
    if !no_symlink(dir)?.is_dir() {
        return Err(invalid(dir, "missing"));
    }
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| invalid(dir, e.to_string()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "sql"))
        .collect();
    files.sort();
    let mut out = Vec::new();
    for p in files {
        no_symlink(&p)?;
        let sql = fs::read_to_string(&p).map_err(|e| invalid(&p, e.to_string()))?;
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        out.push(crate::migrations::Migration { name, sql });
    }
    if out.is_empty() {
        return Err(invalid(dir, "no .sql file"));
    }
    Ok(out)
}

/// One part of a `multipart/form-data` body.
pub struct Part<'a> {
    pub name: &'a str,
    pub filename: Option<&'a str>,
    pub content_type: &'a str,
    pub bytes: &'a [u8],
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

/// Builds a `multipart/form-data` body; returns the content type and the bytes. Part names
/// are hashes, file names or `metadata`, so they need no escaping.
#[must_use]
pub fn multipart(parts: &[Part]) -> (String, Vec<u8>) {
    let boundary = loop {
        let mut rnd = [0u8; 16];
        // A fixed boundary would also do but for the check below.
        let _ = getrandom::getrandom(&mut rnd);
        let b = format!("plumblog{}", plumblog_auth::base64url::hex(&rnd));
        if !parts.iter().any(|p| contains(p.bytes, b.as_bytes())) {
            break b;
        }
    };
    let mut body = Vec::new();
    for p in parts {
        let mut head = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"{}\"",
            p.name
        );
        if let Some(f) = p.filename {
            let _ = write!(head, "; filename=\"{f}\"");
        }
        let _ = write!(head, "\r\nContent-Type: {}\r\n\r\n", p.content_type);
        body.extend_from_slice(head.as_bytes());
        body.extend_from_slice(p.bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_32_hex_of_sha256() {
        // sha256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        // sha256("YWJjcss") (base64 of "abc", then the extension).
        let h = asset_hash(b"abc", "/x.css");
        assert_eq!(h.len(), 32);
        assert_ne!(h, asset_hash(b"abc", "/x.js"));
        assert_eq!(h, asset_hash(b"abc", "/other/y.css"));
        // Two empty files with different types are different assets.
        assert_ne!(asset_hash(b"", "/a.css"), asset_hash(b"", "/b.js"));
    }

    #[test]
    fn multipart_has_every_part_and_a_closing_boundary() {
        let (ctype, body) = multipart(&[
            Part {
                name: "metadata",
                filename: None,
                content_type: "application/json",
                bytes: b"{}",
            },
            Part {
                name: "index.js",
                filename: Some("index.js"),
                content_type: "application/javascript+module",
                bytes: b"export default {}",
            },
        ]);
        let b = ctype
            .strip_prefix("multipart/form-data; boundary=")
            .unwrap();
        let text = String::from_utf8(body).unwrap();
        assert!(text.starts_with(&format!(
            "--{b}\r\nContent-Disposition: form-data; name=\"metadata\"\r\n"
        )));
        assert!(text.contains("name=\"index.js\"; filename=\"index.js\"\r\nContent-Type: application/javascript+module\r\n\r\nexport default {}\r\n"));
        assert!(text.ends_with(&format!("--{b}--\r\n")));
    }
}
