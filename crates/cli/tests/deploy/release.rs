//! `blog deploy` with the release bundle (issue #21): downloaded from a fake GitHub on
//! loopback, checked against the baked hash, unpacked with guards, cached and reused.

use super::{Ran, code, deploy_with, en, ok, set_up};
use crate::TempDir;
use crate::fake_cf::Fake;
use crate::fake_release::{FakeRelease, bundle_json, files_under, gzip, tar, tar_entry};
use plumblog_cli::deploy::Options;
use plumblog_cli::migrations;
use plumblog_cli::release::fetch::{RELEASES_PAGE, Source};
use plumblog_cli::release::{Release, Version, sha256_hex};
use plumblog_cli::text::{Code, Text};
use std::path::{Path, PathBuf};
use std::process::Command;

fn tag() -> String {
    Version::current().tag()
}

fn file_name() -> String {
    format!("plumblog-bundle-{}.tar.gz", tag())
}

/// The bundle as a release archive made by the system `tar`, as the release workflow does.
fn release_archive(bundle: &Path, version: &str) -> Vec<u8> {
    let files = files_under(bundle);
    std::fs::write(bundle.join("bundle.json"), bundle_json(version, &files)).unwrap();
    let out = bundle.with_extension("tar.gz");
    let st = Command::new("tar")
        .arg("-czf")
        .arg(&out)
        .arg("-C")
        .arg(bundle)
        .args(["worker", "assets", "migrations", "bundle.json"])
        // macOS tar would add AppleDouble `._*` entries for extended attributes.
        .env("COPYFILE_DISABLE", "1")
        .status()
        .unwrap();
    assert!(st.success());
    std::fs::read(out).unwrap()
}

fn release(gh: &FakeRelease, cache: &Path, sha256: &str) -> Release {
    Release {
        version: Version::current(),
        sha256: sha256.into(),
        source: Source::local(&gh.base).unwrap(),
        cache: cache.to_path_buf(),
    }
}

fn deploy_release(fake: &Fake, dir: &TempDir, r: Release, text: &Text) -> Ran {
    let opts = Options {
        release: Some(r),
        ..Options::default()
    };
    deploy_with(fake, dir, &opts, &migrations::bundled(), text)
}

/// No write reached Cloudflare after request number `from`.
fn nothing_deployed(fake: &Fake, from: usize) {
    let writes: Vec<String> = fake.requests()[from..]
        .iter()
        .filter(|r| r.method != "GET")
        .map(|r| format!("{} {}", r.method, r.path))
        .collect();
    assert!(writes.is_empty(), "{writes:?}");
}

fn message(r: &Ran) -> String {
    r.result
        .as_ref()
        .err()
        .map(|f| f.message.clone())
        .unwrap_or_default()
}

#[test]
fn downloads_verifies_caches_and_reuses_the_bundle() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    let gz = release_archive(&bundle, &Version::current().to_string());
    let gh = FakeRelease::start();
    gh.file(&tag(), &file_name(), &gz);
    let cache = dir.0.join("cache");
    let path = format!("/download/{}/{}", tag(), file_name());

    let r = deploy_release(&fake, &dir, release(&gh, &cache, &sha256_hex(&gz)), &en());
    ok(&r);
    assert!(
        r.out.contains("Downloading the release bundle"),
        "{}",
        r.out
    );
    assert!(r.out.contains("matches this blog"), "{}", r.out);
    assert_eq!(gh.hits(&path), 1);
    let cached = cache.join(tag()).join(file_name());
    assert_eq!(std::fs::read(&cached).unwrap(), gz);
    let unpacked = cache.join(tag()).join("bundle");
    assert!(unpacked.join("worker/index.js").is_file());
    assert!(unpacked.join("bundle.json").is_file());
    // Public downloads: no credential of any kind goes to GitHub.
    for s in gh.seen() {
        assert!(
            s.headers
                .iter()
                .all(|(k, _)| k != "authorization" && k != "cookie")
        );
    }

    let r = deploy_release(&fake, &dir, release(&gh, &cache, &sha256_hex(&gz)), &en());
    ok(&r);
    assert!(
        r.out.contains("Using the downloaded release bundle"),
        "{}",
        r.out
    );
    assert_eq!(gh.hits(&path), 1, "the verified cache is reused");
    // A damaged unpacked copy is replaced from the verified archive.
    std::fs::write(unpacked.join("worker/index.js"), "tampered").unwrap();
    ok(&deploy_release(
        &fake,
        &dir,
        release(&gh, &cache, &sha256_hex(&gz)),
        &en(),
    ));
    assert_ne!(
        std::fs::read(unpacked.join("worker/index.js")).unwrap(),
        b"tampered"
    );
}

#[test]
fn a_tampered_bundle_is_refused_with_a_plain_message_and_nothing_deployed() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    let gz = release_archive(&bundle, &Version::current().to_string());
    let gh = FakeRelease::start();
    let mut evil = gz.clone();
    let n = evil.len();
    evil[n / 2] ^= 0x55;
    gh.file(&tag(), &file_name(), &evil);
    let cache = dir.0.join("cache");
    let before = fake.requests().len();

    let r = deploy_release(&fake, &dir, release(&gh, &cache, &sha256_hex(&gz)), &en());
    assert_eq!(code(&r), Code::ReleaseHashMismatch);
    let msg = message(&r);
    let vdir = cache.join(tag());
    for want in [
        sha256_hex(&evil),
        sha256_hex(&gz),
        vdir.display().to_string(),
        "blog update".into(),
        RELEASES_PAGE.into(),
        "Nothing was deployed".into(),
    ] {
        assert!(msg.contains(&want), "{want}: {msg}");
    }
    nothing_deployed(&fake, before);
    assert!(
        !vdir.join(file_name()).exists(),
        "a bad download is never cached"
    );

    // A cached archive changed on disk is refused the same way.
    std::fs::create_dir_all(&vdir).unwrap();
    std::fs::write(vdir.join(file_name()), &evil).unwrap();
    gh.file(&tag(), &file_name(), &gz);
    let r = deploy_release(
        &fake,
        &dir,
        release(&gh, &cache, &sha256_hex(&gz)),
        &Text::new("vi"),
    );
    assert_eq!(code(&r), Code::ReleaseHashMismatch);
    eprintln!("vi hash mismatch:\n{}", message(&r));
    nothing_deployed(&fake, before);
}

/// Serves `entries` (plus a valid bundle.json for the regular ones) with a matching hash.
fn hostile(entries: Vec<Vec<u8>>, files: &[(String, Vec<u8>)], version: &str) -> Vec<u8> {
    let mut all = entries;
    all.push(tar_entry(
        "bundle.json",
        b'0',
        "",
        &bundle_json(version, files),
    ));
    gzip(&tar(&all))
}

fn refused(gz: &[u8], want: Code) -> String {
    let (fake, dir, _) = set_up(&migrations::bundled());
    let gh = FakeRelease::start();
    gh.file(&tag(), &file_name(), gz);
    let cache: PathBuf = dir.0.join("cache");
    let before = fake.requests().len();
    let r = deploy_release(&fake, &dir, release(&gh, &cache, &sha256_hex(gz)), &en());
    assert_eq!(code(&r), want, "{}", message(&r));
    nothing_deployed(&fake, before);
    assert!(!cache.join(tag()).join("bundle").exists());
    assert!(!dir.0.join("evil").exists() && !Path::new("/tmp/plumblog-evil").exists());
    message(&r)
}

#[test]
fn extraction_refuses_escapes_links_and_extra_paths() {
    let v = Version::current().to_string();
    let js = vec![("worker/index.js".to_string(), b"x".to_vec())];
    let ok_entry = || tar_entry("worker/index.js", b'0', "", b"x");
    for (entry, reason) in [
        (
            tar_entry("worker/../../evil", b'0', "", b"x"),
            "unsafe path",
        ),
        (
            tar_entry("/tmp/plumblog-evil", b'0', "", b"x"),
            "unsafe path",
        ),
        (
            tar_entry("worker/link", b'2', "/etc/passwd", b""),
            "a symlink",
        ),
        (
            tar_entry("worker/hard", b'1', "worker/index.js", b""),
            "a hard link",
        ),
        (tar_entry("worker/dev", b'3', "", b""), "a device file"),
        (
            tar_entry("extra.txt", b'0', "", b"x"),
            "not allowed in this archive",
        ),
    ] {
        let msg = refused(
            &hostile(vec![ok_entry(), entry], &js, &v),
            Code::ReleaseArchiveInvalid,
        );
        assert!(msg.contains(reason), "{reason}: {msg}");
    }
    // A file the manifest does not list, and a listed file that is missing.
    let msg = refused(
        &hostile(vec![ok_entry()], &[], &v),
        Code::ReleaseArchiveInvalid,
    );
    assert!(msg.contains("not listed in bundle.json"), "{msg}");
    let mut two = js.clone();
    two.push(("assets/a.css".into(), b"a".to_vec()));
    let msg = refused(
        &hostile(vec![ok_entry()], &two, &v),
        Code::ReleaseArchiveInvalid,
    );
    assert!(msg.contains("not in the archive"), "{msg}");
}

#[test]
fn a_bundle_json_version_mismatch_is_refused() {
    let js = vec![("worker/index.js".to_string(), b"x".to_vec())];
    let entry = tar_entry("worker/index.js", b'0', "", b"x");
    let msg = refused(
        &hostile(vec![entry], &js, "99.0.0"),
        Code::ReleaseVersionMismatch,
    );
    assert!(
        msg.contains("99.0.0") && msg.contains(&Version::current().to_string()),
        "{msg}"
    );
}

#[test]
fn a_dev_build_without_flags_refuses_before_any_call() {
    let (fake, dir, _) = set_up(&migrations::bundled());
    let before = fake.requests().len();
    let r = deploy_with(
        &fake,
        &dir,
        &Options::default(),
        &migrations::bundled(),
        &en(),
    );
    assert_eq!(code(&r), Code::ReleaseDevBuild);
    let msg = message(&r);
    assert!(
        msg.contains("--build") && msg.contains(RELEASES_PAGE),
        "{msg}"
    );
    assert_eq!(fake.requests().len(), before);
}

#[test]
fn a_redirect_to_another_host_is_refused() {
    let (fake, dir, _) = set_up(&migrations::bundled());
    let gh = FakeRelease::start();
    let path = format!("/download/{}/{}", tag(), file_name());
    gh.route(
        &path,
        302,
        &[("Location", "https://evil.example/bundle.tar.gz")],
        b"",
    );
    let before = fake.requests().len();
    let r = deploy_release(
        &fake,
        &dir,
        release(&gh, &dir.0.join("c"), &"0".repeat(64)),
        &en(),
    );
    assert_eq!(code(&r), Code::ReleaseRedirectRefused);
    assert!(message(&r).contains("evil.example"));
    nothing_deployed(&fake, before);
}
