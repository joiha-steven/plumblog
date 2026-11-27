//! A fake GitHub Releases server on 127.0.0.1 (files at `/download/<tag>/<file>`, the latest
//! release at `/latest`), plus tar and gzip writers to build release archives, including
//! hostile ones.

use plumblog_cli::release::archive::crc32;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;

#[derive(Clone, Debug)]
pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Seen {
    pub path: String,
    /// Lower-case names.
    pub headers: Vec<(String, String)>,
}

#[derive(Default)]
struct Shared {
    routes: HashMap<String, Reply>,
    seen: Vec<Seen>,
}

pub struct FakeRelease {
    pub base: String,
    shared: Arc<Mutex<Shared>>,
}

impl FakeRelease {
    pub fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let shared = Arc::new(Mutex::new(Shared::default()));
        let s = Arc::clone(&shared);
        // Never joined: the thread ends with the test process.
        thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let s = Arc::clone(&s);
                thread::spawn(move || serve(&stream, &s));
            }
        });
        Self { base, shared }
    }

    pub fn file(&self, tag: &str, name: &str, body: &[u8]) {
        self.route(&format!("/download/{tag}/{name}"), 200, &[], body);
    }

    pub fn latest(&self, tag: &str) {
        let body = format!("{{\"tag_name\":\"{tag}\",\"draft\":false}}");
        self.route("/latest", 200, &[], body.as_bytes());
    }

    pub fn route(&self, path: &str, status: u16, headers: &[(&str, &str)], body: &[u8]) {
        let reply = Reply {
            status,
            headers: headers
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                .collect(),
            body: body.to_vec(),
        };
        self.shared
            .lock()
            .unwrap()
            .routes
            .insert(path.into(), reply);
    }

    pub fn seen(&self) -> Vec<Seen> {
        self.shared.lock().unwrap().seen.clone()
    }

    pub fn hits(&self, path: &str) -> usize {
        self.seen().iter().filter(|s| s.path == path).count()
    }
}

fn serve(stream: &TcpStream, shared: &Mutex<Shared>) {
    let mut reader = BufReader::new(stream);
    let mut first = String::new();
    if reader.read_line(&mut first).is_err() {
        return;
    }
    let path = first.split_whitespace().nth(1).unwrap_or("").to_string();
    let mut headers = Vec::new();
    loop {
        let mut l = String::new();
        if reader.read_line(&mut l).unwrap_or(0) == 0 || l.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = l.trim_end().split_once(':') {
            headers.push((k.trim().to_ascii_lowercase(), v.trim().to_string()));
        }
    }
    let path = path.split('?').next().unwrap_or("").to_string();
    let reply = {
        let mut s = shared.lock().unwrap();
        s.seen.push(Seen {
            path: path.clone(),
            headers,
        });
        s.routes.get(&path).cloned()
    }
    .unwrap_or(Reply {
        status: 404,
        headers: Vec::new(),
        body: b"Not Found".to_vec(),
    });
    let mut out = format!(
        "HTTP/1.1 {} X\r\nContent-Length: {}\r\nConnection: close\r\n",
        reply.status,
        reply.body.len()
    );
    for (k, v) in &reply.headers {
        let _ = write!(out, "{k}: {v}\r\n");
    }
    out.push_str("\r\n");
    let mut w = stream;
    let _ = w.write_all(out.as_bytes());
    let _ = w.write_all(&reply.body);
}

/// One tar entry: a ustar header (`kind` is tar's type flag), then the data blocks.
pub fn tar_entry(name: &str, kind: u8, link: &str, data: &[u8]) -> Vec<u8> {
    let mut h = vec![0u8; 512];
    h[..name.len()].copy_from_slice(name.as_bytes());
    h[100..107].copy_from_slice(b"0000644");
    h[124..135].copy_from_slice(format!("{:011o}", data.len()).as_bytes());
    h[157..157 + link.len()].copy_from_slice(link.as_bytes());
    h[156] = kind;
    h[257..263].copy_from_slice(b"ustar\0");
    h[263..265].copy_from_slice(b"00");
    h[148..156].copy_from_slice(b"        ");
    let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
    h[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
    let mut out = h;
    out.extend_from_slice(data);
    out.resize(out.len().div_ceil(512) * 512, 0);
    out
}

pub fn tar(entries: &[Vec<u8>]) -> Vec<u8> {
    let mut out: Vec<u8> = entries.concat();
    out.extend_from_slice(&[0; 1024]);
    out
}

pub fn gzip(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 255];
    out.extend(miniz_oxide::deflate::compress_to_vec(data, 6));
    out.extend_from_slice(&crc32(data).to_le_bytes());
    out.extend_from_slice(&u32::try_from(data.len()).unwrap().to_le_bytes());
    out
}

/// Every file under `dir`, as (`a/b` path, bytes), sorted.
pub fn files_under(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(root: &Path, d: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for e in std::fs::read_dir(d).unwrap().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(root, &p, out);
            } else {
                let rel = p
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, std::fs::read(&p).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// bundle.json for `files` (paths relative to the bundle root).
pub fn bundle_json(version: &str, files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let listed: serde_json::Map<String, serde_json::Value> = files
        .iter()
        .map(|(p, b)| (p.clone(), plumblog_cli::release::sha256_hex(b).into()))
        .collect();
    serde_json::json!({"version": version, "commit": "0123abc", "files": listed})
        .to_string()
        .into_bytes()
}
