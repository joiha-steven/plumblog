//! The API token: where it comes from, how it is kept hidden, and how it is verified.

use super::client::{ApiError, ApiErrorKind, Call, Client};
use super::endpoint::{Endpoint, TOKENS_PAGE};
use crate::text::{Code, Text};
use plumblog_i18n::key;
use serde::Deserialize;
use serde_json::Value;
use std::fmt::{self, Write};
use std::path::Path;

pub const TOKEN_VAR: &str = "CLOUDFLARE_API_TOKEN";
pub const ACCOUNT_VAR: &str = "CLOUDFLARE_ACCOUNT_ID";

/// A secret bearer value (API token or upload JWT). Debug and Display print `[hidden]`; only
/// the client reads the value, to put it in the Authorization header.
pub struct ApiToken(String);

impl ApiToken {
    /// # Errors
    /// `Malformed` if the value is empty or holds anything but visible ASCII, which would
    /// break, or inject into, the Authorization header.
    pub fn new(value: impl Into<String>) -> Result<Self, TokenError> {
        let value = value.into();
        let value = value.trim();
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_graphic()) {
            return Err(TokenError::Malformed);
        }
        Ok(Self(value.to_string()))
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[hidden]")
    }
}

impl fmt::Display for ApiToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("[hidden]")
    }
}

/// Token and optional account id, from the environment or `.env`.
#[derive(Debug)]
pub struct Credentials {
    pub token: ApiToken,
    pub account_id: Option<String>,
}

/// `KEY=VALUE` lines of a `.env` file: `#` comments, blank lines, an optional `export `,
/// and one pair of matching quotes around the value. Errors name the line, never its text,
/// which may hold the token.
fn parse_dotenv(text: &str) -> Result<Vec<(String, String)>, usize> {
    let mut out = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line);
        let (k, v) = line.split_once('=').ok_or(i + 1)?;
        let k = k.trim();
        if k.is_empty() || !k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            return Err(i + 1);
        }
        let v = v.trim();
        let v = ['"', '\'']
            .iter()
            .find_map(|q| v.strip_prefix(*q).and_then(|r| r.strip_suffix(*q)))
            .unwrap_or(v);
        out.push((k.to_string(), v.to_string()));
    }
    Ok(out)
}

impl Credentials {
    /// Process environment first, then `.env` in `dir`. Empty values count as unset.
    ///
    /// # Errors
    /// `Missing`, `Malformed`, `BadAccountId`, or `DotEnv` if `.env` exists but cannot be read.
    pub fn load(dir: &Path) -> Result<Self, TokenError> {
        let path = dir.join(".env");
        let dotenv = match std::fs::read_to_string(&path) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                return Err(TokenError::DotEnvUnreadable(
                    path.display().to_string(),
                    e.to_string(),
                ));
            }
        };
        Self::from_sources(&|k| std::env::var(k).ok(), dotenv.as_deref())
    }

    /// [`Credentials::load`] with the sources passed in, for tests.
    ///
    /// # Errors
    /// As [`Credentials::load`].
    pub fn from_sources(
        env: &dyn Fn(&str) -> Option<String>,
        dotenv: Option<&str>,
    ) -> Result<Self, TokenError> {
        let pairs = match dotenv {
            Some(text) => parse_dotenv(text).map_err(TokenError::DotEnvLine)?,
            None => Vec::new(),
        };
        let get = |key: &str| {
            env(key).filter(|v| !v.trim().is_empty()).or_else(|| {
                pairs
                    .iter()
                    .rev()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| v.clone())
                    .filter(|v| !v.trim().is_empty())
            })
        };
        let token = ApiToken::new(get(TOKEN_VAR).ok_or(TokenError::Missing)?)?;
        let account_id = get(ACCOUNT_VAR).map(|v| v.trim().to_string());
        if let Some(id) = &account_id {
            // A swapped .env puts the token here; never echo it back.
            if id == token.expose() {
                return Err(TokenError::AccountIdIsToken);
            }
            if !id.bytes().all(|b| b.is_ascii_alphanumeric()) {
                let shown = printable(&id.replace(token.expose(), "[hidden]"));
                return Err(TokenError::BadAccountId(shown));
            }
        }
        Ok(Self { token, account_id })
    }
}

/// An account the token can see.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Account {
    pub id: String,
    pub name: String,
}

/// Who owns the token; account-owned tokens verify under the account.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    User,
    Account,
}

/// A token Cloudflare accepted, and the account `blog` will use.
#[derive(Clone, Debug)]
pub struct Verified {
    pub account: Account,
    pub kind: TokenKind,
    pub expires_on: Option<String>,
}

/// Why the token cannot be used. Texts it holds are already scrubbed of the token.
#[derive(Debug)]
pub enum TokenError {
    Missing,
    Malformed,
    BadAccountId(String),
    AccountIdIsToken,
    /// `(path, reason)`.
    DotEnvUnreadable(String, String),
    /// Line number; the line itself may hold the token, so it is never kept.
    DotEnvLine(usize),
    Invalid,
    Disabled,
    Expired(Option<String>),
    NotActive(String),
    NoAccount,
    SeveralAccounts(Vec<Account>),
    AccountNotVisible(String, Vec<Account>),
    Api(ApiError),
}

/// Text from Cloudflare or the user, safe to print on a terminal.
fn printable(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

fn account_lines(accounts: &[Account]) -> String {
    let mut out = String::new();
    for a in accounts {
        let _ = write!(out, "\n  {}  {}", printable(&a.id), printable(&a.name));
    }
    out
}

impl TokenError {
    #[must_use]
    pub fn code(&self) -> Code {
        match self {
            Self::Missing => Code::TokenMissing,
            Self::Malformed => Code::TokenMalformed,
            Self::BadAccountId(_) => Code::AccountIdInvalid,
            Self::AccountIdIsToken => Code::AccountIdIsToken,
            Self::DotEnvUnreadable(..) => Code::DotenvUnreadable,
            Self::DotEnvLine(_) => Code::DotenvLineInvalid,
            Self::Invalid => Code::TokenInvalid,
            Self::Disabled => Code::TokenDisabled,
            Self::Expired(_) => Code::TokenExpired,
            Self::NotActive(_) => Code::TokenInactive,
            Self::NoAccount => Code::AccountNone,
            Self::SeveralAccounts(_) => Code::AccountAmbiguous,
            Self::AccountNotVisible(..) => Code::AccountNotVisible,
            Self::Api(e) => e.code(),
        }
    }

    /// The message for the owner, in `text`'s language.
    #[must_use]
    pub fn message(&self, text: &Text) -> String {
        let s = |v: &str| v.to_string();
        let common = [
            ("token_var", s(TOKEN_VAR)),
            ("account_var", s(ACCOUNT_VAR)),
            ("page", s(TOKENS_PAGE)),
        ];
        let with = |key, extra: &[(&'static str, String)]| {
            let mut args = common.to_vec();
            args.extend_from_slice(extra);
            text.t(key, &args)
        };
        let list = |l: &[Account]| {
            if l.is_empty() {
                text.t(key!("cli.error.account_list_empty"), &[])
            } else {
                account_lines(l)
            }
        };
        match self {
            Self::Missing => with(key!("cli.error.token_missing"), &[]),
            Self::Malformed => with(key!("cli.error.token_malformed"), &[]),
            Self::BadAccountId(id) => {
                with(key!("cli.error.account_id_invalid"), &[("id", id.clone())])
            }
            Self::AccountIdIsToken => with(key!("cli.error.account_id_is_token"), &[]),
            Self::DotEnvUnreadable(path, reason) => with(
                key!("cli.error.dotenv_unreadable"),
                &[("path", path.clone()), ("reason", reason.clone())],
            ),
            Self::DotEnvLine(n) => with(
                key!("cli.error.dotenv_line_invalid"),
                &[("line", n.to_string())],
            ),
            Self::Invalid => with(key!("cli.error.token_invalid"), &[]),
            Self::Disabled => with(key!("cli.error.token_disabled"), &[]),
            Self::Expired(Some(d)) => with(
                key!("cli.error.token_expired_on"),
                &[("date", printable(d))],
            ),
            Self::Expired(None) => with(key!("cli.error.token_expired"), &[]),
            Self::NotActive(st) => with(
                key!("cli.error.token_inactive"),
                &[("status", printable(st))],
            ),
            Self::NoAccount => with(key!("cli.error.account_none"), &[]),
            Self::SeveralAccounts(l) => {
                with(key!("cli.error.account_ambiguous"), &[("list", list(l))])
            }
            Self::AccountNotVisible(id, l) => with(
                key!("cli.error.account_not_visible"),
                &[("id", printable(id)), ("list", list(l))],
            ),
            Self::Api(e) => e.message(text),
        }
    }
}

/// Language-free, for logs: the code and its data.
impl fmt::Display for TokenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Api(e) => e.fmt(f),
            Self::BadAccountId(v) | Self::NotActive(v) => {
                write!(f, "{} {v:?}", self.code().as_str())
            }
            Self::DotEnvUnreadable(p, r) => write!(f, "{} {p}: {r}", self.code().as_str()),
            Self::DotEnvLine(n) => write!(f, "{} {n}", self.code().as_str()),
            Self::Expired(d) => {
                write!(f, "{} {}", self.code().as_str(), d.as_deref().unwrap_or(""))
            }
            Self::AccountNotVisible(id, _) => write!(f, "{} {id}", self.code().as_str()),
            _ => f.write_str(self.code().as_str()),
        }
    }
}

impl std::error::Error for TokenError {}

impl From<ApiError> for TokenError {
    fn from(e: ApiError) -> Self {
        if e.kind == ApiErrorKind::InvalidToken {
            Self::Invalid
        } else {
            Self::Api(e)
        }
    }
}

/// `result` of a token verify call.
fn check_status(result: &Value) -> Result<Option<String>, TokenError> {
    let expires_on = result
        .get("expires_on")
        .and_then(Value::as_str)
        .map(str::to_string);
    match result.get("status").and_then(Value::as_str) {
        Some("active") => Ok(expires_on),
        Some("disabled") => Err(TokenError::Disabled),
        Some("expired") => Err(TokenError::Expired(expires_on)),
        other => Err(TokenError::NotActive(
            other.unwrap_or("missing").to_string(),
        )),
    }
}

fn list_accounts(client: &Client) -> Result<Vec<Account>, TokenError> {
    Ok(client.call_all_as(&Call::new(Endpoint::AccountsList), 50, None)?)
}

/// Verifies the token, picks the account, and sets it on the client.
///
/// User tokens verify at `/user/tokens/verify`; account-owned tokens fail there with code
/// 1000 (as wrangler's `getTokenType` relies on) and verify under their account instead.
///
/// # Errors
/// [`TokenError`] with a message saying what to do.
pub fn verify(client: &mut Client, account_id: Option<&str>) -> Result<Verified, TokenError> {
    let (kind, mut expires_on) = match client.call(&Call::new(Endpoint::UserTokenVerify)) {
        Ok(result) => (TokenKind::User, Some(check_status(&result)?)),
        Err(e) if e.kind == ApiErrorKind::InvalidToken => (TokenKind::Account, None),
        Err(e) => return Err(TokenError::Api(e)),
    };
    let verify_account = |client: &mut Client, id: &str| -> Result<Option<String>, TokenError> {
        client.set_account(id)?;
        check_status(&client.call(&Call::new(Endpoint::AccountTokenVerify))?)
    };
    // With a known account, verify first: an expired token fails every other call with 401.
    if expires_on.is_none()
        && let Some(id) = account_id
    {
        expires_on = Some(verify_account(client, id)?);
    }
    let accounts = list_accounts(client)?;
    let account = match account_id {
        Some(id) => accounts
            .iter()
            .find(|a| a.id == id)
            .cloned()
            .ok_or_else(|| {
                TokenError::AccountNotVisible(client.scrub(id, &[]), accounts.clone())
            })?,
        None => match accounts.as_slice() {
            [] => return Err(TokenError::NoAccount),
            [one] => one.clone(),
            _ => return Err(TokenError::SeveralAccounts(accounts)),
        },
    };
    let expires_on = match expires_on {
        Some(e) => e,
        None => verify_account(client, &account.id)?,
    };
    client.set_account(&account.id)?;
    Ok(Verified {
        account,
        kind,
        expires_on,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_never_prints() {
        let t = ApiToken::new("sekret-123").unwrap();
        assert_eq!(format!("{t} {t:?}"), "[hidden] [hidden]");
        let c = Credentials {
            token: t,
            account_id: None,
        };
        assert!(!format!("{c:?}").contains("sekret"));
        assert!(ApiToken::new("a b").is_err());
        assert!(ApiToken::new("a\r\nX-Evil: 1").is_err());
        assert!(ApiToken::new("  ").is_err());
    }

    #[test]
    fn env_wins_over_dotenv_and_dotenv_parses() {
        let dotenv = "# comment\n\nexport CLOUDFLARE_API_TOKEN=\"from-file\"\nCLOUDFLARE_ACCOUNT_ID='abc123'\n";
        let none = |_: &str| None;
        let c = Credentials::from_sources(&none, Some(dotenv)).unwrap();
        assert_eq!(c.token.expose(), "from-file");
        assert_eq!(c.account_id.as_deref(), Some("abc123"));

        let env = |k: &str| (k == TOKEN_VAR).then(|| "from-env".to_string());
        let c = Credentials::from_sources(&env, Some(dotenv)).unwrap();
        assert_eq!(c.token.expose(), "from-env");

        let empty = |_: &str| Some(String::new());
        assert!(matches!(
            Credentials::from_sources(&empty, None),
            Err(TokenError::Missing)
        ));
        let err = Credentials::from_sources(&none, Some("CLOUDFLARE_API_TOKEN secret-value"))
            .unwrap_err();
        let msg = err.message(&Text::new("en"));
        assert!(msg.contains("Line 1"), "{msg}");
        assert!(
            !msg.contains("secret-value") && !format!("{err} {err:?}").contains("secret-value")
        );
        assert!(matches!(
            Credentials::from_sources(
                &none,
                Some("CLOUDFLARE_API_TOKEN=x\nCLOUDFLARE_ACCOUNT_ID=a/b")
            ),
            Err(TokenError::BadAccountId(_))
        ));
    }

    /// A swapped .env (the token in `CLOUDFLARE_ACCOUNT_ID`) never prints the token, for a
    /// token with `-`/`_` (fails the id shape) and an alphanumeric one (passes it).
    #[test]
    fn swapped_env_never_echoes_the_token() {
        for tok in ["tok-abc_DEF-123_xyz", "abcDEF0123456789abcDEF0123456789zz"] {
            let none = |_: &str| None;
            let swapped = format!("CLOUDFLARE_API_TOKEN={tok}\nCLOUDFLARE_ACCOUNT_ID={tok}\n");
            let err = Credentials::from_sources(&none, Some(&swapped)).unwrap_err();
            assert!(
                matches!(err, TokenError::AccountIdIsToken),
                "{tok}: {err:?}"
            );
            // The token inside a longer, malformed id is hidden too.
            let longer = format!("CLOUDFLARE_API_TOKEN={tok}\nCLOUDFLARE_ACCOUNT_ID={tok}/x\n");
            let err2 = Credentials::from_sources(&none, Some(&longer)).unwrap_err();
            for lang in ["en", "vi"] {
                for e in [&err, &err2] {
                    let shown = format!("{} {e} {e:?}", e.message(&Text::new(lang)));
                    assert!(!shown.contains(tok), "{lang}: {shown}");
                }
            }
        }
    }
}
