//! `blog theme new | check | dev` and `blog plugin check` against real folders in a temp dir.

use plumblog_cli::text::{Code, Text};
use plumblog_cli::theme::check::{check_plugin, check_theme};
use plumblog_cli::theme::files::DirFiles;
use plumblog_cli::theme::finding::{Finding, Kind, Level};
use plumblog_cli::theme::{self, dev};
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

fn en() -> Text {
    Text::new("en")
}

struct Temp(PathBuf);

impl Temp {
    fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "plumblog-theme-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A fresh copy of the default theme in a temp folder, made by `blog theme new`.
fn fresh_theme(name: &str) -> (Temp, PathBuf) {
    let t = Temp::new();
    let dir = t.0.join(name);
    theme::new(&en(), &dir, None).unwrap();
    (t, dir)
}

fn findings(dir: &Path) -> Vec<Finding> {
    check_theme(&DirFiles::read(dir).unwrap())
}

fn has(f: &[Finding], kind: Kind, file: &str, line: Option<usize>) -> bool {
    f.iter()
        .any(|x| x.kind == kind && x.file == file && (line.is_none() || x.line == line))
}

fn edit(dir: &Path, rel: &str, f: impl FnOnce(String) -> String) {
    let p = dir.join(rel);
    let old = std::fs::read_to_string(&p).unwrap();
    std::fs::write(p, f(old)).unwrap();
}

#[test]
fn new_copies_a_loadable_theme() {
    let (_t, dir) = fresh_theme("my-theme");
    let manifest = std::fs::read_to_string(dir.join("theme.toml")).unwrap();
    assert!(manifest.contains("name = \"my-theme\""), "{manifest}");
    assert!(dir.join("assets/fonts/inter-latin.woff2").is_file());
    // Same bytes as the repo's theme, apart from theme.toml.
    for rel in [
        "style.css",
        "templates/base.html",
        "assets/fonts/inter-latin.woff2",
    ] {
        assert_eq!(
            std::fs::read(dir.join(rel)).unwrap(),
            std::fs::read(repo().join("themes/minimal").join(rel)).unwrap(),
            "{rel}"
        );
    }
    let out = theme::check_theme(&en(), &dir).unwrap();
    assert!(out.contains("No problems found"), "{out}");
    assert!(findings(&dir).is_empty());
}

#[test]
fn new_uses_the_name_flag_and_refuses_bad_names() {
    let t = Temp::new();
    theme::new(&en(), &t.0.join("a"), Some("other-name")).unwrap();
    let m = std::fs::read_to_string(t.0.join("a/theme.toml")).unwrap();
    assert!(m.contains("name = \"other-name\""));
    let (code, _) = theme::new(&en(), &t.0.join("b"), Some("Bad Name")).unwrap_err();
    assert_eq!(code, Code::ThemeNameInvalid);
    assert!(!t.0.join("b").exists());
}

#[test]
fn new_refuses_a_folder_that_is_not_empty() {
    let t = Temp::new();
    std::fs::write(t.0.join("keep.txt"), "mine").unwrap();
    let (code, msg) = theme::new(&en(), &t.0, Some("x")).unwrap_err();
    assert_eq!(code, Code::ThemeDirNotEmpty, "{msg}");
    assert_eq!(
        std::fs::read_to_string(t.0.join("keep.txt")).unwrap(),
        "mine"
    );
    // An empty folder is fine.
    let empty = t.0.join("empty");
    std::fs::create_dir(&empty).unwrap();
    theme::new(&en(), &empty, Some("x")).unwrap();
}

#[test]
fn new_refuses_a_folder_inside_the_plumblog_checkout() {
    let t = Temp::new();
    // A stand-in for a checkout: the two files that mark the workspace.
    for f in ["crates/cli/Cargo.toml", "themes/minimal/theme.toml"] {
        let p = t.0.join(f);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, "").unwrap();
    }
    for dir in [t.0.join("themes/mine"), t.0.join("deep/er/mine")] {
        let (code, _) = theme::new(&en(), &dir, Some("mine")).unwrap_err();
        assert_eq!(code, Code::ThemeDirTracked, "{}", dir.display());
        assert!(!dir.exists());
    }
    // The real checkout too, reached through `..`.
    let (code, _) =
        theme::new(&en(), &repo().join("themes/../zz-never-made"), Some("x")).unwrap_err();
    assert_eq!(code, Code::ThemeDirTracked);
}

#[test]
fn check_passes_the_default_theme() {
    let out = theme::check_theme(&en(), &repo().join("themes/minimal")).unwrap();
    assert!(out.contains("No problems found"), "{out}");
}

#[test]
fn check_flags_a_template_error_with_file_and_line() {
    let (_t, dir) = fresh_theme("t");
    edit(&dir, "templates/home.html", |s| {
        format!("{s}\n{{% for x in %}}\n")
    });
    let f = findings(&dir);
    let line = std::fs::read_to_string(dir.join("templates/home.html"))
        .unwrap()
        .lines()
        .position(|l| l.contains("for x in"))
        .unwrap()
        + 1;
    assert!(
        has(&f, Kind::ThemeTemplate, "templates/home.html", Some(line)),
        "{f:?}"
    );
    let (code, msg) = theme::check_theme(&en(), &dir).unwrap_err();
    assert_eq!(code, Code::ThemeCheckFailed);
    assert!(
        msg.contains(&format!("templates/home.html:{line}")),
        "{msg}"
    );
    assert!(msg.contains("Fix:"), "{msg}");
}

#[test]
fn check_flags_an_error_that_only_shows_when_a_page_renders() {
    let (_t, dir) = fresh_theme("t");
    edit(&dir, "templates/404.html", |s| {
        format!("{s}\n{{{{ nothing_here() }}}}\n")
    });
    let f = findings(&dir);
    assert!(
        f.iter()
            .any(|x| x.kind == Kind::ThemeTemplate && x.level == Level::Error),
        "{f:?}"
    );
}

#[test]
fn check_flags_a_missing_dark_token() {
    let (_t, dir) = fresh_theme("t");
    edit(&dir, "theme.toml", |s| {
        let (head, dark) = s.split_once("[colors.dark]").unwrap();
        let dark = dark
            .lines()
            .filter(|l| !l.starts_with("syn-punct"))
            .collect::<Vec<_>>()
            .join(
                "
",
            );
        format!("{head}[colors.dark]{dark}")
    });
    let f = findings(&dir);
    let want = std::fs::read_to_string(dir.join("theme.toml"))
        .unwrap()
        .lines()
        .position(|l| l.starts_with("syn-punct"))
        .unwrap()
        + 1;
    assert!(
        has(&f, Kind::ThemeDarkToken, "theme.toml", Some(want)),
        "{f:?}"
    );
}

#[test]
fn check_flags_low_contrast_in_either_mode() {
    let (_t, dir) = fresh_theme("t");
    edit(&dir, "theme.toml", |s| {
        let (head, dark) = s.split_once("[colors.dark]").unwrap();
        format!(
            "{head}[colors.dark]{}",
            dark.replacen("text-2 = \"#a59aa2\"", "text-2 = \"#3a3338\"", 1)
        )
    });
    let f = findings(&dir);
    let hit = f
        .iter()
        .find(|x| x.kind == Kind::ThemeContrast)
        .unwrap_or_else(|| panic!("{f:?}"));
    assert_eq!(hit.file, "theme.toml");
    assert!(
        hit.args.contains(&("mode", "dark".to_owned()))
            && hit.args.contains(&("fg", "text-2".to_owned()))
    );
    assert!(hit.line.is_some());

    // The owner-changeable accent is checked at its default too.
    let (_t, dir) = fresh_theme("t");
    edit(&dir, "theme.toml", |s| {
        s.replace(
            "default = { light = \"#8a2d6f\"",
            "default = { light = \"#f0c0e0\"",
        )
    });
    let f = findings(&dir);
    assert!(f.iter().any(|x| x.kind == Kind::ThemeContrast && x.args.contains(&("fg", "accent".to_owned()))), "{f:?}");
}

#[test]
fn check_flags_css_over_the_budget() {
    let (_t, dir) = fresh_theme("t");
    // Distinct rules, so it does not compress away.
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut css = String::new();
    while css.len() < 120_000 {
        x = x
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        writeln!(
            css,
            ".c{x:x}{{margin:{}px {}px;padding:{}px}}\n",
            x % 97,
            (x >> 8) % 89,
            (x >> 16) % 83
        )
        .unwrap();
    }
    edit(&dir, "style.css", |s| format!("{s}\n{css}"));
    let f = findings(&dir);
    assert!(
        has(&f, Kind::ThemeCssSize, "style.css", None),
        "{:?}",
        f.iter().map(|x| x.kind).collect::<Vec<_>>()
    );
}

#[test]
fn check_refuses_secrets_and_stray_files() {
    let (_t, dir) = fresh_theme("t");
    std::fs::write(dir.join(".env"), "CLOUDFLARE_API_TOKEN=x\n").unwrap();
    std::fs::write(dir.join("assets/deploy.pem"), "x").unwrap();
    std::fs::write(dir.join("assets/my-secret.js"), "x").unwrap();
    std::fs::write(dir.join("notes.txt"), "todo").unwrap();
    std::fs::write(dir.join("assets/admin.css"), "body{}").unwrap();
    edit(&dir, "style.css", |s| {
        format!("{s}\n/* old token: Ab1Cd2Ef3Gh4Ij5Kl6Mn7Op8Qr9St0UvWxYz1234 */\n")
    });
    edit(&dir, "assets/minimal.js", |s| {
        format!("{s}\n// -----BEGIN RSA PRIVATE KEY-----\n")
    });
    let f = findings(&dir);
    for file in [".env", "assets/deploy.pem", "assets/my-secret.js"] {
        assert!(has(&f, Kind::FileSecretName, file, None), "{file}: {f:?}");
    }
    assert!(has(&f, Kind::FileStray, "notes.txt", None), "{f:?}");
    assert!(has(&f, Kind::FileAdmin, "assets/admin.css", None), "{f:?}");
    let css_lines = std::fs::read_to_string(dir.join("style.css"))
        .unwrap()
        .lines()
        .count();
    assert!(
        has(&f, Kind::FileSecretContent, "style.css", Some(css_lines)),
        "{f:?}"
    );
    assert!(
        f.iter()
            .any(|x| x.kind == Kind::FileSecretContent && x.file == "assets/minimal.js")
    );
    // Each is an error and the command fails, naming the file in the output.
    let (code, msg) = theme::check_theme(&en(), &dir).unwrap_err();
    assert_eq!(code, Code::ThemeCheckFailed);
    assert!(
        msg.contains("file.secret_name") && msg.contains(".env"),
        "{msg}"
    );
}

#[test]
fn check_warns_without_failing_for_hard_coded_text_and_colors() {
    let (_t, dir) = fresh_theme("t");
    edit(&dir, "templates/404.html", |s| {
        format!("{s}\n<p>Sorry about that</p>\n")
    });
    edit(&dir, "style.css", |s| {
        format!("{s}\nbody.x {{ color: #123456; }}\n")
    });
    let f = findings(&dir);
    assert!(
        f.iter().any(|x| x.kind == Kind::TextHard
            && x.level == Level::Warning
            && x.file == "templates/404.html"),
        "{f:?}"
    );
    assert!(
        f.iter().any(|x| x.kind == Kind::ThemeHardColor
            && x.level == Level::Warning
            && x.file == "style.css"),
        "{f:?}"
    );
    let out = theme::check_theme(&en(), &dir).unwrap();
    assert!(
        out.contains("warning [text.hard_coded] templates/404.html:"),
        "{out}"
    );
}

#[test]
fn check_speaks_vietnamese_with_the_same_codes() {
    let (_t, dir) = fresh_theme("t");
    std::fs::write(dir.join(".env"), "x").unwrap();
    let (code, msg) = theme::check_theme(&Text::new("vi"), &dir).unwrap_err();
    assert_eq!(code, Code::ThemeCheckFailed);
    assert!(
        msg.contains("l\u{1ed7}i [file.secret_name] .env") && msg.contains("C\u{e1}ch s\u{1eed}a:"),
        "{msg}"
    );
}

fn get(port: u16, path: &str, host: &str) -> (String, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let raw = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
    (
        head.lines().next().unwrap_or("").to_owned(),
        body.to_owned(),
    )
}

#[test]
fn dev_serves_pages_over_loopback_and_shows_an_edit_on_the_next_request() {
    let (_t, dir) = fresh_theme("t");
    let listener = dev::bind(0).unwrap();
    let port = listener.local_addr().unwrap().port();
    assert!(listener.local_addr().unwrap().ip().is_loopback());
    let (d, text) = (dir.clone(), en());
    let server = std::thread::spawn(move || dev::serve(&listener, &d, &text, Some(8)).unwrap());
    let host = format!("127.0.0.1:{port}");

    let (status, home) = get(port, "/", &host);
    assert!(status.contains("200"), "{status}");
    assert!(
        home.contains("Plum Notes") && home.contains("<!doctype html>"),
        "{home}"
    );
    assert!(!home.contains("edited-marker"));

    edit(&dir, "style.css", |s| {
        format!("{s}\n.edited-marker {{ margin: 0 }}\n")
    });
    let (_, again) = get(port, "/", &host);
    assert!(
        again.contains("edited-marker"),
        "an edit shows on the next request"
    );

    let (status, article) = get(port, "/first-post", &host);
    assert!(
        status.contains("200") && article.contains("First post"),
        "{status}"
    );
    let (status, _) = get(port, "/tag/life", &host);
    assert!(status.contains("200"), "{status}");
    let (status, _) = get(port, "/archive/2025", &host);
    assert!(status.contains("200"), "{status}");
    let (status, _) = get(port, "/nowhere", &host);
    assert!(status.contains("404"), "{status}");
    // Assets come from the folder, and a stranger's Host header is refused (DNS rebinding).
    let (status, css) = get(port, "/_theme/fonts/faces.css", &host);
    assert!(
        status.contains("200") && css.contains("@font-face"),
        "{status}"
    );
    let (status, _) = get(port, "/", "evil.example");
    assert!(status.contains("403"), "{status}");
    server.join().unwrap();
}

#[test]
fn dev_shows_the_findings_for_a_broken_theme_and_recovers() {
    let (_t, dir) = fresh_theme("t");
    let listener = dev::bind(0).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (d, text) = (dir.clone(), en());
    let server = std::thread::spawn(move || dev::serve(&listener, &d, &text, Some(3)).unwrap());
    let host = format!("127.0.0.1:{port}");
    let original = std::fs::read_to_string(dir.join("templates/home.html")).unwrap();
    std::fs::write(
        dir.join("templates/home.html"),
        format!("{original}\n{{% for x in %}}\n"),
    )
    .unwrap();
    let (status, page) = get(port, "/", &host);
    assert!(
        status.contains("500")
            && page.contains("theme.template")
            && page.contains("templates/home.html"),
        "{status} {page}"
    );
    std::fs::write(dir.join("templates/home.html"), original).unwrap();
    let (status, _) = get(port, "/", &host);
    assert!(status.contains("200"));
    // Traversal out of assets/ is refused.
    let (status, _) = get(port, "/_theme/../theme.toml", &host);
    assert!(status.contains("404"), "{status}");
    server.join().unwrap();
}

fn has_id(f: &[Finding], id: &str, file: &str) -> bool {
    f.iter().any(|x| x.kind.id() == id && x.file == file)
}

#[test]
fn check_refuses_asset_names_that_are_not_plain() {
    let (_t, dir) = fresh_theme("t");
    for name in ["a b.png", "x#1.png", "100%.png", "caf\u{e9}.png"] {
        std::fs::write(dir.join("assets").join(name), b"x").unwrap();
    }
    std::fs::create_dir(dir.join("assets/my dir")).unwrap();
    std::fs::write(dir.join("assets/my dir/ok.png"), b"x").unwrap();
    std::fs::write(dir.join("assets/fine-name_1.v2.png"), b"x").unwrap();
    let f = findings(&dir);
    for name in [
        "a b.png",
        "x#1.png",
        "100%.png",
        "caf\u{e9}.png",
        "my dir/ok.png",
    ] {
        assert!(
            has_id(&f, "file.asset_name", &format!("assets/{name}")),
            "{name}: {f:#?}"
        );
    }
    assert!(!has_id(&f, "file.asset_name", "assets/fine-name_1.v2.png"));
    let shown = f
        .iter()
        .find(|x| x.kind.id() == "file.asset_name")
        .unwrap()
        .render(&en());
    assert!(shown.contains("A-Z"), "{shown}");
}

#[test]
fn check_reports_assets_referenced_but_missing() {
    let (_t, dir) = fresh_theme("t");
    assert!(
        !findings(&dir)
            .iter()
            .any(|x| x.kind.id() == "theme.asset_missing")
    );
    // post.html is borrowed from the default theme, which loads asset("highlight.js").
    std::fs::remove_file(dir.join("templates/post.html")).unwrap();
    std::fs::remove_file(dir.join("assets/highlight.js")).unwrap();
    // A font named in faces.css and preloaded by theme.toml.
    std::fs::remove_file(dir.join("assets/fonts/inter-latin.woff2")).unwrap();
    // The theme's own template and the theme's script list.
    edit(&dir, "templates/home.html", |s| {
        format!("{s}\n<script src=\"{{{{ asset(\"gone.js\") }}}}\"></script>\n")
    });
    edit(&dir, "theme.toml", |s| {
        s.replace(
            "files = [\"assets/minimal.js\"]",
            "files = [\"assets/minimal.js\", \"assets/extra.js\"]",
        )
    });
    let f = findings(&dir);
    let missing: Vec<(String, String)> = f
        .iter()
        .filter(|x| x.kind.id() == "theme.asset_missing")
        .map(|x| {
            let a = x
                .args
                .iter()
                .find(|(k, _)| *k == "asset")
                .unwrap()
                .1
                .clone();
            (x.file.clone(), a)
        })
        .collect();
    for want in [
        ("templates/post.html", "assets/highlight.js"),
        ("assets/fonts/faces.css", "assets/fonts/inter-latin.woff2"),
        ("theme.toml", "assets/fonts/inter-latin.woff2"),
        ("templates/home.html", "assets/gone.js"),
        ("theme.toml", "assets/extra.js"),
    ] {
        assert!(
            missing
                .iter()
                .any(|(file, a)| file == want.0 && a == want.1),
            "{want:?} in {missing:#?}"
        );
    }
    assert!(
        f.iter()
            .any(|x| x.kind.id() == "theme.asset_missing" && x.level == Level::Error)
    );
}

#[test]
fn dev_serves_only_the_theme_s_own_assets() {
    let (_t, dir) = fresh_theme("t");
    std::fs::remove_file(dir.join("assets/highlight.js")).unwrap();
    let listener = dev::bind(0).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (d, text) = (dir.clone(), en());
    let server = std::thread::spawn(move || dev::serve(&listener, &d, &text, Some(2)).unwrap());
    let host = format!("127.0.0.1:{port}");
    // The live Worker serves /_theme/<name>/assets/ only: no default theme behind it.
    let (status, _) = get(port, "/_theme/highlight.js", &host);
    assert!(status.contains("404"), "{status}");
    let (status, _) = get(port, "/_theme/minimal.js", &host);
    assert!(status.contains("200"), "{status}");
    server.join().unwrap();
}

#[cfg(unix)]
#[test]
fn dev_never_follows_a_symlinked_folder_out_of_assets() {
    let (t, dir) = fresh_theme("t");
    let outside = t.0.join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("secret.txt"), "top secret").unwrap();
    std::os::unix::fs::symlink(&outside, dir.join("assets/x")).unwrap();
    std::os::unix::fs::symlink(outside.join("secret.txt"), dir.join("assets/s.txt")).unwrap();
    let listener = dev::bind(0).unwrap();
    let port = listener.local_addr().unwrap().port();
    let (d, text) = (dir.clone(), en());
    let server = std::thread::spawn(move || dev::serve(&listener, &d, &text, Some(3)).unwrap());
    let host = format!("127.0.0.1:{port}");
    for path in ["/_theme/x/secret.txt", "/_theme/s.txt"] {
        let (status, body) = get(port, path, &host);
        assert!(
            status.contains("404") && !body.contains("top secret"),
            "{path}: {status}"
        );
    }
    let (status, _) = get(port, "/_theme/minimal.js", &host);
    assert!(status.contains("200"), "{status}");
    server.join().unwrap();
}

const GHP: &str = "ghp_abcdefghijklmnopqrstuvwxyzABCDEFGHIJ";

#[test]
fn check_finds_a_token_in_an_asset_that_is_not_utf8() {
    let (_t, dir) = fresh_theme("t");
    let mut js = b"var s = '\xe9';\n".to_vec();
    js.extend_from_slice(format!("const t = \"{GHP}\";\n").as_bytes());
    std::fs::write(dir.join("assets/app.js"), js).unwrap();
    let f = findings(&dir);
    assert!(has_id(&f, "file.secret_content", "assets/app.js"), "{f:#?}");
}

#[test]
fn check_refuses_an_oversize_text_file_and_still_scans_it() {
    let (_t, dir) = fresh_theme("t");
    edit(&dir, "locales/en.toml", |s| {
        format!("{s}\n# {}\npad = \"{GHP}\"\n", "x".repeat(5 * 1024 * 1024))
    });
    let f = findings(&dir);
    assert!(has_id(&f, "file.too_large", "locales/en.toml"), "{f:#?}");
    assert!(
        has_id(&f, "file.secret_content", "locales/en.toml"),
        "{f:#?}"
    );
}

#[test]
fn check_and_push_refuse_a_theme_over_the_document_cap() {
    let (_t, dir) = fresh_theme("t");
    let max = plumblog_render::pushed::MAX_DOCUMENT;
    edit(&dir, "locales/en.toml", |s| {
        // Text, not a comment: only values reach the document (a push carries TOML as JSON).
        format!("{s}\n[pad]\nx = \"{}\"\n", "x".repeat(max))
    });
    let f = findings(&dir);
    assert!(
        f.iter()
            .any(|x| x.kind.id() == "theme.too_large" && x.level == Level::Error),
        "{f:#?}"
    );
    let Err((_, why)) = theme::store::build(&dir) else {
        panic!("built a theme over the cap");
    };
    assert!(
        why.words(&en()).contains(&max.to_string()),
        "{}",
        why.words(&en())
    );
}

fn plugin_findings(dir: &Path) -> Vec<Finding> {
    let name = dir.file_name().unwrap().to_string_lossy().into_owned();
    check_plugin(&DirFiles::read(dir).unwrap(), &name)
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.path().is_dir() {
            copy_dir(&e.path(), &dest);
        } else {
            std::fs::copy(e.path(), dest).unwrap();
        }
    }
}

fn hello() -> (Temp, PathBuf) {
    let t = Temp::new();
    let dir = t.0.join("hello");
    copy_dir(
        &repo().join("crates/render/tests/fixtures/plugins/hello"),
        &dir,
    );
    (t, dir)
}

#[test]
fn plugin_check_passes_the_bundled_plugins_and_the_fixture() {
    let (_t, dir) = hello();
    let out = theme::check_plugin(&en(), &dir).unwrap();
    assert!(out.contains("No problems found"), "{out}");
    for e in std::fs::read_dir(repo().join("plugins")).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            theme::check_plugin(&en(), &p).unwrap_or_else(|e| panic!("{}: {e:?}", p.display()));
        }
    }
}

#[test]
fn plugin_check_flags_a_bad_plugin() {
    // An option type the admin does not have.
    let (_t, dir) = hello();
    edit(&dir, "plugin.toml", |s| {
        s.replace("type = \"short_text\"", "type = \"banana\"")
    });
    let f = plugin_findings(&dir);
    assert!(
        f.iter()
            .any(|x| x.kind == Kind::PluginLoad && x.level == Level::Error),
        "{f:?}"
    );
    let (code, msg) = theme::check_plugin(&en(), &dir).unwrap_err();
    assert_eq!(code, Code::PluginCheckFailed);
    assert!(msg.contains("plugin.load"), "{msg}");

    // Fixed text in a partial.
    let (_t, dir) = hello();
    edit(&dir, "partials/footer.html", |s| {
        format!("{s}\n<p>Thanks for reading</p>\n")
    });
    let f = plugin_findings(&dir);
    assert!(
        f.iter().any(|x| x.kind == Kind::TextHard
            && x.level == Level::Error
            && x.file == "partials/footer.html"
            && x.line.is_some()),
        "{f:?}"
    );

    // Files that target the admin, secrets, and files outside the layout.
    let (_t, dir) = hello();
    std::fs::write(dir.join("assets/admin.js"), "x").unwrap();
    std::fs::write(dir.join(".env"), "x").unwrap();
    std::fs::write(dir.join("README.md"), "x").unwrap();
    edit(&dir, "assets/hello.js", |s| {
        format!("{s}\n// ghp_0123456789abcdefghijklmnopqrstuvwxyz\n")
    });
    let f = plugin_findings(&dir);
    assert!(has(&f, Kind::FileAdmin, "assets/admin.js", None), "{f:?}");
    assert!(has(&f, Kind::FileSecretName, ".env", None), "{f:?}");
    assert!(has(&f, Kind::FileStray, "README.md", None), "{f:?}");
    assert!(
        has(&f, Kind::FileSecretContent, "assets/hello.js", None),
        "{f:?}"
    );

    // A partial that is not valid in a slot (`extends`) fails the load.
    let (_t, dir) = hello();
    std::fs::write(
        dir.join("partials/footer.html"),
        "{% extends \"base.html\" %}",
    )
    .unwrap();
    let f = plugin_findings(&dir);
    assert!(
        f.iter()
            .any(|x| x.kind == Kind::PluginLoad && x.file == "partials/footer.html"),
        "{f:?}"
    );
}

/// The Worker loads JSON only; `blog` converts, so a TOML mistake must reach the author here,
/// naming the file and the line, not as a failure on the site.
#[test]
fn check_names_the_toml_file_and_line_that_does_not_convert() {
    let detail = |f: &[Finding], file: &str| {
        f.iter()
            .find(|x| x.file == file && x.level == Level::Error)
            .map(|x| format!("{x:?}"))
            .unwrap_or_default()
    };
    for (file, bad) in [
        ("theme.toml", "name = \"t\"\nbroken = =\n"),
        ("locales/en.toml", "[a]\nbroken = =\n"),
    ] {
        let (_t, dir) = fresh_theme("t");
        edit(&dir, file, |s| format!("{s}\n{bad}"));
        let f = findings(&dir);
        assert!(
            f.iter()
                .any(|x| x.kind == Kind::ThemeLoad && x.file == file && x.line.is_some()),
            "{file}: {f:#?}"
        );
        assert!(detail(&f, file).contains("TOML parse error"), "{file}");
    }
    // A mistyped manifest value is a type error: the TOML position, not a JSON one.
    let (_t, dir) = fresh_theme("t");
    edit(&dir, "theme.toml", |s| {
        s.replacen("slots = [", "slots = 5\n#[", 1)
    });
    let f = findings(&dir);
    assert!(has(&f, Kind::ThemeLoad, "theme.toml", Some(7)), "{f:#?}");
    // Plugins too.
    let (_t, dir) = hello();
    edit(&dir, "plugin.toml", |s| format!("{s}\nbroken = =\n"));
    let f = plugin_findings(&dir);
    assert!(
        f.iter()
            .any(|x| x.kind == Kind::PluginLoad && x.file == "plugin.toml" && x.line.is_some()),
        "{f:#?}"
    );
}

/// A push carries TOML as JSON under `<path>.json`, never as TOML text.
#[test]
fn push_builds_the_api_2_document_without_toml_text() {
    let (_t, dir) = fresh_theme("t");
    let built = theme::store::build(&dir).unwrap_or_else(|e| panic!("{e:?}"));
    assert_eq!(built.doc.api, plumblog_render::THEME_API);
    assert_eq!(built.doc.api, 2);
    let files = &built.doc.files;
    assert!(files.contains_key("theme.toml.json") && files.contains_key("locales/en.toml.json"));
    assert!(
        !files
            .keys()
            .any(|p| plumblog_render::convert::json_name(p).is_some()),
        "{files:?}"
    );
    assert!(files["locales/en.toml.json"].starts_with('{'));
}
