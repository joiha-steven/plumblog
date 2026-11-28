//! `blog theme dev`: a loopback-only preview server. It reads the theme folder again on every
//! request, so an edit shows on refresh; a broken theme answers with the check findings.

use super::check;
use super::embed::THEME_FILES;
use super::files::{DirFiles, Embedded};
use super::sample::Sample;
use crate::text::Text;
use plumblog_i18n::key;
use plumblog_render::Theme;
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const ASSETS_URL: &str = "/_theme";
const HEAD_LIMIT: usize = 16 * 1024;

/// Listens on `127.0.0.1:port` (port 0 picks a free one).
///
/// # Errors
/// The port is taken or not allowed.
pub fn bind(port: u16) -> std::io::Result<TcpListener> {
    TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
}

/// Serves until `limit` connections were handled (`None`: forever).
///
/// # Errors
/// Accepting a connection failed.
pub fn serve(
    listener: &TcpListener,
    dir: &Path,
    text: &Text,
    limit: Option<usize>,
) -> std::io::Result<()> {
    let sample = &Sample::load();
    std::thread::scope(|scope| {
        for (n, stream) in listener.incoming().enumerate() {
            let stream = stream?;
            scope.spawn(move || {
                // A failed write is the browser closing early; nothing to report.
                let _ = handle(stream, dir, text, sample);
            });
            if limit.is_some_and(|l| n + 1 >= l) {
                break;
            }
        }
        Ok(())
    })
}

struct Request {
    method: String,
    target: String,
    host: String,
}

fn read_request(stream: &mut TcpStream) -> Option<Request> {
    stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        if buf.len() > HEAD_LIMIT {
            return None;
        }
        let n = stream.read(&mut chunk).ok().filter(|n| *n > 0)?;
        buf.extend_from_slice(&chunk[..n]);
    }
    let head = String::from_utf8_lossy(&buf);
    let mut lines = head.lines();
    let mut first = lines.next()?.split_whitespace();
    let (method, target) = (first.next()?.to_owned(), first.next()?.to_owned());
    let host = lines
        .find_map(|l| {
            l.split_once(':')
                .filter(|(k, _)| k.eq_ignore_ascii_case("host"))
                .map(|(_, v)| v.trim().to_ascii_lowercase())
        })
        .unwrap_or_default();
    Some(Request {
        method,
        target,
        host,
    })
}

/// Only the machine's own name for itself: a page on the open web must not be able to read
/// the preview through DNS rebinding.
fn loopback_host(host: &str) -> bool {
    let name = host.rsplit_once(':').map_or(host, |(h, p)| {
        if p.chars().all(|c| c.is_ascii_digit()) {
            h
        } else {
            host
        }
    });
    matches!(name, "127.0.0.1" | "localhost" | "[::1]")
}

fn respond(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        _ => "Internal Server Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}

fn handle(mut stream: TcpStream, dir: &Path, text: &Text, sample: &Sample) -> std::io::Result<()> {
    let Some(req) = read_request(&mut stream) else {
        return respond(&mut stream, 404, "text/plain; charset=utf-8", b"");
    };
    let plain = "text/plain; charset=utf-8";
    if !loopback_host(&req.host) {
        return respond(&mut stream, 403, plain, b"");
    }
    if req.method != "GET" && req.method != "HEAD" {
        return respond(&mut stream, 405, plain, b"");
    }
    let (path, query) = req.target.split_once('?').unwrap_or((&req.target, ""));
    if let Some(rest) = path.strip_prefix(&format!("{ASSETS_URL}/")) {
        return match asset(dir, rest) {
            Some(bytes) => respond(&mut stream, 200, content_type(rest), &bytes),
            None => respond(&mut stream, 404, plain, b""),
        };
    }
    let lang = query
        .split('&')
        .find_map(|kv| kv.strip_prefix("lang="))
        .filter(|l| {
            (2..=8).contains(&l.len()) && l.bytes().all(|b| b.is_ascii_alphabetic() || b == b'-')
        })
        .unwrap_or(sample.lang());
    let (status, html) = page(dir, text, sample, &req.target, lang);
    respond(
        &mut stream,
        status,
        "text/html; charset=utf-8",
        html.as_bytes(),
    )
}

/// `rest` below `assets/` of the theme, else the default theme's copy (a theme may lean on
/// its fonts). Never outside those folders, never through a link.
fn asset(dir: &Path, rest: &str) -> Option<Vec<u8>> {
    let clean = !rest.is_empty()
        && rest
            .split('/')
            .all(|c| !c.is_empty() && c != "." && c != ".." && !c.contains(['\\', ':', '\0']));
    if !clean {
        return None;
    }
    let mut path: PathBuf = dir.join("assets");
    path.extend(rest.split('/'));
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        if meta.is_file() {
            return std::fs::read(&path).ok();
        }
        return None;
    }
    let want = format!("assets/{rest}");
    THEME_FILES
        .iter()
        .find(|(p, _)| *p == want)
        .map(|(_, b)| b.to_vec())
}

fn content_type(path: &str) -> &'static str {
    match path
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "woff2" => "font/woff2",
        "woff" => "font/woff",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// `(status, html)` of one sample page, or the problems of the folder as a plain page.
fn page(dir: &Path, text: &Text, sample: &Sample, target: &str, lang: &str) -> (u16, String) {
    let files = match DirFiles::read(dir) {
        Ok(f) => f,
        Err(e) => return (500, problem_page(text, &e.to_string())),
    };
    let default = Embedded::default_theme();
    match Theme::load(&files, Some(&default), Vec::new(), ASSETS_URL) {
        Ok(theme) => match sample.render(&theme, target, lang) {
            (status, Ok(report)) => (status, report.html),
            (_, Err(e)) => (500, failure_page(text, &files, &e.detail)),
        },
        Err(e) => (500, failure_page(text, &files, &e.detail)),
    }
}

fn failure_page(text: &Text, files: &DirFiles, detail: &str) -> String {
    let mut body = String::new();
    for f in check::check_theme(files)
        .iter()
        .filter(|f| f.level == super::finding::Level::Error)
    {
        body.push_str(&f.render(text));
        body.push('\n');
    }
    if body.is_empty() {
        detail.clone_into(&mut body);
    }
    problem_page(text, &body)
}

fn problem_page(text: &Text, body: &str) -> String {
    let esc = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    format!(
        "<!doctype html><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width\"><title>{title}</title><body style=\"font:16px/1.5 system-ui;max-width:60rem;margin:2rem auto;padding:0 1rem\"><h1>{title}</h1><p>{hint}</p><pre style=\"white-space:pre-wrap\">{}</pre>",
        esc(body),
        title = esc(&text.t(key!("cli.theme.dev.problem_title"), &[])),
        hint = esc(&text.t(key!("cli.theme.dev.problem_hint"), &[])),
    )
}

#[cfg(test)]
mod tests {
    use super::loopback_host;

    #[test]
    fn only_loopback_names() {
        for ok in ["127.0.0.1:4000", "localhost", "localhost:80", "[::1]:3000"] {
            assert!(loopback_host(ok), "{ok}");
        }
        for bad in [
            "evil.example",
            "127.0.0.1.evil.example:80",
            "",
            "10.0.0.1:80",
        ] {
            assert!(!loopback_host(bad), "{bad}");
        }
    }
}
