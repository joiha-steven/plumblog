//! `blog update` against a fake GitHub on loopback: a copy of a binary in a temp dir is
//! replaced, never the test runner.

use crate::TempDir;
use crate::fake_release::{FakeRelease, gzip, tar, tar_entry};
use plumblog_cli::release::Version;
use plumblog_cli::release::fetch::Source;
use plumblog_cli::release::minisign::SecretKey;
use plumblog_cli::release::reason::Reason;
use plumblog_cli::release::sha256_hex;
use plumblog_cli::release::update::{Update, archive_name, run};
use plumblog_cli::setup::Failure;
use plumblog_cli::text::{Code, Text};
use std::path::PathBuf;

const TARGET: &str = "x86_64-unknown-linux-musl";
const CURRENT: Version = Version(0, 1, 0);
const OLD: &[u8] = b"#!old blog\n";
const NEW: &[u8] = b"#!new blog\n";

struct Setup {
    gh: FakeRelease,
    key: SecretKey,
    dir: TempDir,
    exe: PathBuf,
}

/// Release `v` on the fake: archive, SHA256SUMS signed by `key`.
fn publish(gh: &FakeRelease, key: &SecretKey, v: Version, binary: &[u8]) -> Vec<u8> {
    let (name, _) = archive_name(v, TARGET);
    let gz = gzip(&tar(&[
        tar_entry("blog", b'0', "", binary),
        tar_entry("LICENSE", b'0', "", b"AGPL"),
    ]));
    let sums = format!("{}  {name}\n", sha256_hex(&gz));
    gh.file(&v.tag(), &name, &gz);
    gh.file(&v.tag(), "SHA256SUMS", sums.as_bytes());
    gh.file(
        &v.tag(),
        "SHA256SUMS.minisig",
        key.sign(sums.as_bytes(), "t").as_bytes(),
    );
    gz
}

fn setup(latest: Version) -> Setup {
    let gh = FakeRelease::start();
    let key = SecretKey::generate().unwrap();
    publish(&gh, &key, latest, NEW);
    gh.latest(&latest.tag());
    let dir = TempDir::new();
    let exe = dir.0.join("blog");
    std::fs::write(&exe, OLD).unwrap();
    Setup { gh, key, dir, exe }
}

fn update(
    s: &Setup,
    to: Option<Version>,
    check: bool,
    lang: &str,
) -> (Result<(), Failure>, String) {
    update_with_key(s, &s.key.public().to_file(), to, check, lang)
}

fn update_with_key(
    s: &Setup,
    key: &str,
    to: Option<Version>,
    check: bool,
    lang: &str,
) -> (Result<(), Failure>, String) {
    let u = Update {
        current: CURRENT,
        target: TARGET.into(),
        key: key.into(),
        exe: s.exe.clone(),
        source: Source::local(&s.gh.base).unwrap(),
        to,
        check,
    };
    let mut out = Vec::new();
    let r = run(&u, &Text::new(lang), &mut out);
    (r, String::from_utf8(out).unwrap())
}

fn failed(r: &(Result<(), Failure>, String), want: Code) -> String {
    match &r.0 {
        Ok(()) => panic!("expected {want:?}\n{}", r.1),
        Err(f) => {
            assert_eq!(f.code, want, "{}", f.message);
            f.message.clone()
        }
    }
}

/// The executable is untouched and no temp file is left next to it.
fn unchanged(s: &Setup) {
    assert_eq!(std::fs::read(&s.exe).unwrap(), OLD);
    let names: Vec<_> = std::fs::read_dir(&s.dir.0)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(names.len(), 1, "{names:?}");
}

#[test]
fn a_good_signature_updates_the_binary() {
    let s = setup(Version(0, 2, 0));
    let (r, out) = update(&s, None, false, "en");
    r.unwrap();
    assert_eq!(std::fs::read(&s.exe).unwrap(), NEW);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&s.exe).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o755);
    }
    assert!(out.contains("from 0.1.0 to 0.2.0"), "{out}");
    assert!(
        out.contains("git pull") && out.contains("blog deploy"),
        "{out}"
    );
    // Windows moves the running exe aside to `blog.old.exe` for a later run to delete; it is the
    // only file besides the new binary. Elsewhere the new binary is the only file.
    let mut names: Vec<_> = std::fs::read_dir(&s.dir.0)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    names.sort();
    #[cfg(windows)]
    {
        let old = s.dir.0.join(plumblog_cli::release::update::OLD_EXE);
        assert_eq!(std::fs::read(&old).unwrap(), OLD, "the old binary is kept");
        assert_eq!(names.len(), 2, "{names:?}");
    }
    #[cfg(not(windows))]
    assert_eq!(names.len(), 1, "no temp file left: {names:?}");
    for seen in s.gh.seen() {
        assert!(seen.headers.iter().all(|(k, _)| k != "authorization"));
    }
}

#[test]
fn vi_update_output() {
    let s = setup(Version(0, 2, 0));
    let (r, out) = update(&s, None, false, "vi");
    r.unwrap();
    assert!(out.contains("git pull"), "{out}");
    eprintln!("vi update:\n{out}");
}

#[test]
fn a_bad_signature_is_refused() {
    let s = setup(Version(0, 2, 0));
    let tag = Version(0, 2, 0).tag();
    let forged = s.key.sign(b"another file list", "t");
    s.gh.file(&tag, "SHA256SUMS.minisig", forged.as_bytes());
    let msg = failed(&update(&s, None, false, "en"), Code::UpdateSignatureInvalid);
    assert!(
        msg.contains(&Reason::SignatureBad.render(&Text::new("en"))),
        "{msg}"
    );
    unchanged(&s);
}

#[test]
fn a_missing_signature_is_refused() {
    let s = setup(Version(0, 2, 0));
    let tag = Version(0, 2, 0).tag();
    s.gh.route(
        &format!("/download/{tag}/SHA256SUMS.minisig"),
        404,
        &[],
        b"",
    );
    failed(&update(&s, None, false, "en"), Code::UpdateSignatureMissing);
    unchanged(&s);
}

#[test]
fn a_signature_by_another_key_is_refused() {
    let s = setup(Version(0, 2, 0));
    let other = SecretKey::generate().unwrap();
    publish(&s.gh, &other, Version(0, 2, 0), NEW);
    let msg = failed(&update(&s, None, false, "en"), Code::UpdateKeyMismatch);
    let want = plumblog_cli::release::minisign::key_id_hex(&s.key.public().key_id);
    assert!(msg.contains(&want), "{msg}");
    unchanged(&s);
}

#[test]
fn an_archive_that_does_not_match_the_signed_list_is_refused() {
    let s = setup(Version(0, 2, 0));
    let v = Version(0, 2, 0);
    let (name, _) = archive_name(v, TARGET);
    s.gh.file(
        &v.tag(),
        &name,
        &gzip(&tar(&[tar_entry("blog", b'0', "", b"evil")])),
    );
    let msg = failed(&update(&s, None, false, "en"), Code::UpdateHashMismatch);
    assert!(msg.contains(&name), "{msg}");
    unchanged(&s);
    let msg = failed(&update(&s, None, false, "vi"), Code::UpdateHashMismatch);
    eprintln!("vi update hash mismatch:\n{msg}");
}

#[test]
fn a_downgrade_is_refused_unless_asked_for() {
    let s = setup(Version(0, 0, 9));
    let msg = failed(&update(&s, None, false, "en"), Code::UpdateDowngrade);
    assert!(msg.contains("--to v0.0.9"), "{msg}");
    unchanged(&s);
    let (r, out) = update(&s, Some(Version(0, 0, 9)), false, "en");
    r.unwrap();
    assert!(out.contains("to 0.0.9"), "{out}");
    assert_eq!(std::fs::read(&s.exe).unwrap(), NEW);
}

#[test]
fn check_only_reports() {
    let s = setup(Version(0, 2, 0));
    let (r, out) = update(&s, None, true, "en");
    r.unwrap();
    assert!(
        out.contains("0.2.0") && out.contains("blog update"),
        "{out}"
    );
    let s2 = setup(CURRENT);
    let (r, out2) = update(&s2, None, true, "en");
    r.unwrap();
    assert!(out2.contains("No newer blog"), "{out2}");
    for s in [&s, &s2] {
        unchanged(s);
        assert_eq!(s.gh.seen().len(), 1, "only the latest-release call");
    }
}

#[test]
fn the_placeholder_key_refuses_before_any_download() {
    let s = setup(Version(0, 2, 0));
    let msg = failed(
        &update_with_key(&s, "PLACEHOLDER\n", None, false, "en"),
        Code::UpdateNoKey,
    );
    assert!(msg.contains("releases"), "{msg}");
    assert!(s.gh.seen().is_empty());
    unchanged(&s);
}

#[test]
fn a_redirect_to_another_host_is_refused() {
    let s = setup(Version(0, 2, 0));
    let v = Version(0, 2, 0);
    let (name, _) = archive_name(v, TARGET);
    let path = format!("/download/{}/{name}", v.tag());
    s.gh.route(
        &path,
        302,
        &[("Location", "https://evil.example/blog.tar.gz")],
        b"",
    );
    failed(&update(&s, None, false, "en"), Code::ReleaseRedirectRefused);
    unchanged(&s);
    // A loopback hop on the test server is followed, as GitHub's asset hop is.
    let real = format!("{}/download/{}/real", s.gh.base, v.tag());
    let gz = publish(&s.gh, &s.key, v, NEW);
    s.gh.file(&v.tag(), "real", &gz);
    s.gh.route(&path, 302, &[("Location", real.as_str())], b"");
    update(&s, None, false, "en").0.unwrap();
    assert_eq!(std::fs::read(&s.exe).unwrap(), NEW);
}
