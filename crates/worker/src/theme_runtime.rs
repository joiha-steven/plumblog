//! Which theme renders: a theme pushed as static assets (`PLUMBLOG_THEME` names it, its text
//! files are one `/_theme/<name>/theme.json`), else the embedded default. Both load once per
//! isolate. A pushed theme that is missing or broken never breaks the site: the default
//! renders and the log says why, once per isolate.
//!
//! A binding value is fixed for a Worker version, and a new version starts new isolates, so
//! the first request's choice holds for the isolate's life.

use crate::pages::source;
use crate::pages::{PLUGIN_ASSETS_URL, THEME_ASSETS_URL};
use plumblog_render::pushed::MAX_DOCUMENT;
use plumblog_render::{Plugin, PushedTheme, Theme, ThemeError};
use std::sync::OnceLock;
use std::sync::atomic::AtomicU8;
use worker::Env;

/// Plain-text binding: the active pushed theme's name. Absent or empty: the embedded default.
pub const THEME_VAR: &str = "PLUMBLOG_THEME";
/// Static assets binding (`blog deploy` binds it; tools/wrangler.dev.toml too).
const ASSETS_BINDING: &str = "ASSETS";

/// A pushed theme that loaded, with its page cache tag.
struct Pushed {
    theme: Theme,
    tag: String,
}

/// Set once per isolate by [`prepare`]: `None` means the embedded default.
static PUSHED: OnceLock<Option<Pushed>> = OnceLock::new();

fn plugins() -> Vec<Plugin> {
    source::PLUGIN_FILES
        .iter()
        .filter_map(|(name, files)| {
            let url = format!("{PLUGIN_ASSETS_URL}/{name}");
            Plugin::load(name, files, &url)
                .map_err(|e| log(&format!("plugin.broken name={name} detail={e}")))
                .ok()
        })
        .collect()
}

/// The embedded default theme with the bundled plugins that loaded. Parsing manifests,
/// locales and templates costs more than rendering a page, so it happens once per isolate.
pub fn embedded() -> Result<&'static Theme, &'static ThemeError> {
    static THEME: OnceLock<Result<Theme, ThemeError>> = OnceLock::new();
    THEME
        .get_or_init(|| Theme::load(&source::THEME_FILES, None, plugins(), THEME_ASSETS_URL))
        .as_ref()
}

/// The theme pages render with: the pushed one when [`prepare`] loaded it, else the default.
pub fn current() -> Result<&'static Theme, &'static ThemeError> {
    match PUSHED.get() {
        Some(Some(p)) => Ok(&p.theme),
        _ => embedded(),
    }
}

/// Part of the page cache key and `ETag`: empty for the embedded default (so keys stay what
/// they were), `-t<hash prefix>` for a pushed theme. A push or a rollback changes it, so a
/// page rendered with the other theme is never served.
pub fn cache_tag() -> &'static str {
    match PUSHED.get() {
        Some(Some(p)) => &p.tag,
        _ => "",
    }
}

/// The pushed theme, when [`prepare`] loaded one.
pub fn pushed() -> Option<&'static Theme> {
    match PUSHED.get() {
        Some(Some(p)) => Some(&p.theme),
        _ => None,
    }
}

/// The theme name `PLUMBLOG_THEME` binds, trimmed; empty for the embedded default.
pub fn bound(env: &Env) -> String {
    env.var(THEME_VAR)
        .map(|v| v.to_string().trim().to_owned())
        .unwrap_or_default()
}

/// The binding names `name` as the pushed theme, but it did not load (missing, invalid, too
/// large, or not fetched yet): its option declarations are unknown. Call after [`prepare`].
pub fn unavailable(env: &Env, name: &str) -> bool {
    bound(env) == name && pushed().is_none_or(|t| t.name() != name)
}

/// `x-plumblog-theme` for a page the pushed theme rendered (`None` for the default).
pub fn served_tag() -> Option<&'static str> {
    cache_tag().strip_prefix("-t")
}

/// What a pushed-theme log line is about. Each kind is logged once per isolate, so a page
/// view never writes a log line per request, yet one kind never hides another.
#[derive(Clone, Copy)]
pub(crate) enum LogKind {
    Missing,
    Invalid,
    TooLarge,
    FetchFailed,
    RenderFailed,
}

/// Logs `line` unless a line of `kind` was logged in this isolate; true when it logged.
pub(crate) fn log_once(kind: LogKind, line: &str) -> bool {
    static LOGGED: AtomicU8 = AtomicU8::new(0);
    log_once_in(&LOGGED, kind, line)
}

fn log_once_in(logged: &AtomicU8, kind: LogKind, line: &str) -> bool {
    let bit = 1u8 << (kind as u8);
    if logged.fetch_or(bit, std::sync::atomic::Ordering::Relaxed) & bit != 0 {
        return false;
    }
    log(line);
    true
}

/// The theme to render with after the active theme's templates failed on `path` (an error
/// that only real data shows; storage errors never come here): the embedded default when a
/// pushed theme is active, so readers get a page instead of a 500. The caller must not cache
/// the page. `None` when no pushed theme is active: nothing to fall back to.
pub fn fallback(path: &str, detail: &str) -> Option<&'static Theme> {
    let Some(Some(p)) = PUSHED.get() else {
        return None;
    };
    fallback_from(p.theme.name(), path, detail)
}

pub(crate) fn fallback_from(name: &str, path: &str, detail: &str) -> Option<&'static Theme> {
    log_once(
        LogKind::RenderFailed,
        &format!(
            "theme.pushed_render_failed name={name} path={path} detail={detail}; \
             served with the default theme, not cached"
        ),
    );
    embedded().ok()
}

/// What fetching `theme.json` gave.
enum Fetched {
    Body(String),
    /// 404: the binding names a theme that was never pushed (or was removed).
    Missing,
    /// Over [`MAX_DOCUMENT`] (declared or read): never parsed.
    TooLarge(usize),
    /// Anything else: worth trying again on a later request.
    Failed(String),
}

/// What to do with a fetch result: `Ok(Some)` use it, `Ok(None)` use the default for the
/// isolate's life (logged here), `Err` use the default for this request only.
fn decide(name: &str, fetched: Fetched) -> Result<Option<Pushed>, String> {
    let json = match fetched {
        Fetched::Body(b) => b,
        Fetched::Missing => {
            log_once(
                LogKind::Missing,
                &format!(
                    "theme.pushed_missing name={name} detail=/_theme/{name}/theme.json not \
                     found; using the default theme"
                ),
            );
            return Ok(None);
        }
        Fetched::TooLarge(n) => {
            log_once(
                LogKind::TooLarge,
                &format!(
                    "theme.pushed_invalid name={name} detail=theme.json is {n} bytes, over \
                     {MAX_DOCUMENT}; using the default theme"
                ),
            );
            return Ok(None);
        }
        Fetched::Failed(e) => return Err(e),
    };
    match load(name, &json) {
        Ok(p) => Ok(Some(p)),
        Err(e) => {
            log_once(
                LogKind::Invalid,
                &format!("theme.pushed_invalid name={name} detail={e}; using the default theme"),
            );
            Ok(None)
        }
    }
}

/// Parses and loads a pushed theme.
fn load(name: &str, json: &str) -> Result<Pushed, String> {
    load_parsed(name, &PushedTheme::parse(json, name)?)
}

/// Loads a parsed push; templates it lacks come from the default theme.
fn load_parsed(name: &str, pushed: &PushedTheme) -> Result<Pushed, String> {
    let default: &[(&str, &str)] = source::THEME_FILES;
    let theme = Theme::load(
        pushed,
        Some(&default),
        plugins(),
        &format!("/_theme/{name}/assets"),
    )
    .map_err(|e| e.detail)?;
    if theme.name() != name {
        return Err(format!(
            "theme name {:?} is not the active theme {name:?}",
            theme.name()
        ));
    }
    Ok(Pushed {
        theme,
        tag: format!("-t{}", &pushed.hash[..16]),
    })
}

/// Theme names are URL path segments: the same shape as option owner names.
fn plain_name(n: &str) -> bool {
    n.len() <= 64
        && n.starts_with(|c: char| c.is_ascii_lowercase())
        && n.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Chooses the theme for this isolate, fetching `theme.json` once (a single request, no
/// per-file fetches). Call before [`current`] or [`cache_tag`] on any request that renders
/// or checks theme options; after the first success it costs nothing.
pub async fn prepare(env: &Env) {
    if PUSHED.get().is_some() {
        return;
    }
    let name = env
        .var(THEME_VAR)
        .map(|v| v.to_string())
        .unwrap_or_default();
    let name = name.trim();
    if name.is_empty() {
        let _ = PUSHED.set(None);
        return;
    }
    if !plain_name(name) {
        log_once(
            LogKind::Invalid,
            &format!(
                "theme.pushed_invalid name={name:?} detail={THEME_VAR} is not a theme name; \
                 using the default theme"
            ),
        );
        let _ = PUSHED.set(None);
        return;
    }
    let started = worker::Date::now().as_millis();
    let fetched = fetch(env, name).await;
    let fetched_at = worker::Date::now().as_millis();
    match decide(name, fetched) {
        Ok(choice) => {
            if let Some(p) = &choice {
                log(&format!(
                    "theme.pushed_loaded name={name} tag={} fetch_ms={} load_ms={}",
                    p.tag,
                    fetched_at.saturating_sub(started),
                    worker::Date::now().as_millis().saturating_sub(fetched_at)
                ));
            }
            // Two requests may race through the await; the first choice stands.
            let _ = PUSHED.set(choice);
        }
        Err(e) => {
            log_once(
                LogKind::FetchFailed,
                &format!(
                    "theme.pushed_fetch_failed name={name} detail={e}; default theme for this \
                     request"
                ),
            );
        }
    }
}

async fn fetch(env: &Env, name: &str) -> Fetched {
    let assets = match env.assets(ASSETS_BINDING) {
        Ok(a) => a,
        Err(e) => return Fetched::Failed(format!("{ASSETS_BINDING} binding: {e}")),
    };
    // The host is ignored by the assets binding; only the path picks the file.
    let url = format!("https://assets.invalid/_theme/{name}/theme.json");
    let mut resp = match assets.fetch(url, None).await {
        Ok(r) => r,
        Err(e) => return Fetched::Failed(format!("fetch: {e}")),
    };
    match resp.status_code() {
        200 => read_capped(&mut resp).await,
        404 => Fetched::Missing,
        s => Fetched::Failed(format!("HTTP {s}")),
    }
}

/// The body, refused unread when `Content-Length` is over [`MAX_DOCUMENT`] and refused as
/// soon as the bytes read pass it (a missing or wrong length cannot get a bigger document
/// parsed): a document too big to load within the request's CPU and memory limits would
/// otherwise end the isolate before it ever falls back.
async fn read_capped(resp: &mut worker::Response) -> Fetched {
    use futures_core::Stream;
    let declared = resp
        .headers()
        .get("content-length")
        .ok()
        .flatten()
        .and_then(|v| v.trim().parse::<usize>().ok());
    if let Some(n) = declared.filter(|n| *n > MAX_DOCUMENT) {
        return Fetched::TooLarge(n);
    }
    let stream = match resp.stream() {
        Ok(s) => s,
        Err(e) => return Fetched::Failed(format!("read: {e}")),
    };
    let mut stream = std::pin::pin!(stream);
    let mut buf = Vec::new();
    while let Some(chunk) = std::future::poll_fn(|cx| stream.as_mut().poll_next(cx)).await {
        match chunk {
            Ok(c) => buf.extend_from_slice(&c),
            Err(e) => return Fetched::Failed(format!("read: {e}")),
        }
        if buf.len() > MAX_DOCUMENT {
            return Fetched::TooLarge(buf.len());
        }
    }
    String::from_utf8(buf).map_or_else(
        |_| Fetched::Failed("read: theme.json is not UTF-8".to_owned()),
        Fetched::Body,
    )
}

/// A Worker log warning, as `plugin.broken` always was (`console_warn!` only exists in wasm;
/// host tests print to stderr).
fn log(line: &str) {
    #[cfg(target_arch = "wasm32")]
    worker::console_warn!("{line}");
    #[cfg(not(target_arch = "wasm32"))]
    eprintln!("{line}");
}

#[cfg(test)]
mod tests {
    use super::{
        AtomicU8, Fetched, LogKind, decide, embedded, fallback, fallback_from, load, load_parsed,
        log_once_in, plain_name,
    };
    use crate::pages::source;
    use plumblog_render::{PushedTheme, THEME_API};

    const HASH: &str = "a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90";

    /// The embedded default, renamed `paper`, with `edit` applied to its files.
    fn pushed(edit: impl Fn(&mut PushedTheme)) -> PushedTheme {
        let mut t = PushedTheme {
            api: THEME_API,
            name: "paper".into(),
            version: "0.2.0".into(),
            hash: HASH.into(),
            files: source::THEME_FILES
                .iter()
                .map(|(p, t)| ((*p).to_owned(), (*t).to_owned()))
                .collect(),
            assets: std::collections::BTreeMap::new(),
        };
        let manifest =
            t.files["theme.toml.json"].replacen("\"name\":\"minimal\"", "\"name\":\"paper\"", 1);
        t.files.insert("theme.toml.json".into(), manifest);
        edit(&mut t);
        t
    }

    /// The default renamed `paper`, grown to about `total` bytes of `theme.json`: a third
    /// each in CSS rules, template markup and locale keys, the three costs of a load.
    pub(crate) fn padded(total: usize) -> String {
        use std::fmt::Write as _;
        let mut t = pushed(|_| {});
        let base = t.to_json().len();
        let third = total.saturating_sub(base) / 3;
        let mut css = t.files["style.css"].clone();
        let mut html = t.files["templates/tag.html"].clone();
        let mut locale: serde_json::Value =
            serde_json::from_str(&t.files["locales/en.toml.json"]).unwrap();
        let mut pad = serde_json::Map::new();
        let (c0, h0) = (css.len(), html.len());
        let mut pad_len = 0;
        let mut n = 0;
        while css.len() - c0 < third {
            let _ = writeln!(
                css,
                ".pad-{n} > a:hover {{ margin: 0 {n}px; color: var(--c-text); }}"
            );
            n += 1;
        }
        while html.len() - h0 < third {
            let _ = writeln!(
                html,
                "{{% if page.title %}}<p class=\"pad-{n}\">{{{{ page.title }}}}</p>{{% endif %}}"
            );
            n += 1;
        }
        while pad_len < third {
            let text = format!("Text number {n} for padding");
            pad_len += text.len() + 8;
            pad.insert(format!("k{n}"), text.into());
            n += 1;
        }
        locale["pad"] = pad.into();
        t.files.insert("style.css".into(), css);
        t.files.insert("templates/tag.html".into(), html);
        t.files
            .insert("locales/en.toml.json".into(), locale.to_string());
        t.to_json()
    }

    /// Cold load cost by `theme.json` size, natively (`cargo test --release -p
    /// plumblog-worker -- --ignored --nocapture theme_load_cost`); wasm is 1.5-3x slower.
    #[test]
    #[ignore = "timing table, run by hand"]
    fn theme_load_cost() {
        for kb in [77, 256, 512, 1024, 2048] {
            let json = padded(kb * 1024);
            let mut best = f64::MAX;
            for _ in 0..5 {
                let t0 = std::time::Instant::now();
                load("paper", &json).unwrap();
                best = best.min(t0.elapsed().as_secs_f64() * 1000.0);
            }
            println!(
                "theme.json {:>5} KB: parse+load {best:.1} ms",
                json.len() / 1024
            );
        }
    }

    #[test]
    fn a_valid_push_loads_with_its_own_assets_and_cache_tag() {
        let p = load_parsed("paper", &pushed(|_| {})).unwrap();
        assert_eq!(p.theme.name(), "paper");
        assert_eq!(p.tag, "-ta1b2c3d4e5f60718");
        assert!(
            p.theme.css().contains("/_theme/paper/assets/fonts/"),
            "fonts from the push"
        );
    }

    #[test]
    fn missing_templates_come_from_the_default() {
        let t = pushed(|t| {
            t.files
                .retain(|p, _| !p.starts_with("templates/") || p == "templates/base.html");
        });
        assert!(load_parsed("paper", &t).is_ok());
    }

    #[test]
    fn every_failure_falls_back_to_the_default() {
        type Edit = fn(&mut PushedTheme);
        let edits: [(Edit, &str); 3] = [
            (
                |t| {
                    t.files.remove("theme.toml.json");
                },
                "theme.toml.json is missing",
            ),
            (
                |t| {
                    t.files
                        .insert("templates/home.html".into(), "{% if %}".into());
                },
                "home.html",
            ),
            (
                |t| {
                    let s = t.files["theme.toml.json"]
                        .replace("\"name\":\"paper\"", "\"name\":\"other\"");
                    t.files.insert("theme.toml.json".into(), s);
                },
                "not the active theme",
            ),
        ];
        for (edit, why) in edits {
            let e = load_parsed("paper", &pushed(edit)).err().expect(why);
            assert!(e.contains(why), "{why}: {e}");
        }
        // Another api either way: the first format (TOML text in `files`) is not read.
        let other_api = |api: u32| {
            format!(r#"{{"api":{api},"name":"paper","version":"1","hash":"{HASH}","files":{{}}}}"#)
        };
        let (newer, older) = (other_api(THEME_API + 1), other_api(THEME_API - 1));
        for (json, why) in [
            ("{not json", "theme.json"),
            (newer.as_str(), "api"),
            (older.as_str(), "api"),
        ] {
            let e = load("paper", json).err().expect(why);
            assert!(e.contains(why), "{why}: {e}");
            assert!(
                matches!(decide("paper", Fetched::Body(json.to_owned())), Ok(None)),
                "{why}"
            );
        }
        assert!(matches!(decide("paper", Fetched::Missing), Ok(None)));
        // A failed fetch is retried later: it decides nothing for the isolate.
        assert!(decide("paper", Fetched::Failed("x".into())).is_err());
        assert!(embedded().is_ok());
    }

    #[test]
    fn a_render_failure_falls_back_only_from_a_pushed_theme() {
        // No pushed theme in this isolate: a failing default has nothing behind it.
        assert!(fallback("/x", "boom").is_none());
        let t = fallback_from("paper", "/x", "boom").expect("the default");
        assert_eq!(t.name(), "minimal");
    }

    #[test]
    fn a_document_over_the_cap_is_never_parsed() {
        assert!(matches!(
            decide("paper", Fetched::TooLarge(super::MAX_DOCUMENT + 1)),
            Ok(None)
        ));
    }

    #[test]
    fn each_log_kind_is_logged_once_without_hiding_the_others() {
        let logged = AtomicU8::new(0);
        let once = |k| log_once_in(&logged, k, "theme.pushed_test");
        assert!(once(LogKind::FetchFailed));
        assert!(!once(LogKind::FetchFailed));
        // A later render failure still logs, once.
        assert!(once(LogKind::RenderFailed));
        assert!(!once(LogKind::RenderFailed));
        for k in [LogKind::Missing, LogKind::Invalid, LogKind::TooLarge] {
            assert!(once(k));
        }
    }

    #[test]
    fn theme_names_are_plain_path_segments() {
        for ok in ["paper", "a-b_1"] {
            assert!(plain_name(ok), "{ok}");
        }
        for bad in ["", "../x", "a/b", "Paper", "1a", &"a".repeat(65)] {
            assert!(!plain_name(bad), "{bad}");
        }
    }
}
