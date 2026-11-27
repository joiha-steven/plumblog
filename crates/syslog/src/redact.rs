//! Redaction: what never reaches the system log (docs/ARCHITECTURE.md, System log: never
//! passwords, tokens or personal data beyond the acting user). Applied when an event is
//! built, so stored rows are already clean.
//!
//! Two layers. Keys: a field whose name says secret or personal is replaced whole, however
//! the name is spelled (`setupCode`, `x-api-key`, `cf-connecting-ip`). Values: inside any
//! other value, credentials, emails and IPs are replaced where they occur, including inside
//! URLs (userinfo, query and fragment parameters by name), `k=v` pairs and JSON (by key, at
//! any depth); the rest of the value is kept so the entry stays useful.

use crate::event::MAX_TEXT;
use plumblog_i18n::{Key, key};

/// Stored in place of redacted text. An internal marker, not UI text: the admin shows
/// [`redacted_label`] wherever it occurs.
pub const REDACTED: &str = "{redacted}";

/// The locale key the admin shows for [`REDACTED`].
#[must_use]
pub fn redacted_label() -> Key {
    key!("syslog.redacted")
}

/// Matched as substrings of the normalised name (lowercase, letters and digits only).
const SECRET_PARTS: &[&str] = &[
    "password",
    "passwd",
    "passphrase",
    "secret",
    "token",
    "apikey",
    "authorization",
    "cookie",
    "session",
    "credential",
    "privatekey",
    "signature",
    "turnstile",
    "invite",
    "setupcode",
    "nonce",
    // Personal data of someone other than the acting user.
    "email",
    "phone",
    "address",
    "useragent",
    "forwarded",
];

/// Matched as whole words of the name (`client_ip`, `clientIp`, `x-api-key`), because as
/// substrings they would hit ordinary names (`recipient`, `keyword`, `barcode`...).
const SECRET_WORDS: &[&str] = &[
    "ip", "otp", "key", "code", "auth", "pass", "pwd", "pin", "mail", "addr", "ssn", "cvv",
];

/// Values under these names are versions or counts, never addresses: no IP guessing.
const VERSION_WORDS: &[&str] = &["version", "ver"];

/// Words of a field name: split on anything not a letter or digit and on lower-to-upper
/// case changes, lowercased.
fn words(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if !c.is_ascii_alphanumeric() {
            prev_lower = false;
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if c.is_ascii_uppercase() && prev_lower && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        prev_lower = c.is_ascii_lowercase() || c.is_ascii_digit();
        cur.push(c.to_ascii_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Whether a field (or URL, form or JSON parameter) of this name holds a secret or someone's
/// personal data. Checked on the full name, before any truncation.
#[must_use]
pub fn secret_name(name: &str) -> bool {
    let words = words(name);
    let joined: String = words.concat();
    SECRET_PARTS.iter().any(|p| joined.contains(p))
        || words.iter().any(|w| SECRET_WORDS.contains(&w.as_str()))
}

/// The stored form of `value` under `key`: [`REDACTED`] when the key names a secret or
/// personal data, else the value with secrets, emails and IPs replaced where they occur,
/// trimmed and capped at [`MAX_TEXT`] characters.
pub fn value(key: &str, value: &str) -> String {
    if secret_name(key) {
        return REDACTED.to_owned();
    }
    let ips = !words(key)
        .iter()
        .any(|w| VERSION_WORDS.contains(&w.as_str()));
    truncate(&scrub(value.trim(), ips), MAX_TEXT)
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

/// Free text: JSON by key, then word by word, keeping the whitespace between words.
fn scrub(v: &str, ips: bool) -> String {
    let lower = v.to_ascii_lowercase();
    if ["bearer ", "basic ", "digest ", "token "]
        .iter()
        .any(|p| lower.starts_with(p))
    {
        return REDACTED.to_owned(); // an Authorization header value
    }
    // `[2001:db8::1]:443` starts with `[` too; JSON arrays worth parsing hold strings.
    if v.starts_with('{') || (v.starts_with('[') && v.contains('"')) {
        return scrub_json(v, ips);
    }
    let mut out = String::with_capacity(v.len());
    let mut word = String::new();
    for c in v.chars() {
        if c.is_whitespace() {
            out.push_str(&scrub_word(&word, ips));
            word.clear();
            out.push(c);
        } else {
            word.push(c);
        }
    }
    out.push_str(&scrub_word(&word, ips));
    out
}

fn scrub_word(w: &str, ips: bool) -> String {
    if w.is_empty() {
        return String::new();
    }
    if w.contains('{') && w.contains(':') && w.contains('"') {
        return scrub_json(w, ips);
    }
    if let Some(scheme_end) = w.find("://") {
        return scrub_url(w, scheme_end, ips);
    }
    // Before parameter parsing: base64 padding is `=` and may contain `/`.
    if looks_secret(w.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '=')) {
        return REDACTED.to_owned();
    }
    if w.contains('?') || w.contains('=') {
        return scrub_url(w, usize::MAX, ips);
    }
    scrub_token(w, ips)
}

/// One piece with no URL or parameter structure left: a credential or email goes whole,
/// IPs are replaced where they occur.
fn scrub_token(w: &str, ips: bool) -> String {
    let bare = w.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    if looks_secret(bare) || looks_email(bare) {
        return w.replace(bare, REDACTED);
    }
    if ips { replace_ips(w) } else { w.to_owned() }
}

/// `scheme://user:pass@host/path?k=v#k=v`. Userinfo goes; path segments get the token check;
/// query and fragment parameters are judged by name, then by value. With `scheme_end` past
/// the end, `w` is a bare path, `?query` or `k=v&k=v`.
fn scrub_url(w: &str, scheme_end: usize, ips: bool) -> String {
    let (head, rest) = if scheme_end < w.len() {
        w.split_at(scheme_end + 3)
    } else {
        ("", w)
    };
    let mut out = head.to_owned();
    let path_end = rest.find(['?', '#']).unwrap_or(rest.len());
    let (mut path, tail) = rest.split_at(path_end);
    if !head.is_empty() {
        let authority_end = path.find('/').unwrap_or(path.len());
        if let Some(at) = path[..authority_end].rfind('@') {
            out.push_str(REDACTED);
            out.push('@');
            path = &path[at + 1..];
        }
    }
    let path = if !path.contains('=') || path.contains('/') {
        path.split('/')
            .map(|s| scrub_token(s, ips))
            .collect::<Vec<_>>()
            .join("/")
    } else {
        scrub_params(path, ips) // `k=v&k=v` with no `?`
    };
    out.push_str(&path);
    let mut tail = tail;
    while let Some(sep) = tail.chars().next() {
        let end = tail[1..].find(['?', '#']).map_or(tail.len(), |i| i + 1);
        out.push(sep);
        out.push_str(&scrub_params(&tail[1..end], ips));
        tail = &tail[end..];
    }
    out
}

fn scrub_params(q: &str, ips: bool) -> String {
    q.split('&')
        .map(|pair| match pair.split_once('=') {
            Some((k, _)) if secret_name(k) => format!("{k}={REDACTED}"),
            Some((k, v)) => format!("{k}={}", scrub_token(v, ips)),
            None => scrub_token(pair, ips),
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// JSON (or JSON-like) text: a value under a secret key goes whole, whatever its shape;
/// other string values are scrubbed as text. Works on malformed input too, so a truncated
/// body cannot slip past.
fn scrub_json(src: &str, ips: bool) -> String {
    let b = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    let mut secret_value = false;
    while i < b.len() {
        match b[i] {
            b'"' => {
                let end = string_end(b, i);
                let closed = end > i + 1 && b[end - 1] == b'"';
                let inner = &src[i + 1..if closed { end - 1 } else { end }];
                let is_key = src[end..].trim_start().starts_with(':');
                if is_key {
                    secret_value = secret_name(inner);
                    out.push_str(&src[i..end]);
                } else {
                    out.push('"');
                    out.push_str(&if secret_value {
                        REDACTED.to_owned()
                    } else {
                        scrub(inner, ips)
                    });
                    if closed {
                        out.push('"');
                    }
                    secret_value = false;
                }
                i = end;
            }
            b'{' | b'[' if secret_value => {
                i = group_end(b, i);
                out.push('"');
                out.push_str(REDACTED);
                out.push('"');
                secret_value = false;
            }
            c if secret_value && !c.is_ascii_whitespace() && c != b':' => {
                // A number, true/false/null: up to the next separator.
                while i < b.len() && !matches!(b[i], b',' | b'}' | b']') {
                    i += 1;
                }
                out.push('"');
                out.push_str(REDACTED);
                out.push('"');
                secret_value = false;
            }
            _ => {
                let c = src[i..].chars().next().unwrap_or(' ');
                out.push(c);
                i += c.len_utf8();
            }
        }
    }
    out
}

/// Index just past the string starting at `start` (a `"`), or the end of input.
fn string_end(b: &[u8], start: usize) -> usize {
    let mut i = start + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            b'"' => return i + 1,
            _ => i += 1,
        }
    }
    b.len()
}

/// Index just past the balanced `{...}` / `[...]` starting at `start`, or the end of input.
fn group_end(b: &[u8], start: usize) -> usize {
    let mut depth = 0usize;
    let mut i = start;
    while i < b.len() {
        match b[i] {
            b'"' => {
                i = string_end(b, i);
                continue;
            }
            b'{' | b'[' => depth += 1,
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return i + 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    b.len()
}

/// Credentials whatever the field is called: JWTs, well-known key prefixes, and long
/// random-looking strings (mixed case with digits, or long hex). Lowercase slugs, dates and
/// words are not random-looking, so they stay.
fn looks_secret(v: &str) -> bool {
    if v.starts_with("eyJ") && v.matches('.').count() == 2 {
        return true; // JWT
    }
    if [
        "sk-",
        "sk_",
        "rk_",
        "ghp_",
        "gho_",
        "ghs_",
        "ghu_",
        "github_pat_",
        "xox",
        "AKIA",
        "ASIA",
    ]
    .iter()
    .any(|p| v.starts_with(p) && v.len() > p.len() + 2)
    {
        return true;
    }
    let token_chars = v
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || b"+/=_-.~".contains(&c));
    let digit = v.bytes().any(|c| c.is_ascii_digit());
    let mixed_case =
        v.bytes().any(|c| c.is_ascii_uppercase()) && v.bytes().any(|c| c.is_ascii_lowercase());
    let hex = v.bytes().all(|c| c.is_ascii_hexdigit());
    token_chars && digit && ((v.len() >= 20 && mixed_case) || (v.len() >= 32 && hex))
}

fn looks_email(v: &str) -> bool {
    v.split_once('@').is_some_and(|(user, host)| {
        !user.is_empty() && host.contains('.') && !host.starts_with('.') && !host.ends_with('.')
    })
}

/// Replaces every IPv4 or IPv6 address in `w` (with or without a port, in lists, after
/// `for=`, IPv4-mapped IPv6...). A candidate must stand alone: `v1.2.3.4` is a version.
fn replace_ips(w: &str) -> String {
    let is_run = |c: char| c.is_ascii_hexdigit() || c == '.' || c == ':';
    let mut out = String::with_capacity(w.len());
    let mut rest = w;
    let mut prev: Option<char> = None;
    while let Some(start) = rest.find(is_run) {
        let before = &rest[..start];
        out.push_str(before);
        let prev_c = before.chars().last().or(prev);
        let len = rest[start..]
            .find(|c| !is_run(c))
            .unwrap_or(rest.len() - start);
        let run = &rest[start..start + len];
        let next_c = rest[start + len..].chars().next();
        let bounded = !prev_c.is_some_and(|c| c.is_ascii_alphanumeric())
            && !next_c.is_some_and(|c| c.is_ascii_alphanumeric());
        if bounded {
            out.push_str(&replace_ips_in_run(run));
        } else {
            out.push_str(run);
        }
        prev = run.chars().last();
        rest = &rest[start + len..];
    }
    out.push_str(rest);
    out
}

fn replace_ips_in_run(run: &str) -> String {
    // `::ffff:1.2.3.4` starts with `::`; a trailing `:` or `.` is punctuation.
    for core in [
        run.trim_matches('.'),
        run.trim_matches(|c| c == '.' || c == ':'),
    ] {
        if is_ipv6(core) {
            return run.replacen(core, REDACTED, 1);
        }
    }
    // IPv4, possibly with a port: `203.0.113.9:443`.
    run.split(':')
        .map(|p| {
            let t = p.trim_matches('.');
            if is_ipv4(t) {
                p.replace(t, REDACTED)
            } else {
                p.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(":")
}

fn is_ipv4(s: &str) -> bool {
    s.split('.').count() == 4
        && s.split('.')
            .all(|p| !p.is_empty() && p.len() <= 3 && p.parse::<u8>().is_ok())
}

/// Compressed (`::`) or full eight-group form, optionally ending in an IPv4 address.
/// Times like `12:30:45` have neither.
fn is_ipv6(s: &str) -> bool {
    let (groups, v4_tail) = match s.rsplit_once(':') {
        Some((head, tail)) if is_ipv4(tail) => (head, true),
        _ => (s, false),
    };
    let compressed = s.contains("::");
    let parts: Vec<&str> = groups.split(':').filter(|g| !g.is_empty()).collect();
    let full = parts.len() + usize::from(v4_tail) * 2 == 8;
    (compressed || full)
        && s.matches("::").count() <= 1
        && parts
            .iter()
            .all(|g| g.len() <= 4 && g.bytes().all(|c| c.is_ascii_hexdigit()))
        && (!parts.is_empty() || v4_tail)
}

#[cfg(test)]
mod tests {
    use super::{REDACTED, secret_name, truncate, value};

    /// The review probes for #35, plus the original cases: every one is redacted.
    #[test]
    fn secret_and_personal_names_in_any_spelling() {
        for key in [
            "password",
            "new_password",
            "api_token",
            "Authorization",
            "session_id",
            "setup_code",
            "setupCode",
            "setup-code",
            "x-api-key",
            "privateKey",
            "invite",
            "inviteToken",
            "clientIp",
            "cf-connecting-ip",
            "x-forwarded-for",
            "X-Real-IP",
            "userEmail",
            "email",
            "reader_email",
            "ip",
            "client_ip",
            "otp",
            "Cookie",
            "user-agent",
        ] {
            assert!(secret_name(key), "{key}");
            assert_eq!(value(key, "K7QX-29PL"), REDACTED, "{key}");
        }
        for key in [
            "slug",
            "title",
            "recipient",
            "keyword",
            "description",
            "status",
            "author",
            "version",
            "from",
            "to",
        ] {
            assert!(!secret_name(key), "{key}");
        }
    }

    #[test]
    fn credentials_inside_values() {
        for v in [
            "Bearer abc.def",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.sig",
            "sk-live-123",
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8",
            "Zm9vYmFy/YmF6cXV4+cXV1eDEyMzQ1Njc4OTA=",
            "dGhpc0lzQVRva2VuMTIz",
        ] {
            assert_eq!(value("note", v), REDACTED, "{v}");
        }
        assert_eq!(
            value("note", "contact me at reader@example.com"),
            format!("contact me at {REDACTED}")
        );
    }

    #[test]
    fn ips_anywhere_in_a_value() {
        let r = REDACTED;
        for (v, want) in [
            ("from 203.0.113.9", format!("from {r}")),
            ("203.0.113.9:443", format!("{r}:443")),
            ("for=203.0.113.9", format!("for={r}")),
            ("1.2.3.4,5.6.7.8", format!("{r},{r}")),
            ("::ffff:203.0.113.9", r.to_owned()),
            ("2001:db8::1", r.to_owned()),
            ("[2001:db8::1]:8443", format!("[{r}]:8443")),
            ("2001:0db8:0000:0000:0000:ff00:0042:8329", r.to_owned()),
        ] {
            assert_eq!(value("note", v), want, "{v}");
        }
    }

    #[test]
    fn nested_values_by_key() {
        let r = REDACTED;
        assert_eq!(
            value("body", r#"{"password":"hunter2","user":"ann"}"#),
            format!(r#"{{"password":"{r}","user":"ann"}}"#)
        );
        assert_eq!(
            value(
                "body",
                r#"{"a":{"apiKey":{"x":[1,2]},"n":[{"otp":123456}],"ip":"1.2.3.4"}}"#
            ),
            format!(r#"{{"a":{{"apiKey":"{r}","n":[{{"otp":"{r}"}}],"ip":"{r}"}}}}"#)
        );
        assert_eq!(
            value("body", r#"{"note":"mail a@b.co","x":"#),
            format!(r#"{{"note":"mail {r}","x":"#)
        );
        assert_eq!(
            value(
                "url",
                "https://example.com/cb?token=abc123&page=2#access_token=xyz"
            ),
            format!("https://example.com/cb?token={r}&page=2#access_token={r}")
        );
        assert_eq!(
            value("url", "https://ann:pw@example.com/a"),
            format!("https://{r}@example.com/a")
        );
        assert_eq!(
            value("note", "posted password=hunter2&lang=vi"),
            format!("posted password={r}&lang=vi")
        );
        assert_eq!(
            value("url", "https://example.com/reset/Q2xhdWRlUm9ja3MxMjM0NTY"),
            format!("https://example.com/reset/{r}")
        );
    }

    #[test]
    fn ordinary_values_are_kept_trimmed_and_capped() {
        for (key, v) in [
            ("slug", "hello-world"),
            ("subject", "2026-11-27-a-very-long-dated-post-slug-here"),
            ("to", "Ti\u{1ebf}ng Vi\u{1ec7}t"),
            (
                "url",
                "https://example.com/posts/2026/a-very-long-slug-indeed?page=2",
            ),
            ("version", "1.2.3.4"),
            ("note", "upgraded to v1.2.3.4"),
            ("at", "12:30:45"),
            ("path", "crate::module::item"),
            ("note", "std::vector and 10:30"),
            ("status", "502"),
        ] {
            assert_eq!(value(key, &format!("  {v}  ")), v, "{key}={v}");
        }
        let long = "word ".repeat(100);
        assert_eq!(value("title", &long).chars().count(), 201);
        assert_eq!(
            truncate("\u{1ec7}\u{1ec7}\u{1ec7}", 2),
            "\u{1ec7}\u{1ec7}\u{2026}"
        );
    }
}
