//! Which theme renders: a theme pushed as static assets (`PLUMBLOG_THEME` names it, its text
//! files are one `/_theme/<name>/theme.json`), else the embedded default. Both load once per
//! isolate. A pushed theme that is missing or broken never breaks the site: the default
//! renders and the log says why, once per isolate.
//!
//! A binding value is fixed for a Worker version, and a new version starts new isolates, so
//! the first request's choice holds for the isolate's life.

use crate::pages::source;
use crate::pages::{PLUGIN_ASSETS_URL, THEME_ASSETS_URL};
use plumblog_render::{Plugin, PushedTheme, Theme, ThemeError};
use std::sync::OnceLock;
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

/// What fetching `theme.json` gave.
enum Fetched {
    Body(String),
    /// 404: the binding names a theme that was never pushed (or was removed).
    Missing,
    /// Anything else: worth trying again on a later request.
    Failed(String),
}

/// What to do with a fetch result: `Ok(Some)` use it, `Ok(None)` use the default for the
/// isolate's life (logged here), `Err` use the default for this request only.
fn decide(name: &str, fetched: Fetched) -> Result<Option<Pushed>, String> {
    let json = match fetched {
        Fetched::Body(b) => b,
        Fetched::Missing => {
            log(&format!(
                "theme.pushed_missing name={name} detail=/_theme/{name}/theme.json not found; \
                 using the default theme"
            ));
            return Ok(None);
        }
        Fetched::Failed(e) => return Err(e),
    };
    match load(name, &json) {
        Ok(p) => Ok(Some(p)),
        Err(e) => {
            log(&format!(
                "theme.pushed_invalid name={name} detail={e}; using the default theme"
            ));
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
            "theme.toml name {:?} is not the active theme {name:?}",
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
        log(&format!(
            "theme.pushed_invalid name={name:?} detail={THEME_VAR} is not a theme name; using the default theme"
        ));
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
        Err(e) => log(&format!(
            "theme.pushed_fetch_failed name={name} detail={e}; default theme for this request"
        )),
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
        200 => match resp.text().await {
            Ok(t) => Fetched::Body(t),
            Err(e) => Fetched::Failed(format!("read: {e}")),
        },
        404 => Fetched::Missing,
        s => Fetched::Failed(format!("HTTP {s}")),
    }
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
    use super::{Fetched, decide, embedded, load, load_parsed, plain_name};
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
        };
        let toml = t.files["theme.toml"].replacen("name = \"minimal\"", "name = \"paper\"", 1);
        t.files.insert("theme.toml".into(), toml);
        edit(&mut t);
        t
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
                    t.files.remove("theme.toml");
                },
                "theme.toml is missing",
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
                    let s = t.files["theme.toml"].replace("name = \"paper\"", "name = \"other\"");
                    t.files.insert("theme.toml".into(), s);
                },
                "not the active theme",
            ),
        ];
        for (edit, why) in edits {
            let e = load_parsed("paper", &pushed(edit)).err().expect(why);
            assert!(e.contains(why), "{why}: {e}");
        }
        let api2 = format!(
            r#"{{"api":{},"name":"paper","version":"1","hash":"{HASH}","files":{{}}}}"#,
            THEME_API + 1
        );
        for (json, why) in [("{not json", "theme.json"), (api2.as_str(), "api")] {
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
    fn theme_names_are_plain_path_segments() {
        for ok in ["paper", "a-b_1"] {
            assert!(plain_name(ok), "{ok}");
        }
        for bad in ["", "../x", "a/b", "Paper", "1a", &"a".repeat(65)] {
            assert!(!plain_name(bad), "{bad}");
        }
    }
}
