//! Option declarations shared by themes (`theme.toml`) and plugins (`plugin.toml`): the
//! schema, its checks at load, and the checks every stored value passes before a template
//! sees it (docs/ARCHITECTURE.md, Plugins and options). Stored values are JSON
//! (`extension_options.value_json`); a value that fails its declaration falls back to the
//! default and is reported, never breaking a page.

use serde::Deserialize;
use serde_json::Value as Json;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;

/// The admin's fixed field types. Unknown types fail to parse, so a theme or plugin with one
/// fails to load instead of rendering half its options.
#[derive(Deserialize, Clone, Copy, PartialEq, Eq, Debug)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    ShortText,
    LongText,
    Number,
    Toggle,
    Choice,
    Color,
    Image,
    List,
}

/// Defaults for limits a declaration may leave out.
const SHORT_TEXT_MAX: usize = 200;
const LONG_TEXT_MAX: usize = 5000;
const LIST_MAX_ITEMS: usize = 20;

impl FieldType {
    pub const ALL: [Self; 8] = [
        Self::ShortText,
        Self::LongText,
        Self::Number,
        Self::Toggle,
        Self::Choice,
        Self::Color,
        Self::Image,
        Self::List,
    ];

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ShortText => "short_text",
            Self::LongText => "long_text",
            Self::Number => "number",
            Self::Toggle => "toggle",
            Self::Choice => "choice",
            Self::Color => "color",
            Self::Image => "image",
            Self::List => "list",
        }
    }

    /// The stored JSON value and what templates read, for the generated reference.
    #[must_use]
    pub fn value_doc(self) -> String {
        match self {
            Self::ShortText => format!(
                "a string without line breaks, at most `max_len` characters ({SHORT_TEXT_MAX})"
            ),
            Self::LongText => format!("a string, at most `max_len` characters ({LONG_TEXT_MAX})"),
            Self::Number => {
                "a number within `min`..`max`, on the `step` grid from `min`; whole unless `min`, `max` or `step` has a fraction".into()
            }
            Self::Toggle => "`true` or `false`".into(),
            Self::Choice => "the `value` of one choice that is not a draft".into(),
            Self::Color => "`{ light = \"#rrggbb\", dark = \"#rrggbb\" }` (3, 4, 6 or 8 hex digits); a theme color replaces its `token`".into(),
            Self::Image => "a media id (integer) or `\"\"` for none; templates read `.id` (`theme.options.<key>.id`), empty when none".into(),
            Self::List => format!(
                "an array of strings without line breaks, at most `max_items` ({LIST_MAX_ITEMS}), each at most `max_len` characters ({SHORT_TEXT_MAX})"
            ),
        }
    }

    /// Declaration keys this type accepts besides the common ones.
    #[must_use]
    pub const fn extra_keys(self) -> &'static [&'static str] {
        match self {
            Self::ShortText | Self::LongText => &["max_len"],
            Self::Number => &["min", "max", "step"],
            Self::Toggle | Self::Image => &[],
            Self::Choice => &["choices"],
            Self::Color => &["token"],
            Self::List => &["max_items", "max_len"],
        }
    }
}

/// Keys every declaration has, for the generated reference.
pub const COMMON_KEYS: &[(&str, &str)] = &[
    ("type", "one of the field types below (required)"),
    ("label", "locale key of the field name (required)"),
    ("help", "locale key of a one-line explanation"),
    (
        "group",
        "key of a `[groups.<key>]` table (`label`, `icon`) the field is shown in",
    ),
    ("icon", "name from the admin icon set"),
    (
        "default",
        "value used until the owner changes it; must pass the same checks as a stored value (required)",
    ),
];

/// `[options.<key>]`.
#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct OptionDecl {
    #[serde(rename = "type")]
    pub kind: FieldType,
    pub label: String,
    pub help: Option<String>,
    pub group: Option<String>,
    pub icon: Option<String>,
    pub default: Json,
    /// `color` in a theme: the token it overrides (the option key when absent).
    pub token: Option<String>,
    #[serde(default)]
    pub choices: Vec<Choice>,
    pub min: Option<f64>,
    pub max: Option<f64>,
    pub step: Option<f64>,
    pub max_len: Option<usize>,
    pub max_items: Option<usize>,
}

#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct Choice {
    pub value: String,
    pub label: String,
    /// Theme styles: one stylesheet shipped after `style.css` when this value is chosen.
    pub css: Option<String>,
    /// Not offered to owners; a stored draft value falls back to the default.
    #[serde(default)]
    pub draft: bool,
}

/// `[groups.<key>]`.
#[derive(Deserialize, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct Group {
    pub label: String,
    pub icon: Option<String>,
}

/// Declarations with their defaults as JSON, checked once at load.
#[derive(Debug, Default)]
pub(crate) struct Schema {
    pub decls: BTreeMap<String, OptionDecl>,
    defaults: BTreeMap<String, Json>,
}

/// Who declares the options, for messages and the rules that differ.
#[derive(Clone, Copy)]
pub(crate) enum Owner<'a> {
    /// The theme's color token names.
    Theme(&'a BTreeSet<String>),
    Plugin,
}

impl Schema {
    /// Checks every declaration: `file` names the manifest in errors, `strings` is the
    /// owner's English locale (labels and help must exist there).
    pub fn check(
        file: &str,
        decls: BTreeMap<String, OptionDecl>,
        groups: &BTreeMap<String, Group>,
        strings: &HashMap<String, String>,
        owner: Owner<'_>,
    ) -> Result<Self, String> {
        let key_exists = |what: &str, key: &str| {
            if strings.contains_key(key) {
                Ok(())
            } else {
                Err(format!("{file}: {what} {key:?} is not in locales/en.toml"))
            }
        };
        // Icons are names from the admin icon set; the admin checks the name exists, this
        // only that it is a name.
        let icon_ok = |what: &str, icon: Option<&String>| match icon {
            Some(i) if !is_icon(i) => Err(format!(
                "{file}: {what} {i:?}: icon names are lowercase letters, digits and -"
            )),
            _ => Ok(()),
        };
        for (key, g) in groups {
            key_exists(&format!("groups.{key}.label"), &g.label)?;
            icon_ok(&format!("groups.{key}.icon"), g.icon.as_ref())?;
        }
        let mut defaults = BTreeMap::new();
        for (key, d) in &decls {
            let at = format!("options.{key}");
            if !is_key(key) {
                return Err(format!(
                    "{file}: [{at}]: option keys are lowercase letters, digits and _"
                ));
            }
            key_exists(&format!("{at}.label"), &d.label)?;
            icon_ok(&format!("{at}.icon"), d.icon.as_ref())?;
            if let Some(h) = &d.help {
                key_exists(&format!("{at}.help"), h)?;
            }
            if let Some(g) = &d.group
                && !groups.contains_key(g)
            {
                return Err(format!("{file}: {at}.group {g:?} has no [groups.{g}]"));
            }
            check_keys(d).map_err(|e| format!("{file}: {at}: {e}"))?;
            match d.kind {
                FieldType::Choice => {
                    let mut seen = BTreeSet::new();
                    for c in &d.choices {
                        key_exists(&format!("{at}.choices {:?} label", c.value), &c.label)?;
                        if !seen.insert(c.value.as_str()) {
                            return Err(format!("{file}: {at}: choice {:?} twice", c.value));
                        }
                        if let Some(css) = &c.css {
                            if matches!(owner, Owner::Plugin) {
                                return Err(format!(
                                    "{file}: {at}: plugin choices cannot add stylesheets"
                                ));
                            }
                            safe_path(css).map_err(|e| format!("{file}: {at}: {e}"))?;
                        }
                    }
                    if d.choices.iter().all(|c| c.draft) {
                        return Err(format!("{file}: {at}: a choice needs a non-draft choice"));
                    }
                }
                FieldType::Number => {
                    if let (Some(lo), Some(hi)) = (d.min, d.max)
                        && lo > hi
                    {
                        return Err(format!("{file}: {at}: min is above max"));
                    }
                    if d.step.is_some_and(|s| s <= 0.0 || !s.is_finite()) {
                        return Err(format!("{file}: {at}: step must be above 0"));
                    }
                }
                FieldType::Color => match (owner, &d.token) {
                    (Owner::Plugin, Some(_)) => {
                        return Err(format!("{file}: {at}: only theme colors replace a token"));
                    }
                    (Owner::Theme(tokens), token) => {
                        let token = token.as_deref().unwrap_or(key);
                        if !tokens.contains(token) {
                            return Err(format!(
                                "{file}: {at} overrides unknown color token {token:?}"
                            ));
                        }
                    }
                    (Owner::Plugin, None) => {}
                },
                _ => {}
            }
            let default =
                check_value(d, &d.default).map_err(|e| format!("{file}: {at}.default: {e}"))?;
            defaults.insert(key.clone(), default);
        }
        Ok(Self { decls, defaults })
    }

    /// One value (JSON as stored) against its declaration, the check [`Self::resolve`]
    /// applies; `None` when `key` is not declared.
    pub fn check_one(&self, key: &str, raw: &str) -> Option<Result<(), String>> {
        let decl = self.decls.get(key)?;
        Some(
            serde_json::from_str::<Json>(raw)
                .map_err(|e| format!("not JSON: {e}"))
                .and_then(|v| check_value(decl, &v))
                .map(|_| ()),
        )
    }

    /// The value of every declared option: the stored one when it passes its declaration,
    /// else the default. Each fallback and each stored key with no declaration is reported
    /// (`owner` like `theme:minimal`), for the Worker log.
    pub fn resolve(
        &self,
        owner: &str,
        stored: &[(String, String)],
    ) -> (BTreeMap<String, Json>, Vec<String>) {
        let mut out = self.defaults.clone();
        let mut problems = Vec::new();
        for (key, raw) in stored {
            let Some(decl) = self.decls.get(key) else {
                problems.push(format!("option.unknown owner={owner} key={key}"));
                continue;
            };
            let checked = serde_json::from_str::<Json>(raw)
                .map_err(|e| format!("not JSON: {e}"))
                .and_then(|v| check_value(decl, &v));
            match checked {
                Ok(v) => {
                    out.insert(key.clone(), v);
                }
                Err(reason) => problems.push(format!(
                    "option.invalid owner={owner} key={key} reason={reason}; using the default"
                )),
            }
        }
        (out, problems)
    }
}

/// What templates read for an option: JSON as stored, except `image`, which becomes an
/// object so a resolved URL and size can be added later without changing templates.
pub(crate) fn template_values(
    schema: &Schema,
    values: &BTreeMap<String, Json>,
) -> BTreeMap<String, Json> {
    values
        .iter()
        .map(|(k, v)| {
            let v = match schema.decls.get(k).map(|d| d.kind) {
                Some(FieldType::Image) => match v.as_i64() {
                    Some(id) => serde_json::json!({ "id": id }),
                    None => Json::Null,
                },
                _ => v.clone(),
            };
            (k.clone(), v)
        })
        .collect()
}

fn is_icon(i: &str) -> bool {
    !i.is_empty()
        && i.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn is_key(k: &str) -> bool {
    k.starts_with(|c: char| c.is_ascii_lowercase())
        && k.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

/// A path inside the owner's folder: relative, no `..`, no empty or backslash segments.
pub(crate) fn safe_path(p: &str) -> Result<(), String> {
    if p.is_empty()
        || p.starts_with('/')
        || p.contains('\\')
        || p.contains("//")
        || p.split('/').any(|s| s == ".." || s == ".")
    {
        Err(format!("{p:?} must be a path inside the folder"))
    } else {
        Ok(())
    }
}

/// Keys that belong to another type are a mistake, not something to ignore.
fn check_keys(d: &OptionDecl) -> Result<(), String> {
    let present = [
        ("token", d.token.is_some()),
        ("choices", !d.choices.is_empty()),
        ("min", d.min.is_some()),
        ("max", d.max.is_some()),
        ("step", d.step.is_some()),
        ("max_len", d.max_len.is_some()),
        ("max_items", d.max_items.is_some()),
    ];
    let allowed = d.kind.extra_keys();
    for (name, set) in present {
        if set && !allowed.contains(&name) {
            return Err(format!("{name} does not apply to {}", d.kind.name()));
        }
    }
    if d.kind == FieldType::Choice && d.choices.is_empty() {
        return Err("a choice needs choices".to_owned());
    }
    Ok(())
}

/// Checks one value against its declaration; returns it as kept (an empty image is `""`).
pub(crate) fn check_value(d: &OptionDecl, v: &Json) -> Result<Json, String> {
    let line = |s: &str, max: usize| -> Result<(), String> {
        if s.contains(['\n', '\r']) {
            Err("line breaks are not allowed".to_owned())
        } else if s.chars().count() > max {
            Err(format!("longer than {max} characters"))
        } else {
            Ok(())
        }
    };
    match d.kind {
        FieldType::ShortText => {
            let s = v.as_str().ok_or("expected text")?;
            line(s, d.max_len.unwrap_or(SHORT_TEXT_MAX))?;
        }
        FieldType::LongText => {
            let s = v.as_str().ok_or("expected text")?;
            let max = d.max_len.unwrap_or(LONG_TEXT_MAX);
            if s.chars().count() > max {
                return Err(format!("longer than {max} characters"));
            }
        }
        FieldType::Number => {
            let n = v.as_f64().ok_or("expected a number")?;
            if d.min.is_some_and(|lo| n < lo) || d.max.is_some_and(|hi| n > hi) {
                return Err(format!(
                    "{n} is outside {}..{}",
                    d.min.map_or("-".into(), |x| x.to_string()),
                    d.max.map_or("-".into(), |x| x.to_string())
                ));
            }
            if let Some(step) = d.step {
                let steps = (n - d.min.unwrap_or(0.0)) / step;
                if (steps - steps.round()).abs() > 1e-9 {
                    return Err(format!("{n} is not on the step of {step}"));
                }
            }
            // Stored JSON may say 10.0 for 10; templates and core read whole numbers as
            // integers, so keep them as integers. A field whose bounds and step are all
            // whole only takes whole numbers.
            if let Some(i) = whole(n) {
                return Ok(Json::from(i));
            }
            if [d.min, d.max, d.step]
                .iter()
                .all(|x| x.is_none_or(|x| whole(x).is_some()))
            {
                return Err(format!("{n} is not a whole number"));
            }
        }
        FieldType::Toggle => {
            v.as_bool().ok_or("expected true or false")?;
        }
        FieldType::Choice => {
            let s = v.as_str().ok_or("expected text")?;
            if !d.choices.iter().any(|c| c.value == s && !c.draft) {
                return Err(format!("{s:?} is not an offered choice"));
            }
        }
        FieldType::Color => {
            let obj = v.as_object().ok_or("expected { light, dark }")?;
            if obj.len() != 2 {
                return Err("expected exactly light and dark".to_owned());
            }
            for mode in ["light", "dark"] {
                let c = obj
                    .get(mode)
                    .and_then(Json::as_str)
                    .ok_or(format!("{mode} is missing"))?;
                // Colors go into CSS: anything but a hex color could inject rules.
                if !is_hex_color(c) {
                    return Err(format!("{mode} {c:?} is not a #hex color"));
                }
            }
        }
        FieldType::Image => match v {
            Json::String(s) if s.is_empty() => {}
            Json::Number(n) => match n.as_f64().and_then(whole) {
                Some(id) if id > 0 => return Ok(Json::from(id)),
                _ => return Err("expected a media id or \"\"".to_owned()),
            },
            _ => return Err("expected a media id or \"\"".to_owned()),
        },
        FieldType::List => {
            let items = v.as_array().ok_or("expected a list")?;
            let max = d.max_items.unwrap_or(LIST_MAX_ITEMS);
            if items.len() > max {
                return Err(format!("more than {max} items"));
            }
            for item in items {
                line(
                    item.as_str().ok_or("list items are text")?,
                    d.max_len.unwrap_or(SHORT_TEXT_MAX),
                )?;
            }
        }
    }
    Ok(v.clone())
}

/// `n` as an integer when it is one (and exactly representable).
fn whole(n: f64) -> Option<i64> {
    // 2^53: beyond it f64 cannot tell neighbouring integers apart.
    const EXACT: f64 = 9_007_199_254_740_992.0;
    #[allow(clippy::cast_possible_truncation)] // checked: whole and within +-2^53
    (n.fract() == 0.0 && n.abs() <= EXACT).then_some(n as i64)
}

fn is_hex_color(c: &str) -> bool {
    c.strip_prefix('#').is_some_and(|h| {
        matches!(h.len(), 3 | 4 | 6 | 8) && h.bytes().all(|b| b.is_ascii_hexdigit())
    })
}

/// The option schema for people and AI writing themes and plugins, built from the types
/// above (`blog guide options`).
#[must_use]
pub fn reference() -> String {
    let mut o = String::from(
        "# Theme and plugin options\n\n\
         Declared in `theme.toml` or `plugin.toml` as `[options.<key>]` (keys: lowercase letters, \
         digits, `_`). Templates read `theme.options.<key>`; plugin partials read \
         `plugin.options.<key>`. Stored values are JSON; one that fails its declaration is \
         logged and the default is used.\n\n## Every option\n\n",
    );
    for (k, doc) in COMMON_KEYS {
        let _ = writeln!(o, "- `{k}`: {doc}");
    }
    o.push_str("\n## Field types\n");
    for t in FieldType::ALL {
        let _ = write!(o, "\n### `{}`\n\nValue: {}.\n", t.name(), t.value_doc());
        if !t.extra_keys().is_empty() {
            let keys: Vec<String> = t.extra_keys().iter().map(|k| format!("`{k}`")).collect();
            let _ = writeln!(o, "Also: {}.", keys.join(", "));
        }
        if t == FieldType::Choice {
            o.push_str(
                "Each choice: `value`, `label` (locale key), `draft` (not offered), and in a \
                 theme `css` (a stylesheet shipped after `style.css` when chosen).\n",
            );
        }
    }
    o
}

#[cfg(test)]
mod tests {
    use super::{FieldType, OptionDecl, check_value, reference};
    use serde_json::json;

    fn parsed(src: &str) -> Result<OptionDecl, serde_json::Error> {
        serde_json::from_str(&plumblog_i18n::toml_to_json(src).unwrap())
    }

    fn decl(src: &str) -> OptionDecl {
        parsed(src).unwrap()
    }

    #[test]
    fn stored_values_must_fit_their_declaration() {
        let n = decl("type='number'\nlabel='x'\nmin=0\nmax=120\nstep=5\ndefault=55");
        assert!(check_value(&n, &json!(60)).is_ok());
        // Whole floats are kept as integers; fractions fail where bounds and step are whole.
        assert_eq!(check_value(&n, &json!(10.0)), Ok(json!(10)));
        assert!(check_value(&n, &json!(10.0)).unwrap().is_u64());
        let any = decl("type='number'\nlabel='x'\ndefault=1");
        assert_eq!(check_value(&any, &json!(2.0)), Ok(json!(2)));
        assert!(
            check_value(&any, &json!(2.5))
                .unwrap_err()
                .contains("whole")
        );
        let frac = decl("type='number'\nlabel='x'\nmin=0\nmax=1\nstep=0.25\ndefault=0.5");
        assert_eq!(check_value(&frac, &json!(0.75)), Ok(json!(0.75)));
        for bad in [json!(61), json!(-5), json!(125), json!("55"), json!(null)] {
            assert!(check_value(&n, &bad).is_err(), "{bad}");
        }
        let c = decl(
            "type='choice'\nlabel='x'\ndefault='a'\nchoices=[{value='a',label='l'},{value='b',label='l',draft=true}]",
        );
        assert!(check_value(&c, &json!("a")).is_ok());
        assert!(check_value(&c, &json!("b")).is_err(), "draft");
        assert!(check_value(&c, &json!("z")).is_err());
        let col = decl("type='color'\nlabel='x'\ndefault={light='#fff',dark='#000'}");
        assert!(check_value(&col, &json!({"light": "#8a2d6f", "dark": "#e2a3d2ff"})).is_ok());
        for bad in [
            json!({"light": "red", "dark": "#000"}),
            json!({"light": "#fff;}body{display:none", "dark": "#000"}),
            json!({"light": "#fff"}),
            json!({"light": "#fff", "dark": "#000", "x": "#111"}),
        ] {
            assert!(check_value(&col, &bad).is_err(), "{bad}");
        }
        let img = decl("type='image'\nlabel='x'\ndefault=''");
        assert!(check_value(&img, &json!(7)).is_ok());
        assert_eq!(check_value(&img, &json!(7.0)), Ok(json!(7)));
        assert!(check_value(&img, &json!(7.5)).is_err());
        assert!(check_value(&img, &json!("")).is_ok());
        assert!(check_value(&img, &json!(0)).is_err());
        assert!(check_value(&img, &json!("/x.png")).is_err());
        let list = decl("type='list'\nlabel='x'\ndefault=[]\nmax_items=2\nmax_len=3");
        assert!(check_value(&list, &json!(["ab", "cde"])).is_ok());
        for bad in [
            json!(["a", "b", "c"]),
            json!(["abcd"]),
            json!(["a\nb"]),
            json!([1]),
        ] {
            assert!(check_value(&list, &bad).is_err(), "{bad}");
        }
        let text = decl("type='short_text'\nlabel='x'\ndefault=''\nmax_len=3");
        assert!(
            check_value(&text, &json!("\u{0111}\u{0111}\u{0111}")).is_ok(),
            "chars, not bytes"
        );
        assert!(check_value(&text, &json!("abcd")).is_err());
        assert!(check_value(&text, &json!("a\nb")).is_err());
    }

    #[test]
    fn unknown_types_and_stray_keys_do_not_parse() {
        assert!(parsed("type='slider'\nlabel='x'\ndefault=1").is_err());
        assert!(parsed("type='toggle'\nlabel='x'\ndefault=true\ncolour=1").is_err());
    }

    #[test]
    fn the_reference_covers_every_type() {
        let r = reference();
        for t in FieldType::ALL {
            assert!(r.contains(&format!("### `{}`", t.name())), "{}", t.name());
        }
    }
}
