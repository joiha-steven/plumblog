//! Redaction: what never reaches the system log (docs/ARCHITECTURE.md, System log: never
//! passwords, tokens or personal data beyond the acting user). Applied when an event is
//! built, so stored rows are already clean.
//!
//! Two layers. Names: a field whose name says secret or personal is replaced whole, however
//! the name is spelled (`setupCode`, `x-api-key`, `cf-connecting-ip`). Values: inside any
//! other value, credentials, emails and IPs are replaced where they occur, as is whatever
//! follows a secret name inside the value: URL userinfo and parameters, `name: value` and
//! `name=value` text, and JSON (double- or single-quoted, escaped inside a string, at any
//! depth up to [`MAX_DEPTH`]). The rest of the value is kept so the entry stays useful.

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
    "passcode",
    "pincode",
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
    "totp",
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
    "ip", "otp", "key", "code", "auth", "pass", "pwd", "pin", "jwt", "mail", "addr", "ssn", "cvv",
];

/// Values under these names are versions or counts, never addresses: no IP guessing.
const VERSION_WORDS: &[&str] = &["version", "ver"];

/// Authorization schemes: the word after one is the credential.
const AUTH_SCHEMES: &[&str] = &["bearer", "basic", "digest", "token"];

/// Longest input scanned, in characters; the rest is cut first, so a pasted document costs
/// bounded CPU. Far above [`MAX_TEXT`], so what is stored has been fully scanned.
const MAX_SCAN: usize = 4096;

/// Deepest JSON-in-a-JSON-string nesting scrubbed; anything deeper is redacted whole.
pub const MAX_DEPTH: u8 = 4;

/// Which value guessers run. `entropy` (the random-string guesser) is off for subjects;
/// `ips` is off under version-like names.
#[derive(Clone, Copy)]
struct Guess {
    ips: bool,
    entropy: bool,
    depth: u8,
}

impl Guess {
    fn deeper(self) -> Self {
        Self {
            depth: self.depth.saturating_add(1),
            ..self
        }
    }
}

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

/// Whether a field (or URL, form, JSON or `name: value` parameter) of this name holds a
/// secret or someone's personal data. Checked on the full, percent-decoded name.
#[must_use]
pub fn secret_name(name: &str) -> bool {
    let words = words(&percent_decode(name));
    if words.is_empty() {
        return false;
    }
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
    stored(
        value,
        Guess {
            ips,
            entropy: true,
            depth: 0,
        },
    )
}

/// The stored form of a subject (a slug, setting key, job or plugin name chosen by code).
/// Scrubbed like a value, but without the random-string guesser: titles and dated slugs are
/// never secrets, and a false hit would hide what the entry is about.
pub fn subject(subject: &str) -> String {
    stored(
        subject,
        Guess {
            ips: true,
            entropy: false,
            depth: 0,
        },
    )
}

fn stored(v: &str, g: Guess) -> String {
    truncate(&scrub(&truncate(v.trim(), MAX_SCAN), g), MAX_TEXT)
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

/// Free text: JSON by key, then word by word, keeping the whitespace between words. A word
/// after `secret:` / `secret =` (and after an auth scheme there) is redacted.
fn scrub(v: &str, g: Guess) -> String {
    let lower = v.to_ascii_lowercase();
    if AUTH_SCHEMES.iter().any(|p| {
        lower
            .strip_prefix(p)
            .is_some_and(|rest| rest.starts_with(' '))
    }) {
        return REDACTED.to_owned(); // an Authorization header value
    }
    // `[2001:db8::1]:443` starts with `[` too; JSON arrays worth parsing hold strings.
    if v.starts_with('{') || (v.starts_with('[') && (v.contains('"') || v.contains('\''))) {
        return scrub_json(v, g);
    }
    let mut out = String::with_capacity(v.len());
    let mut word = String::new();
    let mut state = Free::default();
    for c in v.chars() {
        if c.is_whitespace() {
            out.push_str(&state.word(&word, g));
            word.clear();
            out.push(c);
        } else {
            word.push(c);
        }
    }
    out.push_str(&state.word(&word, g));
    out
}

/// Word-to-word state of free text: `password: hunter2`, `Authorization: Bearer abc`.
#[derive(Default)]
struct Free {
    /// The next word is a secret value.
    pending: bool,
    /// The last word was a secret name, so a lone `:` or `=` makes the next one secret.
    after_name: bool,
}

impl Free {
    fn word(&mut self, w: &str, g: Guess) -> String {
        if w.is_empty() {
            return String::new();
        }
        if self.pending {
            self.pending = AUTH_SCHEMES.contains(&w.to_ascii_lowercase().as_str());
            self.after_name = false;
            return REDACTED.to_owned();
        }
        if (w == ":" || w == "=") && self.after_name {
            self.pending = true;
            return w.to_owned();
        }
        if let Some(name) = w.strip_suffix([':', '='])
            && secret_name(name.trim_matches(|c: char| !c.is_ascii_alphanumeric()))
        {
            self.pending = true;
            return w.to_owned();
        }
        self.after_name = secret_name(w.trim_matches(|c: char| !c.is_ascii_alphanumeric()));
        scrub_word(w, g)
    }
}

fn scrub_word(w: &str, g: Guess) -> String {
    if let Some(open) = w.find(['{', '[']) {
        let rest = &w[open..];
        if rest.contains(':') && (rest.contains('"') || rest.contains('\'')) {
            // `body={"password":...}`: the prefix is a name or label, the rest is JSON.
            return format!("{}{}", scrub_word(&w[..open], g), scrub_json(rest, g));
        }
    }
    if let Some((name, rest)) = w.split_once(':')
        && !rest.starts_with("//")
        && secret_name(name)
    {
        return format!("{name}:{REDACTED}");
    }
    if let Some(scheme_end) = w.find("://") {
        return scrub_url(w, scheme_end, g);
    }
    // Before parameter parsing: base64 padding is `=` and may contain `/`.
    let bare = w.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '=');
    if looks_secret(bare, g.entropy) {
        return w.replacen(bare, REDACTED, 1);
    }
    if w.contains('?') || w.contains('=') {
        return scrub_url(w, usize::MAX, g);
    }
    scrub_token(w, g)
}

/// One piece with no URL or parameter structure left: a credential or email goes whole
/// (percent-decoded first, so `a%40b.co` counts), IPs are replaced where they occur.
fn scrub_token(w: &str, g: Guess) -> String {
    let bare = w.trim_matches(|c: char| !c.is_ascii_alphanumeric());
    if bare.is_empty() {
        return w.to_owned();
    }
    let decoded = percent_decode(bare);
    if looks_secret(bare, g.entropy) || looks_email(&decoded) {
        return w.replacen(bare, REDACTED, 1);
    }
    if g.ips { replace_ips(w) } else { w.to_owned() }
}

/// `%XX` decoded; anything else, including a malformed escape, kept as is.
fn percent_decode(s: &str) -> String {
    if !s.contains('%') {
        return s.to_owned();
    }
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        let hex = |c: u8| (c as char).to_digit(16);
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(u8::try_from(h * 16 + l).unwrap_or(b'?'));
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `scheme://user:pass@host/path?k=v#k=v`. Userinfo goes; path segments get the token check;
/// query and fragment parameters are judged by name, then by value. With `scheme_end` past
/// the end, `w` is a bare path, `?query` or `k=v&k=v`.
fn scrub_url(w: &str, scheme_end: usize, g: Guess) -> String {
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
            .map(|s| scrub_token(s, g))
            .collect::<Vec<_>>()
            .join("/")
    } else {
        scrub_params(path, g) // `k=v&k=v` with no `?`
    };
    out.push_str(&path);
    let mut tail = tail;
    while let Some(sep) = tail.chars().next() {
        let end = tail[1..].find(['?', '#']).map_or(tail.len(), |i| i + 1);
        out.push(sep);
        out.push_str(&scrub_params(&tail[1..end], g));
        tail = &tail[end..];
    }
    out
}

fn scrub_params(q: &str, g: Guess) -> String {
    q.split('&')
        .map(|pair| match pair.split_once('=') {
            Some((k, _)) if secret_name(k) => format!("{k}={REDACTED}"),
            Some((k, v)) => format!("{k}={}", scrub_token(v, g)),
            None => scrub_token(pair, g),
        })
        .collect::<Vec<_>>()
        .join("&")
}

/// JSON or JSON-like text (single quotes too): a value under a secret key goes whole,
/// whatever its shape; other string values are scrubbed as text, which recurses into JSON
/// held in a string. Works on malformed input, so a truncated body cannot slip past.
fn scrub_json(src: &str, g: Guess) -> String {
    if g.depth > MAX_DEPTH {
        return REDACTED.to_owned();
    }
    // `{\"password\":...}`: JSON escaped inside a string, every quote escaped.
    if src.contains("\\\"") && !has_bare_quote(src) {
        return escape_json(&scrub_json(&unescape_json(src), g.deeper()));
    }
    let b = src.as_bytes();
    let mut out = String::with_capacity(src.len());
    let mut i = 0;
    let mut secret_value = false;
    while i < b.len() {
        match b[i] {
            q @ (b'"' | b'\'') => {
                let end = string_end(b, i, q);
                let closed = end > i + 1 && b[end - 1] == q;
                let inner = &src[i + 1..if closed { end - 1 } else { end }];
                let is_key = src[end..].trim_start().starts_with(':');
                let quote = char::from(q);
                if is_key {
                    secret_value = secret_name(inner);
                    out.push_str(&src[i..end]);
                } else {
                    out.push(quote);
                    out.push_str(&if secret_value {
                        REDACTED.to_owned()
                    } else {
                        scrub(inner, g.deeper())
                    });
                    if closed {
                        out.push(quote);
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

/// A `"` not preceded by a backslash.
fn has_bare_quote(s: &str) -> bool {
    let mut escaped = false;
    for c in s.chars() {
        match c {
            '\\' if !escaped => escaped = true,
            '"' if !escaped => return true,
            _ => escaped = false,
        }
    }
    false
}

fn unescape_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some(e @ ('"' | '\\' | '/')) => out.push(e),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\t', "\\t")
        .replace('\r', "\\r")
}

/// Index just past the string starting at `start` (quote `q`), or the end of input.
fn string_end(b: &[u8], start: usize, q: u8) -> usize {
    let mut i = start + 1;
    while i < b.len() {
        match b[i] {
            b'\\' => i += 2,
            c if c == q => return i + 1,
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
            q @ (b'"' | b'\'') => {
                i = string_end(b, i, q);
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

/// Credentials whatever the field is called: JWTs and well-known key prefixes always; with
/// `entropy`, also long random-looking strings. Random-looking means 32+ hex characters, or
/// 20+ characters with no word separators (`-`, `_`, `.`) mixing upper, lower and digits
/// with a class change at least every second character on average: true of keys, false of
/// titles and filenames like `Hello-World-Recap-2026` or `HelloWorldRecapNov2026`.
fn looks_secret(v: &str, entropy: bool) -> bool {
    if v.starts_with("eyJ") && v.matches('.').count() == 2 {
        return true; // JWT
    }
    if [
        // plumblog's own API tokens (crates/auth api_token::PREFIX).
        "plb_",
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
    if !entropy {
        return false;
    }
    let digit = v.bytes().any(|c| c.is_ascii_digit());
    if v.len() >= 32 && digit && v.bytes().all(|c| c.is_ascii_hexdigit()) {
        return true;
    }
    let token_chars = v
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || b"+/=".contains(&c));
    let class = |c: u8| match c {
        b'a'..=b'z' => 1,
        b'A'..=b'Z' => 2,
        b'0'..=b'9' => 3,
        _ => 0,
    };
    let changes = v
        .as_bytes()
        .windows(2)
        .filter(|w| class(w[0]) != class(w[1]))
        .count();
    let mixed_case =
        v.bytes().any(|c| c.is_ascii_uppercase()) && v.bytes().any(|c| c.is_ascii_lowercase());
    v.len() >= 20 && token_chars && digit && mixed_case && changes * 2 >= v.len()
}

fn looks_email(v: &str) -> bool {
    v.split_once('@').is_some_and(|(user, host)| {
        !user.is_empty() && host.contains('.') && !host.starts_with('.') && !host.ends_with('.')
    })
}

/// Replaces every IPv4 or IPv6 address in `w` (with or without a port, in lists, after
/// `for=` or `label:`, IPv4-mapped IPv6...). A candidate must stand alone: `v1.2.3.4` is a
/// version.
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
        let mut run = &rest[start..start + len];
        let next_c = rest[start + len..].chars().next();
        // `ip:1.2.3.4`, `cafe:1.2.3.4`: the run starts inside a word, so the address can
        // only start after its first `:`.
        if prev_c.is_some_and(|c| c.is_ascii_alphanumeric()) {
            let lead = run.find(':').map_or(run.len(), |k| k + 1);
            out.push_str(&run[..lead]);
            run = &run[lead..];
        }
        if !run.is_empty() && !next_c.is_some_and(|c| c.is_ascii_alphanumeric()) {
            out.push_str(&replace_ips_in_run(run));
        } else {
            out.push_str(run);
        }
        prev = rest[start..start + len].chars().last();
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
    use super::{MAX_DEPTH, REDACTED, secret_name, subject, truncate, value};

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
            "plb_BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc",
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

    /// Second review round for #35: each probe was stored verbatim before.
    #[test]
    fn second_round_probes() {
        let r = REDACTED;
        for (v, want) in [
            // 1. An IP right after `label:`.
            ("ip:203.0.113.9", format!("ip:{r}")),
            ("client_ip:203.0.113.9", format!("client_ip:{r}")),
            ("host:10.0.0.1:8080", format!("host:{r}:8080")),
            ("from:2001:db8::1", format!("from:{r}")),
            // 2. JSON inside a JSON string.
            (
                r#"body={"data":"{\"password\":\"hunter2\"}"}"#,
                format!(r#"body={{"data":"{{\"password\":\"{r}\"}}"}}"#),
            ),
            (
                r#"{"data":"{\"inner\":\"{\\\"token\\\":\\\"abc\\\"}\"}"}"#,
                format!(r#"{{"data":"{{\"inner\":\"{{\\\"token\\\":\\\"{r}\\\"}}\"}}"}}"#),
            ),
            // 3. `name: value` text, and single-quoted JSON.
            ("password: hunter2", format!("password: {r}")),
            (
                "password = hunter2 then more",
                format!("password = {r} then more"),
            ),
            ("password:hunter2", format!("password:{r}")),
            (
                "Authorization: Bearer abc.def",
                format!("Authorization: {r} {r}"),
            ),
            ("{'password':'hunter2'}", format!("{{'password':'{r}'}}")),
            (
                "{'password': 'hunter2', 'lang': 'vi'}",
                format!("{{'password': '{r}', 'lang': 'vi'}}"),
            ),
            // 5. Percent-encoded email.
            ("a%40b.co", r.to_owned()),
            (
                "https://x.example/u?to=a%40b.co&p=2",
                format!("https://x.example/u?to={r}&p=2"),
            ),
        ] {
            assert_eq!(value("note", v), want, "{v}");
        }
        // 4. More secret names.
        for key in [
            "passcode",
            "totp",
            "otp",
            "pin",
            "jwt",
            "nonce",
            "signature",
            "credential",
            "userPinCode",
        ] {
            assert!(secret_name(key), "{key}");
        }
    }

    /// 6. Titles and filenames survive; the guesser never runs on subjects.
    #[test]
    fn titles_and_filenames_are_not_secrets() {
        for v in [
            "Hello-World-Recap-2026",
            "IMG_20261127_Header.jpg",
            "HelloWorldRecapNov2026",
            "2026-11-27-a-very-long-dated-post-slug-here",
        ] {
            assert_eq!(value("file", v), v, "{v}");
        }
        assert_eq!(subject("dGhpc0lzQVRva2VuMTIz"), "dGhpc0lzQVRva2VuMTIz");
        assert_eq!(value("note", "dGhpc0lzQVRva2VuMTIz"), REDACTED);
        // Emails and IPs are still scrubbed in subjects.
        assert_eq!(
            subject("invite for ann@example.com"),
            format!("invite for {REDACTED}")
        );
    }

    /// JSON nested in strings deeper than `MAX_DEPTH` is redacted whole, never stored raw.
    #[test]
    fn deep_nesting_is_bounded() {
        let mut v = r#"{"password":"hunter2"}"#.to_owned();
        for _ in 0..=MAX_DEPTH + 1 {
            v = format!(
                r#"{{"d":"{}"}}"#,
                v.replace('\\', "\\\\").replace('"', "\\\"")
            );
        }
        let out = value("body", &v);
        assert!(!out.contains("hunter2"), "{out}");
    }
}
