//! User-facing text of `blog`: every sentence comes from `locales/<lang>.toml` (`[cli]`), so
//! the CLI speaks the owner's language. Error lines also carry a stable [`Code`], which agents
//! act on in any language.

use plumblog_i18n::{Catalog, Key, key};
use std::collections::HashMap;

include!(concat!(env!("OUT_DIR"), "/locales.rs"));

macro_rules! codes {
    ($($variant:ident => $code:literal),+ $(,)?) => {
        /// Stable error codes `blog` prints. The string form is a contract with agents and
        /// scripts: never rename one once shipped.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum Code { $($variant),+ }

        impl Code {
            pub const ALL: &[Code] = &[$(Code::$variant),+];

            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self { $(Code::$variant => $code),+ }
            }
        }
    };
}

codes! {
    TokenMissing => "cf.token_missing",
    TokenMalformed => "cf.token_malformed",
    TokenInvalid => "cf.token_invalid",
    TokenDisabled => "cf.token_disabled",
    TokenExpired => "cf.token_expired",
    TokenInactive => "cf.token_inactive",
    AccountIdInvalid => "cf.account_id_invalid",
    AccountIdIsToken => "cf.account_id_is_token",
    AccountNone => "cf.account_none",
    AccountAmbiguous => "cf.account_ambiguous",
    AccountNotVisible => "cf.account_not_visible",
    DotenvUnreadable => "cf.dotenv_unreadable",
    DotenvLineInvalid => "cf.dotenv_line_invalid",
    PermissionMissing => "cf.permission_missing",
    AccessDenied => "cf.access_denied",
    HttpFailed => "cf.http_failed",
    Network => "cf.network",
    NetworkWriteUncertain => "cf.network_write_uncertain",
    ReplyMalformed => "cf.reply_malformed",
    BaseRefused => "cf.base_refused",
    Internal => "cli.internal",
    ArgsInvalid => "cli.args_invalid",
    GuideUnknown => "cli.guide_unknown",
}

/// One language's `[cli]` text, English behind it.
pub struct Text {
    catalog: Catalog,
    lang: String,
}

fn strings(lang: &str) -> HashMap<String, String> {
    LOCALES
        .iter()
        .find(|(l, _)| *l == lang)
        // The locale check parses these files on every commit; a broken file falls back.
        .and_then(|(_, src)| plumblog_i18n::parse(src).ok())
        .unwrap_or_default()
}

impl Text {
    /// `lang` must be one of [`LOCALES`]; anything else gives English.
    #[must_use]
    pub fn new(lang: &str) -> Self {
        let lang = if LOCALES.iter().any(|(l, _)| *l == lang) {
            lang
        } else {
            "en"
        };
        Self {
            catalog: Catalog::new(strings(lang), strings("en")).with_lang(lang),
            lang: lang.to_string(),
        }
    }

    #[must_use]
    pub fn lang(&self) -> &str {
        &self.lang
    }

    #[must_use]
    pub fn t(&self, key: Key, args: &[(&str, String)]) -> String {
        self.catalog.t(key, args)
    }

    /// One error line: `error [<code>]: <message>`, in this language.
    #[must_use]
    pub fn error_line(&self, code: Code, message: &str) -> String {
        self.t(
            key!("cli.error_line"),
            &[
                ("code", code.as_str().to_string()),
                ("message", message.to_string()),
            ],
        )
    }
}

/// `vi_VN.UTF-8` -> `vi`; `C` and `POSIX` -> none.
fn base_lang(value: &str) -> Option<String> {
    let v = value.split(['.', '@']).next().unwrap_or("");
    let v = v
        .split(['_', '-'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    (!v.is_empty() && v != "c" && v != "posix").then_some(v)
}

/// Language from `--lang`, else `PLUMBLOG_LANG`, else the OS locale (`LC_ALL`, `LC_MESSAGES`,
/// `LANG`, the first one set, as POSIX does), else English. Unknown languages give English.
#[must_use]
pub fn pick_lang(flag: Option<&str>, env: &dyn Fn(&str) -> Option<String>) -> String {
    let set = |v: Option<String>| v.filter(|v| !v.trim().is_empty());
    let chosen = flag
        .map(str::to_string)
        .or_else(|| set(env("PLUMBLOG_LANG")))
        .or_else(|| {
            ["LC_ALL", "LC_MESSAGES", "LANG"]
                .iter()
                .find_map(|k| set(env(k)))
        });
    chosen
        .as_deref()
        .and_then(base_lang)
        .filter(|l| LOCALES.iter().any(|(name, _)| name == l))
        .unwrap_or_else(|| "en".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_order_and_fallback() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| (*v).to_string())
            }
        };
        let none = env(&[]);
        assert_eq!(pick_lang(None, &none), "en");
        assert_eq!(
            pick_lang(Some("vi"), &env(&[("PLUMBLOG_LANG", "en")])),
            "vi"
        );
        assert_eq!(
            pick_lang(None, &env(&[("PLUMBLOG_LANG", "vi"), ("LANG", "en_US")])),
            "vi"
        );
        assert_eq!(pick_lang(None, &env(&[("LANG", "vi_VN.UTF-8")])), "vi");
        assert_eq!(
            pick_lang(None, &env(&[("LC_ALL", "C"), ("LANG", "vi_VN")])),
            "en"
        );
        assert_eq!(
            pick_lang(None, &env(&[("LC_MESSAGES", "vi-VN"), ("LANG", "en")])),
            "vi"
        );
        assert_eq!(pick_lang(Some("xx"), &none), "en");
        assert_eq!(Text::new("xx").lang(), "en");
    }

    #[test]
    fn codes_are_unique_and_dotted() {
        for (i, c) in Code::ALL.iter().enumerate() {
            assert!(c.as_str().contains('.'), "{c:?}");
            assert!(
                Code::ALL[i + 1..].iter().all(|d| d.as_str() != c.as_str()),
                "{c:?}"
            );
        }
    }

    #[test]
    fn every_language_has_the_cli_text() {
        let en = strings("en");
        assert!(en.keys().any(|k| k.starts_with("cli.")));
        for (lang, _) in LOCALES {
            let line = Text::new(lang).error_line(Code::TokenInvalid, "m");
            assert!(
                line.contains("[cf.token_invalid]") && line.ends_with('m'),
                "{lang}: {line}"
            );
        }
    }
}
