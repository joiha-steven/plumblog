//! Declarative plugins (`plugins/<name>/`): options, HTML partials for slots the theme
//! declares, browser CSS/JS and locales. No server code: partials are templates in the same
//! sandbox as theme templates (autoescape on), and they only fill slots.
//!
//! ```text
//! plugin.toml          name, version, slots, css, js, [groups.*], [options.*]
//! partials/<slot>.html one per declared slot
//! assets/              served at <assets_url>/<path in assets/>
//! locales/<lang>.toml  the plugin's own text; partials call t("section.key")
//! ```

use crate::SLOTS;
use crate::css;
use crate::options::{Group, OptionDecl, Owner, Schema, safe_path};
use crate::theme::{Catalogs, ThemeError, ThemeFiles, catalogs, locale_strings};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    name: String,
    version: String,
    #[serde(default)]
    slots: Vec<String>,
    #[serde(default)]
    css: Vec<String>,
    #[serde(default)]
    js: Vec<String>,
    #[serde(default)]
    groups: BTreeMap<String, Group>,
    #[serde(default)]
    options: BTreeMap<String, OptionDecl>,
}

/// A plugin that passed every load check. Whether it is enabled is the site's choice
/// ([`crate::Theme::with_options`]).
pub struct Plugin {
    pub(crate) name: String,
    pub(crate) version: String,
    pub(crate) schema: Schema,
    /// `(slot, template source)`.
    pub(crate) partials: Vec<(String, String)>,
    /// Minified, inlined after the theme's CSS when enabled.
    pub(crate) css: String,
    /// Public URLs, loaded deferred when enabled.
    pub(crate) js: Vec<String>,
    pub(crate) catalogs: Catalogs,
}

/// Template tags a partial may not use: it fills a slot, it never takes over the page.
const FORBIDDEN_TAGS: &[&str] = &["extends", "block", "include", "import", "from"];

impl Plugin {
    /// Loads the plugin in folder `folder`. `assets_url` is where its `assets/` folder is
    /// served, without a trailing slash.
    ///
    /// # Errors
    /// Anything wrong with the folder, naming the file, so `blog plugin check` and the log
    /// say exactly what to fix.
    pub fn load(
        folder: &str,
        files: &dyn ThemeFiles,
        assets_url: &str,
    ) -> Result<Self, ThemeError> {
        let err = |m: String| ThemeError::new(format!("plugins/{folder}/{m}"));
        if !is_name(folder) {
            return Err(err(
                "plugin names are lowercase letters, digits and -, starting with a letter".into(),
            ));
        }
        let src = files
            .read("plugin.toml")
            .ok_or_else(|| err("plugin.toml is missing".into()))?;
        let m: Manifest = toml::from_str(src).map_err(|e| err(format!("plugin.toml: {e}")))?;
        if m.name != folder {
            return Err(err(format!(
                "plugin.toml: name {:?} must match the folder name",
                m.name
            )));
        }
        for slot in &m.slots {
            if !SLOTS.contains(&slot.as_str()) {
                return Err(err(format!(
                    "plugin.toml: unknown slot {slot:?}; known: {}",
                    SLOTS.join(", ")
                )));
            }
        }
        for path in files.paths() {
            let top = path.split('/').next().unwrap_or_default();
            if !(path == "plugin.toml" || matches!(top, "partials" | "assets" | "locales")) {
                return Err(err(format!(
                    "{path}: not part of a plugin (plugin.toml, partials/, assets/, locales/)"
                )));
            }
            if top == "partials" && !m.slots.iter().any(|s| path == format!("partials/{s}.html")) {
                return Err(err(format!("{path}: no slot of that name in plugin.toml")));
            }
        }
        let mut partials = Vec::new();
        for slot in &m.slots {
            if partials.iter().any(|(s, _): &(String, String)| s == slot) {
                return Err(err(format!("plugin.toml: slot {slot:?} twice")));
            }
            let path = format!("partials/{slot}.html");
            let src = files
                .read(&path)
                .ok_or_else(|| err(format!("{path} is missing")))?;
            if let Some(tag) = forbidden_tag(src) {
                return Err(err(format!(
                    "{path}: {{% {tag} %}} is not allowed; a partial only fills its slot"
                )));
            }
            // Compile once here so a syntax error is a load error, not a broken page.
            let mut env = minijinja::Environment::new();
            env.add_template("partial", src)
                .map_err(|e| err(format!("{path}: {e:#}")))?;
            partials.push((slot.clone(), src.to_owned()));
        }
        let asset = |list: &[String]| -> Result<Vec<String>, ThemeError> {
            list.iter()
                .map(|p| {
                    safe_path(p).map_err(|e| err(format!("plugin.toml: {e}")))?;
                    let rest = p
                        .strip_prefix("assets/")
                        .ok_or_else(|| err(format!("plugin.toml: {p:?} must be in assets/")))?;
                    if files.read(p).is_none() {
                        return Err(err(format!("{p} is missing")));
                    }
                    Ok(rest.to_owned())
                })
                .collect()
        };
        let mut css_src = String::new();
        for p in asset(&m.css)? {
            css_src.push_str(files.read(&format!("assets/{p}")).unwrap_or_default());
            css_src.push('\n');
        }
        let js = asset(&m.js)?
            .into_iter()
            .map(|p| format!("{assets_url}/{p}"))
            .collect();
        let strings = locale_strings(files, None).map_err(|e| err(e.detail))?;
        let english = strings.get("en").cloned().unwrap_or_default();
        let schema = Schema::check("plugin.toml", m.options, &m.groups, &english, Owner::Plugin)
            .map_err(err)?;
        Ok(Self {
            name: m.name,
            version: m.version,
            schema,
            partials,
            css: css::minify(&css_src),
            js,
            catalogs: catalogs(strings),
        })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Bytes this plugin adds to the inlined stylesheet when enabled (size budget).
    #[must_use]
    pub fn css_len(&self) -> usize {
        self.css.len()
    }

    pub(crate) fn template_name(&self, slot: &str) -> String {
        format!("@plugin/{}/{slot}.html", self.name)
    }
}

fn is_name(n: &str) -> bool {
    n.starts_with(|c: char| c.is_ascii_lowercase())
        && n.len() <= 64
        && n.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// The first forbidden tag in `src`, if any (`{%`, optional `-`/`+`, spaces, then the word).
fn forbidden_tag(src: &str) -> Option<&'static str> {
    src.match_indices("{%").find_map(|(i, _)| {
        let rest = src[i + 2..].trim_start_matches(['-', '+']).trim_start();
        FORBIDDEN_TAGS.iter().copied().find(|t| {
            rest.strip_prefix(t)
                .is_some_and(|after| !after.starts_with(|c: char| c.is_alphanumeric() || c == '_'))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::{forbidden_tag, is_name};

    #[test]
    fn names_and_tags() {
        assert!(is_name("reading-time"));
        assert!(is_name("a2"));
        for bad in ["", "Reading", "-x", "x_y", "x/y", "..", "x.y"] {
            assert!(!is_name(bad), "{bad}");
        }
        assert_eq!(
            forbidden_tag("{% extends \"base.html\" %}"),
            Some("extends")
        );
        assert_eq!(forbidden_tag("{%- block main %}"), Some("block"));
        assert_eq!(forbidden_tag("{%+include 'x' %}"), Some("include"));
        assert_eq!(forbidden_tag("{% if blocked %}{% endif %}"), None);
        assert_eq!(forbidden_tag("{% for x in imports %}{% endfor %}"), None);
    }
}
