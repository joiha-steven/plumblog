//! Theme options and declarative plugins: schema checks at load, stored values that do not
//! fit, and the fixture plugin (tests/fixtures/plugins/hello) filling the minimal theme.

use plumblog_render::{Body, PageInfo, Plugin, Site, Theme};
use std::path::Path;

fn dir_files(root: &Path) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if let Ok(text) = std::fs::read_to_string(&p) {
                let rel = p
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, text));
            }
        }
    }
    out
}

fn minimal() -> Vec<(String, String)> {
    dir_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../themes/minimal"))
}

fn hello() -> Vec<(String, String)> {
    dir_files(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/plugins/hello"))
}

fn refs(files: &[(String, String)]) -> Vec<(&str, &str)> {
    files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect()
}

/// `files` with `path` replaced by `f(old text)`.
fn edit(
    files: &[(String, String)],
    path: &str,
    f: impl Fn(&str) -> String,
) -> Vec<(String, String)> {
    files
        .iter()
        .map(|(p, t)| (p.clone(), if p == path { f(t) } else { t.clone() }))
        .collect()
}

fn plugin(files: &[(String, String)]) -> Result<Plugin, String> {
    Plugin::load("hello", &refs(files).as_slice(), "/plugin/hello").map_err(|e| e.detail)
}

fn theme_with(files: &[(String, String)], plugins: Vec<Plugin>) -> Result<Theme, String> {
    Theme::load(&refs(files).as_slice(), None, plugins, "/theme/minimal").map_err(|e| e.detail)
}

fn site() -> Site {
    Site {
        title: "Plum Notes".into(),
        description: "A test blog.".into(),
        default_lang: "en".into(),
        home_url: "/".into(),
        search_url: None,
        feed_url: "/feed.xml".into(),
        rss_url: "/rss.xml".into(),
        json_feed_url: "/feed.json".into(),
        icon_url: "data:,".into(),
        image_url: None,
        author: None,
        footer: "Footer".into(),
        nav: vec![],
    }
}

fn home(theme: &Theme) -> plumblog_render::Report {
    theme
        .render_reporting(
            &site(),
            &PageInfo {
                lang: "en".into(),
                canonical: "https://example.com/".into(),
                origin: "https://example.com".into(),
            },
            &Body::Home {
                posts: &[],
                older_url: None,
                newer_url: None,
            },
        )
        .unwrap()
}

fn s(k: &str, v: &str) -> (String, String) {
    (k.to_owned(), v.to_owned())
}

#[test]
fn an_enabled_plugin_fills_its_slots_and_ships_its_files() {
    let theme = theme_with(&minimal(), vec![plugin(&hello()).unwrap()]).unwrap();
    let (on, problems) = theme.with_options(
        &[],
        &[(
            "hello".into(),
            vec![s("greeting", "\"Hi <b>\""), s("cover", "7")],
        )],
    );
    assert!(problems.is_empty(), "{problems:?}");
    let r = home(&on);
    assert!(r.problems.is_empty(), "{:?}", r.problems);
    // Autoescaped like theme text; the text comes from the plugin's own locale.
    assert!(
        r.html
            .contains("<p class=\"hello\">Hi &lt;b&gt; from a plugin #7</p>"),
        "footer"
    );
    assert!(
        r.html
            .contains("<meta name=\"hello\" content=\"Hi &lt;b&gt;\">"),
        "head"
    );
    assert!(
        r.html
            .contains("<script src=\"/plugin/hello/hello.js\" defer></script>")
    );
    assert!(
        on.css().contains(".hello{color:var(--c-accent)}"),
        "css inlined"
    );

    // Disabled: zero bytes of it anywhere.
    let off = home(&theme);
    assert!(!off.html.contains("hello"), "nothing of a disabled plugin");
    let bare = theme_with(&minimal(), Vec::new()).unwrap();
    assert_eq!(theme.css(), bare.css());
    assert_eq!(off.html, home(&bare).html);
}

#[test]
fn plugin_values_that_do_not_fit_fall_back_and_are_reported() {
    let theme = theme_with(&minimal(), vec![plugin(&hello()).unwrap()]).unwrap();
    let (on, problems) = theme.with_options(
        &[],
        &[
            (
                "hello".into(),
                vec![
                    s("greeting", &format!("\"{}\"", "x".repeat(41))),
                    s("gone", "1"),
                ],
            ),
            ("not-bundled".into(), Vec::new()),
        ],
    );
    let text = problems.join("\n");
    assert!(
        text.contains("option.invalid owner=plugin:hello key=greeting"),
        "{text}"
    );
    assert!(
        text.contains("option.unknown owner=plugin:hello key=gone"),
        "{text}"
    );
    assert!(text.contains("plugin.unknown name=not-bundled"), "{text}");
    assert!(
        home(&on)
            .html
            .contains("<p class=\"hello\">Hello from a plugin</p>"),
        "default"
    );
}

#[test]
fn a_slot_the_theme_lacks_shows_nothing_and_is_reported() {
    // A theme that does not offer the footer slot (and does not call it).
    let files = edit(&minimal(), "theme.toml", |t| {
        t.replace(
            "\"before_post\", \"after_post\", \"footer\"]",
            "\"before_post\", \"after_post\"]",
        )
    });
    let files = edit(&files, "templates/base.html", |t| {
        t.replace("{{ slot(\"footer\") }}", "")
    });
    let theme = theme_with(&files, vec![plugin(&hello()).unwrap()]).unwrap();
    let (on, problems) = theme.with_options(&[], &[("hello".into(), Vec::new())]);
    assert_eq!(
        problems,
        ["plugin.slot_missing name=hello slot=footer: theme minimal has no such slot"]
    );
    let html = home(&on).html;
    assert!(!html.contains("class=\"hello\""));
    assert!(
        html.contains("<meta name=\"hello\""),
        "its other slot still works"
    );
}

#[test]
fn a_partial_that_fails_while_rendering_is_left_out() {
    let files = edit(&hello(), "partials/footer.html", |_| {
        "{{ plugin.options.greeting | no_such_filter }}".into()
    });
    let theme = theme_with(&minimal(), vec![plugin(&files).unwrap()]).unwrap();
    let (on, _) = theme.with_options(&[], &[("hello".into(), Vec::new())]);
    let r = home(&on);
    assert!(
        r.html.contains("<meta name=\"hello\""),
        "page and other partials render"
    );
    assert_eq!(r.problems.len(), 1, "{:?}", r.problems);
    assert!(
        r.problems[0].starts_with("plugin.broken name=hello slot=footer"),
        "{:?}",
        r.problems
    );
}

/// A broken copy of the fixture and the message its load must give.
fn bad_plugin_cases() -> Vec<(Vec<(String, String)>, &'static str)> {
    vec![
        (
            edit(&hello(), "plugin.toml", |t| {
                t.replace("name = \"hello\"", "name = \"other\"")
            }),
            "must match the folder",
        ),
        (
            edit(&hello(), "plugin.toml", |t| {
                t.replace("\"head\", \"footer\"", "\"head\", \"sidebar\"")
            }),
            "unknown slot \"sidebar\"",
        ),
        (
            edit(&hello(), "plugin.toml", |t| {
                t.replace("assets/hello.css", "../../themes/minimal/style.css")
            }),
            "must be a path inside",
        ),
        (
            edit(&hello(), "plugin.toml", |t| {
                t.replace("assets/hello.js", "partials/head.html")
            }),
            "must be in assets/",
        ),
        (
            edit(&hello(), "plugin.toml", |t| {
                t.replace("version = ", "server = \"x\"\nversion = ")
            }),
            "unknown field `server`",
        ),
        (
            edit(&hello(), "plugin.toml", |t| {
                t.replace("type = \"short_text\"", "type = \"slider\"")
            }),
            "unknown variant `slider`",
        ),
        (
            edit(&hello(), "plugin.toml", |t| {
                t.replace("label = \"options.greeting\"", "label = \"options.nope\"")
            }),
            "\"options.nope\" is not in locales/en.toml",
        ),
        (
            edit(&hello(), "plugin.toml", |t| {
                t.replace("default = \"Hello\"", "default = 3")
            }),
            "options.greeting.default: expected text",
        ),
        (
            edit(&hello(), "plugin.toml", |t| {
                t.replace("max_len = 40", "max_len = 40\nmin = 1")
            }),
            "min does not apply to short_text",
        ),
        (
            edit(&hello(), "plugin.toml", |t| {
                t.replace(
                    "group = \"main\"\nlabel = \"options.cover\"",
                    "group = \"side\"\nlabel = \"options.cover\"",
                )
            }),
            "has no [groups.side]",
        ),
        (
            edit(&hello(), "partials/footer.html", |_| {
                "{% extends \"base.html\" %}".into()
            }),
            "{% extends %} is not allowed",
        ),
        (
            edit(&hello(), "partials/footer.html", |_| "{% if %}".into()),
            "partials/footer.html:",
        ),
        (
            hello()
                .into_iter()
                .filter(|(p, _)| p != "partials/head.html")
                .collect(),
            "partials/head.html is missing",
        ),
        (
            hello()
                .into_iter()
                .chain([("partials/sidebar.html".to_owned(), String::new())])
                .collect(),
            "partials/sidebar.html: no slot of that name",
        ),
        (
            hello()
                .into_iter()
                .chain([("worker.js".to_owned(), String::new())])
                .collect(),
            "worker.js: not part of a plugin",
        ),
    ]
}

#[test]
fn bad_plugins_do_not_load_and_say_why() {
    let cases = bad_plugin_cases();
    for (files, want) in cases {
        let err = plugin(&files).err().unwrap_or_default();
        assert!(err.contains(want), "want {want:?} in {err:?}");
        assert!(err.starts_with("plugins/hello/"), "names the folder: {err}");
    }
    let err = Plugin::load("Hello", &refs(&hello()).as_slice(), "/p")
        .err()
        .unwrap();
    assert!(err.detail.contains("lowercase letters"), "{}", err.detail);
}

#[test]
fn bad_theme_option_declarations_fail_at_load() {
    let cases = [
        (
            "type = \"toggle\"",
            "type = \"switch\"",
            "unknown variant `switch`",
        ),
        (
            "default = 55",
            "default = 57",
            "options.excerpt_words.default: 57 is not on the step of 5",
        ),
        ("default = 55", "default = 500", "500 is outside 0..120"),
        (
            "min = 0\nmax = 120",
            "min = 200\nmax = 120",
            "min is above max",
        ),
        (
            "token = \"accent\"",
            "token = \"nope\"",
            "unknown color token \"nope\"",
        ),
        (
            "default = \"writer\"",
            "default = \"book\"",
            "\"book\" is not an offered choice",
        ),
        (
            "label = \"options.sidenotes\"",
            "label = \"options.missing\"",
            "\"options.missing\" is not in locales/en.toml",
        ),
        (
            "group = \"reading\"\nlabel = \"options.sidenotes\"",
            "group = \"later\"\nlabel = \"options.sidenotes\"",
            "has no [groups.later]",
        ),
        (
            "[options.sidenotes]",
            "[options.Side-notes]",
            "option keys are lowercase",
        ),
        (
            "label = \"options.list_images\"",
            "label = \"options.list_images\"\nmax = 3",
            "max does not apply to toggle",
        ),
        (
            "icon = \"palette\"",
            "icon = \"Palette Icon\"",
            "icon names are lowercase",
        ),
        (
            "{ value = \"book\"",
            "{ value = \"writer\"",
            "choice \"writer\" twice",
        ),
        (
            "\"after_post\", \"footer\"]",
            "\"after_post\", \"sidebar\"]",
            "unknown slot \"sidebar\"",
        ),
    ];
    for (from, to, want) in cases {
        let files = edit(&minimal(), "theme.toml", |t| {
            assert!(t.contains(from), "fixture text {from:?} moved");
            t.replacen(from, to, 1)
        });
        let err = theme_with(&files, Vec::new()).err().unwrap_or_default();
        assert!(err.contains(want), "{from:?} -> want {want:?} in {err:?}");
    }
}

#[test]
fn stored_theme_values_that_do_not_fit_fall_back_and_are_reported() {
    let theme = theme_with(&minimal(), Vec::new()).unwrap();
    let (on, problems) = theme.with_options(
        &[
            s(
                "accent",
                "{\"light\":\"#fff;}body{display:none\",\"dark\":\"#000\"}",
            ),
            s("excerpt_words", "\"many\""),
            s("style", "\"book\""),
            s("sidenotes", "not json"),
            s("removed_in_an_update", "true"),
        ],
        &[],
    );
    assert_eq!(
        on.css(),
        theme.css(),
        "every value fell back to its default"
    );
    let text = problems.join("\n");
    for want in [
        "option.invalid owner=theme:minimal key=accent reason=light \"#fff;}body{display:none\" is not a #hex color",
        "option.invalid owner=theme:minimal key=excerpt_words reason=expected a number",
        "option.invalid owner=theme:minimal key=style reason=\"book\" is not an offered choice",
        "option.invalid owner=theme:minimal key=sidenotes reason=not JSON",
        "option.unknown owner=theme:minimal key=removed_in_an_update",
    ] {
        assert!(text.contains(want), "want {want:?} in\n{text}");
    }
    assert!(
        home(&on).html.contains("<html lang=\"en\""),
        "the page still renders"
    );
}

#[test]
fn stored_theme_values_reach_templates_and_css() {
    let theme = theme_with(&minimal(), Vec::new()).unwrap();
    let (on, problems) = theme.with_options(
        &[
            s("accent", "{\"light\":\"#123456\",\"dark\":\"#abcdef\"}"),
            s("sidenotes", "false"),
        ],
        &[],
    );
    assert!(problems.is_empty(), "{problems:?}");
    assert!(on.css().contains("--c-accent:light-dark(#123456,#abcdef)"));
    assert!(
        !theme.css().contains("#123456"),
        "the loaded theme is untouched"
    );
}

/// Same raw cap as `inlined_css_fits_the_budget` in minimal.rs: a bundled plugin's share is
/// what the default theme leaves under it. An owner may still enable several plugins and go
/// over (pages are served; the admin will warn), but no single bundled one may.
const CSS_RAW_CAP: usize = 40_000;

#[test]
fn every_bundled_plugin_loads_and_fits_the_css_budget_alone() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins");
    let mut checked = Vec::new();
    for e in std::fs::read_dir(&root).unwrap() {
        let dir = e.unwrap().path();
        if !dir.is_dir() {
            continue;
        }
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let files = dir_files(&dir);
        let p = Plugin::load(&name, &refs(&files).as_slice(), &format!("/plugin/{name}"))
            .unwrap_or_else(|e| panic!("{}", e.detail));
        let theme = theme_with(&minimal(), vec![p]).unwrap();
        let (on, problems) = theme.with_options(&[], &[(name.clone(), Vec::new())]);
        assert!(problems.is_empty(), "{name}: {problems:?}");
        let len = on.css().len();
        assert!(
            len < CSS_RAW_CAP,
            "plugins/{name}: minimal + this plugin inline {len} bytes of CSS (cap {CSS_RAW_CAP})"
        );
        checked.push(name);
    }
    println!("bundled plugins checked: {checked:?}");
}
