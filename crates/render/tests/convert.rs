//! TOML to the JSON the Worker loads: one rule for names, and errors an author can act on.

use plumblog_render::convert::{file, json_name};

#[test]
fn every_toml_file_keeps_its_name_plus_json() {
    assert_eq!(json_name("theme.toml").as_deref(), Some("theme.toml.json"));
    assert_eq!(
        json_name("locales/en.toml").as_deref(),
        Some("locales/en.toml.json")
    );
    assert_eq!(json_name("style.css"), None);
    assert_eq!(json_name("assets/x.json"), None);
}

#[test]
fn a_syntax_error_names_the_file_and_the_line() {
    for path in ["theme.toml", "plugin.toml", "locales/en.toml"] {
        let e = file(path, "name = \"x\"\nbroken = =\n").unwrap_err();
        assert!(e.starts_with(&format!("{path}: ")), "{e}");
        assert!(e.contains("line 2"), "{e}");
    }
}

#[test]
fn a_manifest_with_the_wrong_shape_is_refused_with_the_toml_position() {
    let e = file("plugin.toml", "name = \"p\"\nversion = \"1\"\ncolour = 1\n").unwrap_err();
    assert!(e.starts_with("plugin.toml: "), "{e}");
    assert!(e.contains("colour") && e.contains("line 3"), "{e}");
    let e = file("theme.toml", "name = 1\n").unwrap_err();
    assert!(e.starts_with("theme.toml: ") && e.contains("line 1"), "{e}");
}

#[test]
fn valid_files_become_json_and_datetimes_text() {
    assert_eq!(
        file("locales/en.toml", "[a]\nb = \"c\"\n").unwrap(),
        "{\"a\":{\"b\":\"c\"}}"
    );
    assert_eq!(
        file("x.toml", "d = 2026-01-02\n").unwrap(),
        "{\"d\":\"2026-01-02\"}"
    );
}

#[test]
fn an_option_with_nan_is_refused_naming_file_and_key() {
    let src = "name = \"t\"\n[colors.light]\n[colors.dark]\n[options.n]\ntype = \"number\"\nlabel = \"x\"\ndefault = 1\nstep = nan\n";
    let e = file("theme.toml", src).unwrap_err();
    assert!(
        e.starts_with("theme.toml: ") && e.contains("options.n.step"),
        "{e}"
    );
}
