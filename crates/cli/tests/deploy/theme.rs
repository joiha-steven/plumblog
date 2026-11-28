//! `blog theme push | rollback | list` and the theme a later `blog deploy` carries, against
//! the fake Cloudflare.

use super::{FakeClock, NOW, P, V2, V3, active, en, load, metadata, set_up, set_up_domain};
use crate::fake_cf::Fake;
use crate::{GOOD, TempDir, client};
use plumblog_cli::deploy::{self, Options, Runtime};
use plumblog_cli::migrations;
use plumblog_cli::setup::{Context, Failure as RunFailure};
use plumblog_cli::text::{Code, Text};
use plumblog_cli::theme::push::{self, PushOptions, RollbackOptions};
use plumblog_cli::theme::store::{self, STORE_DIR};
use plumblog_render::PushedTheme;
use std::path::{Path, PathBuf};

const V4: &str = "00000000-0000-4000-8000-000000000004";
const V5: &str = "00000000-0000-4000-8000-000000000005";
const DOC: &str = "/_theme/paper/theme.json";
const JS: &str = "/_theme/paper/assets/minimal.js";

struct Ran {
    result: Result<(), RunFailure>,
    out: String,
}

fn code(r: &Ran) -> Code {
    match &r.result {
        Ok(()) => panic!("expected a failure\n{}", r.out),
        Err(e) => e.code,
    }
}

fn ok(r: &Ran) {
    if let Err(e) = &r.result {
        panic!("{:?}: {}\n{}", e.code, e.message, r.out);
    }
}

#[derive(Clone, Copy)]
enum Cmd<'a> {
    Push(Option<&'a Path>),
    Deploy { theme_default: bool },
    Rollback(Option<&'a str>),
}

/// One command at minute `minute` (each run gets its own record time).
fn run(fake: &Fake, dir: &TempDir, bundle: &Path, minute: i64, cmd: Cmd) -> Ran {
    run_in(fake, dir, bundle, minute, cmd, &en())
}

/// [`run`] in the language of `text`.
fn run_in(fake: &Fake, dir: &TempDir, bundle: &Path, minute: i64, cmd: Cmd, text: &Text) -> Ran {
    let mut c = client(fake, GOOD);
    let migs = migrations::bundled();
    let cx = Context {
        dir: &dir.0,
        migrations: &migs,
        now_ms: NOW + minute * 60_000,
        text,
    };
    let clock = FakeClock::default();
    let rt = Runtime {
        clock: &clock,
        smoke_base: Some(fake.base.clone()),
        smoke_budget: deploy::SMOKE_BUDGET,
    };
    let source = Options {
        bundle: Some(bundle.to_path_buf()),
        ..Options::default()
    };
    let mut buf = Vec::new();
    let result = match cmd {
        Cmd::Push(theme) => {
            let opts = PushOptions {
                dir: theme.map(Path::to_path_buf),
                source,
            };
            push::push(&mut c, None, &opts, &cx, &rt, &mut buf)
        }
        Cmd::Deploy { theme_default } => {
            let opts = Options {
                theme: if theme_default {
                    deploy::ThemeCarry::Drop
                } else {
                    deploy::ThemeCarry::Keep
                },
                ..source
            };
            deploy::run(&mut c, None, &opts, &cx, &rt, &mut buf)
        }
        Cmd::Rollback(to) => {
            let opts = RollbackOptions {
                to: to.map(str::to_string),
                allow_older: false,
            };
            push::rollback(&mut c, None, &opts, &cx, &rt, &mut buf)
        }
    };
    Ran {
        result,
        out: String::from_utf8(buf).unwrap(),
    }
}

/// `blog theme new` into `<dir>/paper`.
fn paper(dir: &TempDir) -> PathBuf {
    let t = dir.0.join("paper");
    plumblog_cli::theme::new(&en(), &t, Some("paper")).unwrap();
    t
}

fn theme_binding(fake: &Fake, version: &str) -> Option<String> {
    metadata(fake, version)["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["name"] == "PLUMBLOG_THEME")
        .map(|b| {
            assert_eq!(b["type"], "plain_text");
            b["text"].as_str().unwrap().to_string()
        })
}

/// `PLUMBLOG_THEME_TAG` of `version`, if set.
fn tag_binding(fake: &Fake, version: &str) -> Option<String> {
    metadata(fake, version)["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["name"] == "PLUMBLOG_THEME_TAG")
        .map(|b| {
            assert_eq!(b["type"], "plain_text");
            b["text"].as_str().unwrap().to_string()
        })
}

/// The tag the binding must carry for the theme `version` serves.
fn served_tag(fake: &Fake, version: &str) -> String {
    plumblog_render::pushed::theme_tag(&served_doc(fake, version).hash).to_string()
}

/// Static asset paths of `version` under `/_theme/`.
fn theme_assets(fake: &Fake, version: &str) -> Vec<String> {
    let mut v: Vec<String> = fake.state().version_assets[version]
        .keys()
        .filter(|p| p.starts_with("/_theme/"))
        .cloned()
        .collect();
    v.sort();
    v
}

/// `theme.json` as `version` serves it.
fn served_doc(fake: &Fake, version: &str) -> PushedTheme {
    let st = fake.state();
    let hash = &st.version_assets[version][DOC];
    PushedTheme::parse(std::str::from_utf8(&st.asset_bytes[hash]).unwrap(), "paper").unwrap()
}

fn requests_since(fake: &Fake, skip: usize) -> Vec<String> {
    fake.requests()[skip..]
        .iter()
        .map(|r| format!("{} {}", r.method, r.path))
        .collect()
}

#[test]
fn push_uploads_the_theme_and_binds_it() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    let theme = paper(&dir);
    let skip = fake.requests().len();
    let r = run(&fake, &dir, &bundle, 1, Cmd::Push(Some(&theme)));
    ok(&r);
    assert!(r.out.contains("Pushing theme paper"), "{}", r.out);
    assert_eq!(active(&fake), V2);
    assert_eq!(theme_binding(&fake, V2).as_deref(), Some("paper"));
    assert_eq!(tag_binding(&fake, V2), Some(served_tag(&fake, V2)));
    let paths = theme_assets(&fake, V2);
    assert!(paths.contains(&DOC.to_string()), "{paths:?}");
    assert!(paths.contains(&JS.to_string()), "{paths:?}");
    assert!(
        paths
            .iter()
            .any(|p| p.starts_with("/_theme/paper/assets/fonts/")),
        "{paths:?}"
    );
    // The bundle's own assets are still there.
    assert!(fake.state().version_assets[V2].contains_key("/admin/app.js"));
    // theme.json: every text file, assets listed by SHA-256, hash over both.
    let doc = served_doc(&fake, V2);
    assert!(
        doc.files.contains_key("theme.toml.json")
            && !doc.files.contains_key("theme.toml")
            && doc.files.contains_key("templates/base.html")
    );
    // Below assets/ the document carries only the file the loader reads (`fonts.faces`).
    let carried: Vec<_> = doc
        .files
        .keys()
        .filter(|p| p.starts_with("assets/"))
        .collect();
    assert_eq!(carried, ["assets/fonts/faces.css"]);
    assert!(doc.assets.contains_key("minimal.js"));
    // What the version serves loads the way the Worker loads it (issue #110).
    let default = plumblog_cli::theme::files::Embedded::default_theme();
    if let Err(e) = plumblog_render::Theme::load(&doc, Some(&default), Vec::new(), "/_theme") {
        panic!("the served theme.json does not load: {}", e.detail);
    }
    let rec = load(&dir).deploys.last().unwrap().clone();
    assert_eq!(rec.outcome, "deployed");
    assert_eq!(rec.kind.as_deref(), Some("theme_push"));
    assert_eq!(rec.previous.as_deref(), Some(P));
    let t = rec.theme.unwrap();
    assert_eq!(
        (t.name.as_str(), t.hash.as_str()),
        ("paper", doc.hash.as_str())
    );
    // Kept locally, and the lock is gone.
    assert!(
        dir.0
            .join(STORE_DIR)
            .join(&doc.hash)
            .join("theme.json")
            .is_file()
    );
    assert!(!dir.0.join(deploy::LOCK_FILE).exists());
    // No bookmark or export for a theme: the database is not touched.
    let calls = requests_since(&fake, skip);
    assert!(
        !calls.iter().any(|r| r.contains("time_travel")),
        "{calls:?}"
    );
    let list = push::list(&en(), &dir.0).unwrap();
    assert!(
        list.contains(&format!("{V2}  deployed  theme push: paper")),
        "{list}"
    );
}

#[test]
fn push_refuses_a_theme_with_check_errors_before_any_call() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    let theme = paper(&dir);
    std::fs::write(theme.join(".env"), "CLOUDFLARE_API_TOKEN=x\n").unwrap();
    let skip = fake.requests().len();
    let r = run(&fake, &dir, &bundle, 1, Cmd::Push(Some(&theme)));
    assert_eq!(code(&r), Code::ThemeCheckFailed);
    assert!(r.result.unwrap_err().message.contains(".env"));
    assert_eq!(requests_since(&fake, skip), Vec::<String>::new());
}

#[test]
fn push_waits_for_a_held_lock_before_any_call() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    let theme = paper(&dir);
    std::fs::write(dir.0.join(deploy::LOCK_FILE), "1\n").unwrap();
    let skip = fake.requests().len();
    let r = run(&fake, &dir, &bundle, 1, Cmd::Push(Some(&theme)));
    assert_eq!(code(&r), Code::DeployLocked);
    assert_eq!(requests_since(&fake, skip), Vec::<String>::new());
    // Someone else's lock stays.
    assert!(dir.0.join(deploy::LOCK_FILE).exists());
}

#[test]
fn push_refuses_a_bundle_with_pending_migrations() {
    let mut older = migrations::bundled();
    older.pop();
    let (fake, dir, bundle) = set_up(&older);
    let theme = paper(&dir);
    let skip = fake.requests().len();
    let r = run(&fake, &dir, &bundle, 1, Cmd::Push(Some(&theme)));
    assert_eq!(code(&r), Code::ThemePushPending);
    let msg = r.result.unwrap_err().message;
    assert!(msg.contains("blog deploy"), "{msg}");
    let calls = requests_since(&fake, skip);
    assert!(
        !calls
            .iter()
            .any(|c| c.starts_with("POST") && !c.ends_with("/query")),
        "{calls:?}"
    );
    assert_eq!(active(&fake), P);
}

#[test]
fn deploy_after_a_push_carries_the_theme_from_the_local_copy() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    let theme = paper(&dir);
    ok(&run(&fake, &dir, &bundle, 1, Cmd::Push(Some(&theme))));
    let skip = fake.requests().len();
    let r = run(
        &fake,
        &dir,
        &bundle,
        2,
        Cmd::Deploy {
            theme_default: false,
        },
    );
    ok(&r);
    assert!(r.out.contains("from this folder's copy"), "{}", r.out);
    assert_eq!(active(&fake), V3);
    assert_eq!(theme_binding(&fake, V3).as_deref(), Some("paper"));
    assert_eq!(tag_binding(&fake, V3), Some(served_tag(&fake, V3)));
    assert_eq!(theme_assets(&fake, V3), theme_assets(&fake, V2));
    assert_eq!(served_doc(&fake, V3), served_doc(&fake, V2));
    // Nothing fetched from the site but the smoke check.
    assert!(
        !requests_since(&fake, skip)
            .iter()
            .any(|r| r.contains("/_theme/")),
        "{:?}",
        requests_since(&fake, skip)
    );
    let rec = load(&dir).deploys.last().unwrap().clone();
    assert_eq!(rec.kind, None);
    assert_eq!(rec.theme.unwrap().hash, served_doc(&fake, V2).hash);
}

#[test]
fn deploy_fetches_the_theme_from_the_site_when_the_copy_is_gone_or_damaged() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    let theme = paper(&dir);
    ok(&run(&fake, &dir, &bundle, 1, Cmd::Push(Some(&theme))));
    let hash = served_doc(&fake, V2).hash;
    // Gone.
    std::fs::remove_dir_all(dir.0.join(".blog")).unwrap();
    let skip = fake.requests().len();
    let r = run(
        &fake,
        &dir,
        &bundle,
        2,
        Cmd::Deploy {
            theme_default: false,
        },
    );
    ok(&r);
    assert!(
        r.out.contains("fetched from the site and checked"),
        "{}",
        r.out
    );
    let calls = requests_since(&fake, skip);
    assert!(calls.iter().any(|c| c.ends_with(DOC)), "{calls:?}");
    assert!(calls.iter().any(|c| c.ends_with(JS)), "{calls:?}");
    assert_eq!(theme_binding(&fake, V3).as_deref(), Some("paper"));
    assert_eq!(tag_binding(&fake, V3), Some(served_tag(&fake, V3)));
    assert_eq!(served_doc(&fake, V3).hash, hash);
    let copy = dir.0.join(STORE_DIR).join(&hash);
    assert!(copy.join("assets/minimal.js").is_file());
    // Damaged: one byte changed in the copy; the site's verified files win.
    std::fs::write(copy.join("assets/minimal.js"), "evil()").unwrap();
    let r = run(
        &fake,
        &dir,
        &bundle,
        3,
        Cmd::Deploy {
            theme_default: false,
        },
    );
    ok(&r);
    assert!(r.out.contains("cannot be used"), "{}", r.out);
    assert!(r.out.contains("fetched from the site"), "{}", r.out);
    assert_ne!(
        std::fs::read_to_string(copy.join("assets/minimal.js")).unwrap(),
        "evil()"
    );
    assert_eq!(served_doc(&fake, V4).hash, hash);
}

#[test]
fn a_file_changed_on_the_site_is_refused() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    let theme = paper(&dir);
    ok(&run(&fake, &dir, &bundle, 1, Cmd::Push(Some(&theme))));
    std::fs::remove_dir_all(dir.0.join(".blog")).unwrap();
    fake.state()
        .public_override
        .insert(JS.into(), b"evil()".to_vec());
    let skip = fake.requests().len();
    let r = run(
        &fake,
        &dir,
        &bundle,
        2,
        Cmd::Deploy {
            theme_default: false,
        },
    );
    assert_eq!(code(&r), Code::ThemeUnrecoverable);
    let msg = r.result.unwrap_err().message;
    assert!(
        msg.contains("minimal.js") && msg.contains("--theme-default"),
        "{msg}"
    );
    let calls = requests_since(&fake, skip);
    assert!(
        !calls.iter().any(|c| c.contains("assets-upload-session")),
        "{calls:?}"
    );
    assert_eq!(active(&fake), V2);
    assert_eq!(load(&dir).deploys.last().unwrap().outcome, "failed");
    // theme.json itself changed: refused too.
    fake.state().public_override.clear();
    let mut doc = served_doc(&fake, V2);
    doc.files.insert("style.css".into(), "body{}".into());
    fake.state()
        .public_override
        .insert(DOC.into(), serde_json::to_vec(&doc).unwrap());
    let r = run(
        &fake,
        &dir,
        &bundle,
        3,
        Cmd::Deploy {
            theme_default: false,
        },
    );
    assert_eq!(code(&r), Code::ThemeUnrecoverable);
}

#[test]
fn deploy_refuses_a_lost_theme_and_theme_default_drops_it() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    let theme = paper(&dir);
    ok(&run(&fake, &dir, &bundle, 1, Cmd::Push(Some(&theme))));
    std::fs::remove_dir_all(dir.0.join(".blog")).unwrap();
    fake.state()
        .version_assets
        .get_mut(V2)
        .unwrap()
        .retain(|p, _| !p.starts_with("/_theme/"));
    let r = run(
        &fake,
        &dir,
        &bundle,
        2,
        Cmd::Deploy {
            theme_default: false,
        },
    );
    assert_eq!(code(&r), Code::ThemeUnrecoverable);
    let msg = r.result.unwrap_err().message;
    assert!(
        msg.contains("HTTP 404") && msg.contains("blog theme push"),
        "{msg}"
    );
    assert_eq!(active(&fake), V2);
    let r = run(
        &fake,
        &dir,
        &bundle,
        3,
        Cmd::Deploy {
            theme_default: true,
        },
    );
    ok(&r);
    assert!(r.out.contains("is dropped (--theme-default)"), "{}", r.out);
    assert_eq!(active(&fake), V3);
    assert_eq!(theme_binding(&fake, V3), None);
    assert_eq!(tag_binding(&fake, V3), None);
    assert_eq!(theme_assets(&fake, V3), Vec::<String>::new());
    assert_eq!(load(&dir).deploys.last().unwrap().theme, None);
}

#[test]
fn default_push_removes_the_theme_and_later_deploys_keep_it_removed() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    let theme = paper(&dir);
    ok(&run(&fake, &dir, &bundle, 1, Cmd::Push(Some(&theme))));
    let r = run(&fake, &dir, &bundle, 2, Cmd::Push(None));
    ok(&r);
    assert!(
        r.out.contains("Switching to the default theme"),
        "{}",
        r.out
    );
    assert_eq!(active(&fake), V3);
    assert_eq!(theme_binding(&fake, V3), None);
    assert_eq!(tag_binding(&fake, V3), None);
    assert_eq!(theme_assets(&fake, V3), Vec::<String>::new());
    let rec = load(&dir).deploys.last().unwrap().clone();
    assert_eq!(
        (rec.kind.as_deref(), rec.theme),
        (Some("theme_default"), None)
    );
    let skip = fake.requests().len();
    ok(&run(
        &fake,
        &dir,
        &bundle,
        3,
        Cmd::Deploy {
            theme_default: false,
        },
    ));
    assert!(
        !requests_since(&fake, skip)
            .iter()
            .any(|r| r.contains("/_theme/"))
    );
    assert_eq!(theme_binding(&fake, V4), None);
    assert_eq!(tag_binding(&fake, V4), None);
}

#[test]
fn theme_rollback_goes_back_one_push_or_to_a_chosen_one_and_says_which_theme() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    ok(&run(
        &fake,
        &dir,
        &bundle,
        1,
        Cmd::Deploy {
            theme_default: false,
        },
    ));
    let theme = paper(&dir);
    ok(&run(&fake, &dir, &bundle, 2, Cmd::Push(Some(&theme))));
    let first = served_doc(&fake, V3).hash;
    let css = theme.join("style.css");
    let mut text = std::fs::read_to_string(&css).unwrap();
    text.push_str("\n/* v2 */\n");
    std::fs::write(&css, text).unwrap();
    ok(&run(&fake, &dir, &bundle, 3, Cmd::Push(Some(&theme))));
    let second = served_doc(&fake, V4).hash;
    assert_ne!(first, second);
    // Back one push: the first push's version.
    let r = run(&fake, &dir, &bundle, 4, Cmd::Rollback(None));
    ok(&r);
    assert_eq!(active(&fake), V3);
    assert!(
        r.out.contains("uses the theme paper") && r.out.contains(&first[..16]),
        "{}",
        r.out
    );
    // A chosen push.
    let r = run(&fake, &dir, &bundle, 5, Cmd::Rollback(Some(V4)));
    ok(&r);
    assert_eq!(active(&fake), V4);
    assert!(r.out.contains(&second[..16]), "{}", r.out);
    // Not a theme push: refused, the recent ones listed.
    let r = run(&fake, &dir, &bundle, 6, Cmd::Rollback(Some(V2)));
    assert_eq!(code(&r), Code::ThemeRollbackUnknown);
    let msg = r.result.unwrap_err().message;
    assert!(msg.contains(V4) && msg.contains(V3), "{msg}");
    assert_eq!(active(&fake), V4);
    // A deploy after the rollback carries what serves now.
    ok(&run(
        &fake,
        &dir,
        &bundle,
        7,
        Cmd::Deploy {
            theme_default: false,
        },
    ));
    assert_eq!(served_doc(&fake, V5).hash, second);
}

#[test]
fn theme_rollback_to_before_the_first_push_reports_the_default_theme() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    let r = run(&fake, &dir, &bundle, 1, Cmd::Rollback(None));
    assert_eq!(code(&r), Code::ThemeRollbackNoRecord);
    ok(&run(
        &fake,
        &dir,
        &bundle,
        2,
        Cmd::Deploy {
            theme_default: false,
        },
    ));
    let theme = paper(&dir);
    ok(&run(&fake, &dir, &bundle, 3, Cmd::Push(Some(&theme))));
    let r = run(&fake, &dir, &bundle, 4, Cmd::Rollback(None));
    ok(&r);
    assert_eq!(active(&fake), V2);
    assert!(r.out.contains("now uses the default theme"), "{}", r.out);
    let rb = load(&dir).rollbacks.last().unwrap().clone();
    assert_eq!((rb.to.as_str(), rb.previous.as_deref()), (V2, Some(V3)));
}

#[test]
fn a_push_that_fails_the_smoke_check_rolls_back() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    ok(&run(
        &fake,
        &dir,
        &bundle,
        1,
        Cmd::Deploy {
            theme_default: false,
        },
    ));
    fake.state().site.home_status = Some((V3.into(), 500));
    let theme = paper(&dir);
    let r = run(&fake, &dir, &bundle, 2, Cmd::Push(Some(&theme)));
    assert_eq!(code(&r), Code::SmokeFailed);
    assert_eq!(active(&fake), V2);
    let rec = load(&dir).deploys.last().unwrap().clone();
    assert_eq!(rec.outcome, "rolled_back");
    assert_eq!(rec.kind.as_deref(), Some("theme_push"));
    // Nothing to go back from: the failed push never served.
    let r = run(&fake, &dir, &bundle, 3, Cmd::Rollback(None));
    assert_eq!(code(&r), Code::ThemeRollbackNoRecord);
}

#[test]
fn a_push_the_site_does_not_serve_rolls_back() {
    // The new version answers, but renders the default theme (an older bundle, or theme.json
    // not reachable through ASSETS): the push must not count as done.
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    fake.state().site.theme_tag = Some((V2.into(), None));
    let theme = paper(&dir);
    let r = run(&fake, &dir, &bundle, 1, Cmd::Push(Some(&theme)));
    assert_eq!(code(&r), Code::SmokeFailed, "{}", r.out);
    let msg = r.result.as_ref().unwrap_err().message.clone();
    assert!(msg.contains("default theme"), "{msg}");
    assert_eq!(active(&fake), P);
    let rec = load(&dir).deploys.last().unwrap().clone();
    assert_eq!(rec.outcome, "rolled_back");
    assert_eq!(rec.smoke.as_deref(), Some("/ theme -"));
    // Another theme's tag fails the same way.
    fake.state().site.theme_tag = Some((V3.into(), Some("0123456789abcdef".into())));
    let r = run(&fake, &dir, &bundle, 2, Cmd::Push(Some(&theme)));
    assert_eq!(code(&r), Code::SmokeFailed, "{}", r.out);
    assert_eq!(active(&fake), P);
}

#[test]
fn a_matching_binding_sees_a_js_hit_and_passes_quietly() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    let theme = paper(&dir);
    let r = run(&fake, &dir, &bundle, 1, Cmd::Push(Some(&theme)));
    ok(&r);
    assert!(!r.out.contains("deploy.fast_path_unseen"), "{}", r.out);
    // The first `/` and one probe: the hit was seen at once.
    assert_eq!(fake.state().site.home_seen.len(), 2);
    // The default theme behaves the same: no binding, no tag, a hit.
    let r = run(&fake, &dir, &bundle, 2, Cmd::Push(None));
    ok(&r);
    assert!(!r.out.contains("deploy.fast_path_unseen"), "{}", r.out);
    assert_eq!(fake.state().site.home_seen.len(), 4);
}

#[test]
fn a_mismatched_binding_never_hits_and_only_warns() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    let theme = paper(&dir);
    fake.state().site.binding_tag = Some((V2.into(), Some("0123456789abcdef".into())));
    let r = run(&fake, &dir, &bundle, 1, Cmd::Push(Some(&theme)));
    ok(&r);
    assert!(r.out.contains("deploy.fast_path_unseen"), "{}", r.out);
    assert_eq!(active(&fake), V2, "a warning, not a rollback");
    assert_eq!(load(&dir).deploys.last().unwrap().outcome, "deployed");
    // The first `/` plus every probe, all within the smoke budget.
    assert_eq!(fake.state().site.home_seen.len(), 5);
}

#[test]
fn a_default_push_or_deploy_checks_the_theme_too() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    let theme = paper(&dir);
    ok(&run(&fake, &dir, &bundle, 1, Cmd::Push(Some(&theme))));
    let tag = served_doc(&fake, V2).hash[..16].to_string();
    // A deploy carrying the theme whose new version serves the default: rolled back.
    fake.state().site.theme_tag = Some((V3.into(), None));
    let r = run(
        &fake,
        &dir,
        &bundle,
        2,
        Cmd::Deploy {
            theme_default: false,
        },
    );
    assert_eq!(code(&r), Code::SmokeFailed, "{}", r.out);
    assert_eq!(active(&fake), V2);
    // `--default` whose new version still serves a theme: rolled back.
    fake.state().site.theme_tag = Some((V4.into(), Some(tag)));
    let r = run(&fake, &dir, &bundle, 3, Cmd::Push(None));
    assert_eq!(code(&r), Code::SmokeFailed, "{}", r.out);
    assert_eq!(active(&fake), V2);
    // Serving what was asked for passes.
    fake.state().site.theme_tag = None;
    ok(&run(&fake, &dir, &bundle, 4, Cmd::Push(None)));
    assert_eq!(theme_binding(&fake, &active(&fake)), None);
    assert_eq!(tag_binding(&fake, &active(&fake)), None);
}

#[test]
fn assets_whose_names_need_encoding_are_fetched_back() {
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    let theme = paper(&dir);
    ok(&run(&fake, &dir, &bundle, 1, Cmd::Push(Some(&theme))));
    // A theme pushed before names were checked: space, #, % and ? in asset names.
    let names = [
        "img/a b.png",
        "x#1.png",
        "100%.png",
        "q?.png",
        "caf\u{e9}.png",
    ];
    let mut doc = served_doc(&fake, V2);
    for n in names {
        let bytes = n.as_bytes().to_vec();
        doc.assets.insert(n.into(), store::sha256_hex(&bytes));
        fake.state()
            .public_override
            .insert(format!("/_theme/paper/assets/{n}"), bytes);
    }
    doc.hash = store::sha256_hex(&doc.hash_input());
    fake.state()
        .public_override
        .insert(DOC.into(), serde_json::to_vec(&doc).unwrap());
    let agent = ureq::Agent::new_with_defaults();
    let b = store::fetch(&agent, &format!("{}/site.example", fake.base), "paper", "1")
        .unwrap_or_else(|e| panic!("{e:?}"));
    for n in names {
        assert_eq!(b.assets[n], n.as_bytes(), "{n}");
    }
}

#[test]
fn theme_recovery_reasons_are_translated() {
    let vi = Text::new("vi");
    let (fake, dir, bundle) = set_up(&migrations::bundled());
    let theme = paper(&dir);
    ok(&run(&fake, &dir, &bundle, 1, Cmd::Push(Some(&theme))));
    std::fs::remove_dir_all(dir.0.join(".blog")).unwrap();
    // A changed file on the site.
    fake.state()
        .public_override
        .insert(JS.into(), b"evil()".to_vec());
    let deploy = Cmd::Deploy {
        theme_default: false,
    };
    let r = run_in(&fake, &dir, &bundle, 2, deploy, &vi);
    assert_eq!(code(&r), Code::ThemeUnrecoverable);
    let msg = r.result.unwrap_err().message;
    assert!(msg.contains("assets/minimal.js"), "{msg}");
    assert!(
        !msg.contains("does not match"),
        "English reason in a Vietnamese message: {msg}"
    );
    // Another theme served than the record names.
    fake.state().public_override.clear();
    let mut doc = served_doc(&fake, V2);
    doc.files.insert("style.css".into(), "body{}".into());
    doc.hash = store::sha256_hex(&doc.hash_input());
    fake.state()
        .public_override
        .insert(DOC.into(), serde_json::to_vec(&doc).unwrap());
    let r = run_in(&fake, &dir, &bundle, 3, deploy, &vi);
    assert_eq!(code(&r), Code::ThemeUnrecoverable);
    let msg = r.result.unwrap_err().message;
    assert!(msg.contains(store::short(&doc.hash)), "{msg}");
    assert!(
        !msg.contains("the deploy record says"),
        "English reason in a Vietnamese message: {msg}"
    );
}

#[test]
fn a_theme_is_fetched_back_from_workers_dev_uncached() {
    // A custom domain may sit behind a "Cache Everything" rule serving an old theme.json
    // that matches its own hash: fetch from workers.dev, past any cache.
    let (fake, dir, bundle) = set_up_domain(true);
    let theme = paper(&dir);
    ok(&run(&fake, &dir, &bundle, 1, Cmd::Push(Some(&theme))));
    std::fs::remove_dir_all(dir.0.join(".blog")).unwrap();
    let skip = fake.requests().len();
    let deploy = Cmd::Deploy {
        theme_default: false,
    };
    ok(&run(&fake, &dir, &bundle, 2, deploy));
    let theme_calls: Vec<_> = fake.requests()[skip..]
        .iter()
        .filter(|r| r.method == "GET" && r.path.contains("/_theme/"))
        .cloned()
        .collect();
    assert!(
        theme_calls.iter().any(|r| r.path.ends_with(DOC)),
        "{theme_calls:?}"
    );
    assert!(
        theme_calls.iter().any(|r| r.path.ends_with(JS)),
        "{theme_calls:?}"
    );
    for r in &theme_calls {
        assert!(
            r.path.starts_with("/plumblog.steven.workers.dev/"),
            "not from workers.dev: {}",
            r.path
        );
        assert_eq!(r.header("cache-control"), Some("no-cache"), "{}", r.path);
        assert!(
            r.query.starts_with("fresh="),
            "no cache-busting query: {:?}",
            r.query
        );
    }
}
