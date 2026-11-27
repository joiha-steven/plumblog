//! Shape checks for account fields sent to the auth API. Each failure is `InvalidInput`
//! naming the field; the admin maps the code to a message.

use plumblog_model::{Error, ErrorCode, Result};

const MAX_LOGIN: usize = 64;
const MAX_SLUG: usize = 64;
const MAX_DISPLAY_NAME: usize = 100;
const MAX_BIO: usize = 2000;
const MAX_PASSKEY_NAME: usize = 64;
const MAX_TOKEN_NAME: usize = 64;

fn invalid(field: &str, why: &str) -> Error {
    Error::new(ErrorCode::InvalidInput, format!("{field}: {why}"))
}

fn plain_text(field: &str, value: &str, max: usize, multiline: bool) -> Result<()> {
    if value.chars().count() > max {
        return Err(invalid(field, "too long"));
    }
    let bad = |c: char| c.is_control() && !(multiline && (c == '\n' || c == '\t'));
    if value.chars().any(bad) {
        return Err(invalid(field, "control character"));
    }
    Ok(())
}

/// A login: 1-64 characters, no control characters and no surrounding whitespace (it is
/// part of the password salt, so invisible differences would lock the user out).
///
/// # Errors
///
/// `InvalidInput` naming the field.
pub fn login(value: &str) -> Result<()> {
    if value.is_empty() || value.trim() != value {
        return Err(invalid("login", "empty or padded with spaces"));
    }
    plain_text("login", value, MAX_LOGIN, false)
}

/// A user slug for author URLs: 1-64 of `a-z 0-9 -`, not starting or ending with `-`.
///
/// # Errors
///
/// `InvalidInput` naming the field.
pub fn slug(value: &str) -> Result<()> {
    let ok = (1..=MAX_SLUG).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-');
    if ok {
        Ok(())
    } else {
        Err(invalid("slug", "use 1-64 of a-z, 0-9 and inner hyphens"))
    }
}

/// # Errors
///
/// `InvalidInput` naming the field.
pub fn display_name(value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(invalid("display_name", "empty"));
    }
    plain_text("display_name", value, MAX_DISPLAY_NAME, false)
}

/// # Errors
///
/// `InvalidInput` naming the field.
pub fn bio(value: &str) -> Result<()> {
    plain_text("bio", value, MAX_BIO, true)
}

/// A passkey's label in the admin: 1-64 characters, not blank, no control characters.
///
/// # Errors
///
/// `InvalidInput` naming the field.
pub fn passkey_name(value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(invalid("name", "empty"));
    }
    plain_text("name", value, MAX_PASSKEY_NAME, false)
}

/// An API token's name: 1-64 characters, not blank, no control characters. It names the
/// client in the system log, so it must be readable.
///
/// # Errors
///
/// `InvalidInput` naming the field.
pub fn token_name(value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(invalid("name", "empty"));
    }
    plain_text("name", value, MAX_TOKEN_NAME, false)
}

#[cfg(test)]
mod tests {
    use super::{bio, display_name, login, passkey_name, slug, token_name};

    #[test]
    fn accepts_and_refuses() {
        assert!(login("Alice").is_ok());
        assert!(login("Tr\u{1ea7}n").is_ok());
        for bad in ["", " a", "a ", "a\u{0}b", &"x".repeat(65)] {
            assert!(login(bad).is_err(), "{bad:?}");
        }
        assert!(slug("alice-2").is_ok());
        for bad in ["", "-a", "a-", "A", "a b", &"a".repeat(65)] {
            assert!(slug(bad).is_err(), "{bad:?}");
        }
        assert!(display_name("Alice").is_ok());
        assert!(display_name("  ").is_err());
        assert!(bio("line\nline").is_ok());
        assert!(bio("\u{7}").is_err());
        assert!(bio("").is_ok());
        assert!(passkey_name("Phone").is_ok());
        for bad in ["", " ", "a\nb", &"x".repeat(65)] {
            assert!(passkey_name(bad).is_err(), "{bad:?}");
        }
        assert!(token_name("Claude \u{111}\u{1ecd}c").is_ok());
        assert!(token_name(&"\u{1ecd}".repeat(64)).is_ok());
        for bad in ["", "  ", "a\nb", "a\u{7f}", "a\tb", &"x".repeat(65)] {
            assert!(token_name(bad).is_err(), "{bad:?}");
        }
    }
}
