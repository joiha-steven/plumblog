//! What `blog theme check` and `blog plugin check` report: one finding per problem, with the
//! file (and line), what is wrong and how to fix it. The id is a stable contract for agents.

use crate::text::Text;
use plumblog_i18n::{Key, key};
use std::fmt::Write;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Error,
    Warning,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    ThemeLoad,
    ThemeTemplate,
    ThemeDarkToken,
    ThemeContrast,
    ThemeCssSize,
    ThemeJsSize,
    ThemeHardColor,
    ThemeLocaleKey,
    ThemePageProblem,
    ThemeAssetMissing,
    ThemeTooLarge,
    FileSecretName,
    FileSecretContent,
    FileStray,
    FileSymlink,
    FileAdmin,
    FileTooMany,
    FileAssetName,
    FileTooLarge,
    FileNotText,
    TextHard,
    PluginLoad,
    PluginBroken,
    PluginCssSize,
}

impl Kind {
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::ThemeLoad => "theme.load",
            Self::ThemeTemplate => "theme.template",
            Self::ThemeDarkToken => "theme.dark_token",
            Self::ThemeContrast => "theme.contrast",
            Self::ThemeCssSize => "theme.css_size",
            Self::ThemeJsSize => "theme.js_size",
            Self::ThemeHardColor => "theme.hard_color",
            Self::ThemeLocaleKey => "theme.locale_key",
            Self::ThemePageProblem => "theme.page_problem",
            Self::ThemeAssetMissing => "theme.asset_missing",
            Self::ThemeTooLarge => "theme.too_large",
            Self::FileSecretName => "file.secret_name",
            Self::FileSecretContent => "file.secret_content",
            Self::FileStray => "file.stray",
            Self::FileSymlink => "file.symlink",
            Self::FileAdmin => "file.admin",
            Self::FileTooMany => "file.too_many",
            Self::FileAssetName => "file.asset_name",
            Self::FileTooLarge => "file.too_large",
            Self::FileNotText => "file.not_text",
            Self::TextHard => "text.hard_coded",
            Self::PluginLoad => "plugin.load",
            Self::PluginBroken => "plugin.broken",
            Self::PluginCssSize => "plugin.css_size",
        }
    }

    /// `(what is wrong, how to fix it)`.
    fn keys(self) -> (Key, Key) {
        match self {
            Self::ThemeLoad => (
                key!("cli.finding.theme_load.what"),
                key!("cli.finding.theme_load.fix"),
            ),
            Self::ThemeTemplate => (
                key!("cli.finding.theme_template.what"),
                key!("cli.finding.theme_template.fix"),
            ),
            Self::ThemeDarkToken => (
                key!("cli.finding.theme_dark_token.what"),
                key!("cli.finding.theme_dark_token.fix"),
            ),
            Self::ThemeContrast => (
                key!("cli.finding.theme_contrast.what"),
                key!("cli.finding.theme_contrast.fix"),
            ),
            Self::ThemeCssSize => (
                key!("cli.finding.theme_css_size.what"),
                key!("cli.finding.theme_css_size.fix"),
            ),
            Self::ThemeJsSize => (
                key!("cli.finding.theme_js_size.what"),
                key!("cli.finding.theme_js_size.fix"),
            ),
            Self::ThemeHardColor => (
                key!("cli.finding.theme_hard_color.what"),
                key!("cli.finding.theme_hard_color.fix"),
            ),
            Self::ThemeLocaleKey => (
                key!("cli.finding.theme_locale_key.what"),
                key!("cli.finding.theme_locale_key.fix"),
            ),
            Self::ThemePageProblem => (
                key!("cli.finding.theme_page_problem.what"),
                key!("cli.finding.theme_page_problem.fix"),
            ),
            Self::ThemeAssetMissing => (
                key!("cli.finding.theme_asset_missing.what"),
                key!("cli.finding.theme_asset_missing.fix"),
            ),
            Self::ThemeTooLarge => (
                key!("cli.finding.theme_too_large.what"),
                key!("cli.finding.theme_too_large.fix"),
            ),
            Self::FileTooLarge => (
                key!("cli.finding.file_too_large.what"),
                key!("cli.finding.file_too_large.fix"),
            ),
            Self::FileNotText => (
                key!("cli.finding.file_not_text.what"),
                key!("cli.finding.file_not_text.fix"),
            ),
            Self::FileAssetName => (
                key!("cli.finding.file_asset_name.what"),
                key!("cli.finding.file_asset_name.fix"),
            ),
            Self::FileSecretName => (
                key!("cli.finding.file_secret_name.what"),
                key!("cli.finding.file_secret_name.fix"),
            ),
            Self::FileSecretContent => (
                key!("cli.finding.file_secret_content.what"),
                key!("cli.finding.file_secret_content.fix"),
            ),
            Self::FileStray => (
                key!("cli.finding.file_stray.what"),
                key!("cli.finding.file_stray.fix"),
            ),
            Self::FileSymlink => (
                key!("cli.finding.file_symlink.what"),
                key!("cli.finding.file_symlink.fix"),
            ),
            Self::FileAdmin => (
                key!("cli.finding.file_admin.what"),
                key!("cli.finding.file_admin.fix"),
            ),
            Self::FileTooMany => (
                key!("cli.finding.file_too_many.what"),
                key!("cli.finding.file_too_many.fix"),
            ),
            Self::TextHard => (
                key!("cli.finding.text_hard.what"),
                key!("cli.finding.text_hard.fix"),
            ),
            Self::PluginLoad => (
                key!("cli.finding.plugin_load.what"),
                key!("cli.finding.plugin_load.fix"),
            ),
            Self::PluginBroken => (
                key!("cli.finding.plugin_broken.what"),
                key!("cli.finding.plugin_broken.fix"),
            ),
            Self::PluginCssSize => (
                key!("cli.finding.plugin_css_size.what"),
                key!("cli.finding.plugin_css_size.fix"),
            ),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Finding {
    pub level: Level,
    pub kind: Kind,
    /// Relative to the checked folder; empty when the problem is the folder itself.
    pub file: String,
    pub line: Option<usize>,
    pub args: Vec<(&'static str, String)>,
}

impl Finding {
    #[must_use]
    pub fn new(level: Level, kind: Kind, file: &str, line: Option<usize>) -> Self {
        Self {
            level,
            kind,
            file: file.to_owned(),
            line,
            args: Vec::new(),
        }
    }

    #[must_use]
    pub fn arg(mut self, name: &'static str, value: impl Into<String>) -> Self {
        self.args.push((name, value.into()));
        self
    }

    /// Two lines: `error [id] file:line: what`, then the fix.
    #[must_use]
    pub fn render(&self, text: &Text) -> String {
        let (what, fix) = self.kind.keys();
        let args: Vec<(&str, String)> = self.args.iter().map(|(k, v)| (*k, v.clone())).collect();
        let level = match self.level {
            Level::Error => text.t(key!("cli.finding.error"), &[]),
            Level::Warning => text.t(key!("cli.finding.warning"), &[]),
        };
        let place = match (self.file.is_empty(), self.line) {
            (true, _) => String::new(),
            (false, None) => format!("{}: ", self.file),
            (false, Some(n)) => format!("{}:{n}: ", self.file),
        };
        let mut out = String::new();
        let _ = writeln!(
            out,
            "{level} [{}] {place}{}",
            self.kind.id(),
            text.t(what, &args)
        );
        let _ = writeln!(
            out,
            "  {} {}",
            text.t(key!("cli.finding.fix"), &[]),
            text.t(fix, &args)
        );
        out
    }
}

/// Every finding, then the count line.
///
/// # Errors
/// Any finding is an error: the same text, summary first.
pub fn report(text: &Text, dir: &str, findings: &[Finding]) -> Result<String, String> {
    let errors = findings.iter().filter(|f| f.level == Level::Error).count();
    let warnings = findings.len() - errors;
    let mut out = String::new();
    for f in findings {
        out.push_str(&f.render(text));
    }
    let args = [
        ("dir", dir.to_owned()),
        ("errors", errors.to_string()),
        ("warnings", warnings.to_string()),
    ];
    let summary = if findings.is_empty() {
        text.t(key!("cli.finding.summary_ok"), &args)
    } else if errors == 0 {
        text.t(key!("cli.finding.summary_warnings"), &args)
    } else {
        text.t(key!("cli.finding.summary_errors"), &args)
    };
    if errors == 0 {
        Ok(format!("{out}{summary}\n"))
    } else {
        Err(format!("{summary}\n{out}").trim_end().to_owned())
    }
}
