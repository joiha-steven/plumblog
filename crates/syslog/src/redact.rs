//! Redaction: what never reaches the system log (docs/ARCHITECTURE.md, System log: never
//! passwords, tokens or personal data beyond the acting user). Applied when an event is
//! built, so stored rows are already clean.

use crate::event::MAX_TEXT;

/// Stored instead of a redacted value. ASCII and not a locale key: the admin shows it as is.
pub const REDACTED: &str = "[redacted]";

/// Field names whose values are secrets, matched as substrings of the lowercased name.
const SECRET_NAMES: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "authorization",
    "cookie",
    "session",
    "setup_code",
    "otp",
    "credential",
    "private_key",
    "signature",
    "turnstile",
];

/// Field names that hold personal data of someone other than the acting user.
const PERSONAL_NAMES: &[&str] = &["email", "ip", "phone", "address", "user_agent"];

/// The stored form of `value` under `key`: redacted when the key names a secret or personal
/// data, or the value looks like one; otherwise trimmed and capped at [`MAX_TEXT`].
pub fn value(key: &str, value: &str) -> String {
    let key = key.to_ascii_lowercase();
    let named = SECRET_NAMES.iter().any(|s| key.contains(s))
        || PERSONAL_NAMES.iter().any(|p| {
            key == *p || key.ends_with(&format!("_{p}")) || key.starts_with(&format!("{p}_"))
        });
    if named || looks_secret(value) || looks_personal(value) {
        REDACTED.to_owned()
    } else {
        truncate(value.trim(), MAX_TEXT)
    }
}

/// At most `max` characters, never splitting one, with an ellipsis when cut.
pub fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        None => s.to_owned(),
        Some((i, _)) => {
            let mut out = s[..i].to_owned();
            out.push('\u{2026}');
            out
        }
    }
}

/// Credentials that show up in values whatever the field is called: auth headers, JWTs,
/// well-known key prefixes and long unbroken random-looking strings.
fn looks_secret(v: &str) -> bool {
    let v = v.trim();
    let lower = v.to_ascii_lowercase();
    if lower.starts_with("bearer ") || lower.starts_with("basic ") {
        return true;
    }
    if v.starts_with("eyJ") && v.matches('.').count() == 2 {
        return true; // JWT
    }
    if ["sk-", "sk_", "ghp_", "gho_", "github_pat_", "xox", "AKIA"]
        .iter()
        .any(|p| v.starts_with(p))
    {
        return true;
    }
    // 32+ characters with no spaces, mixing letters and digits: keys, hashes, session ids.
    v.len() >= 32
        && !v.contains(char::is_whitespace)
        && v.bytes().any(|b| b.is_ascii_digit())
        && v.bytes().any(|b| b.is_ascii_alphabetic())
        && !v.contains("://")
        && !v.contains('/')
}

/// Email addresses and IP addresses of readers or other people.
fn looks_personal(v: &str) -> bool {
    v.split_whitespace().any(|w| {
        let w = w.trim_matches(|c: char| !c.is_ascii_alphanumeric());
        let email = w
            .split_once('@')
            .is_some_and(|(user, host)| !user.is_empty() && host.contains('.'));
        let ipv4 = w.split('.').count() == 4 && w.split('.').all(|p| p.parse::<u8>().is_ok());
        let ipv6 = w.matches(':').count() >= 2
            && w.chars().all(|c| c.is_ascii_hexdigit() || c == ':')
            && w.chars().any(|c| c.is_ascii_hexdigit());
        email || ipv4 || ipv6
    })
}

#[cfg(test)]
mod tests {
    use super::{REDACTED, truncate, value};

    #[test]
    fn secret_and_personal_fields_are_redacted() {
        for key in [
            "password",
            "new_password",
            "api_token",
            "Authorization",
            "session_id",
            "setup_code",
            "email",
            "reader_email",
            "ip",
            "client_ip",
        ] {
            assert_eq!(value(key, "anything"), REDACTED, "{key}");
        }
        for v in [
            "Bearer abc.def",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.sig",
            "sk-live-123",
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8",
            "contact me at reader@example.com",
            "from 203.0.113.9",
            "2001:db8::1",
        ] {
            assert_eq!(value("note", v), REDACTED, "{v}");
        }
    }

    #[test]
    fn ordinary_values_are_kept_trimmed_and_capped() {
        assert_eq!(value("slug", "  hello-world  "), "hello-world");
        assert_eq!(
            value("to", "Ti\u{1ebf}ng Vi\u{1ec7}t"),
            "Ti\u{1ebf}ng Vi\u{1ec7}t"
        );
        assert_eq!(
            value(
                "url",
                "https://example.com/posts/2026/a-very-long-slug-indeed"
            ),
            "https://example.com/posts/2026/a-very-long-slug-indeed"
        );
        assert_eq!(value("version", "1.2.3"), "1.2.3");
        assert_eq!(
            value("ip_address_count", "3"),
            REDACTED,
            "conservative on personal names"
        );
        let long = "word ".repeat(100);
        assert_eq!(value("title", &long).chars().count(), 201);
        assert_eq!(
            truncate("\u{1ec7}\u{1ec7}\u{1ec7}", 2),
            "\u{1ec7}\u{1ec7}\u{2026}"
        );
    }
}
