//! `clientDataJSON` checks with a tiny strict JSON reader: one top-level object whose values
//! are strings or booleans (all browsers send today), no duplicate keys, size-capped. Anything
//! else is refused rather than guessed at.

use crate::{base64url, constant_time_eq};
use plumblog_model::{Error, ErrorCode, Result};

/// Real client data is a few hundred bytes.
pub const MAX_CLIENT_DATA_LEN: usize = 4096;

pub const TYPE_CREATE: &str = "webauthn.create";
pub const TYPE_GET: &str = "webauthn.get";

#[derive(Debug, PartialEq, Eq)]
enum Json {
    Str(String),
    Bool(bool),
}

fn invalid(detail: impl Into<String>) -> Error {
    Error::new(
        ErrorCode::PasskeyInvalid,
        format!("clientDataJSON: {}", detail.into()),
    )
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
}

impl Parser<'_> {
    fn skip_ws(&mut self) {
        while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.input.get(self.pos) {
            self.pos += 1;
        }
    }

    fn next(&mut self) -> Result<u8> {
        let b = *self
            .input
            .get(self.pos)
            .ok_or_else(|| invalid("unexpected end"))?;
        self.pos += 1;
        Ok(b)
    }

    fn expect(&mut self, want: u8) -> Result<()> {
        self.skip_ws();
        if self.next()? == want {
            Ok(())
        } else {
            Err(invalid(format!(
                "expected '{}' at offset {}",
                char::from(want),
                self.pos - 1
            )))
        }
    }

    fn hex4(&mut self) -> Result<u32> {
        let mut v = 0u32;
        for _ in 0..4 {
            let d = char::from(self.next()?)
                .to_digit(16)
                .ok_or_else(|| invalid("bad \\u escape"))?;
            v = (v << 4) | d;
        }
        Ok(v)
    }

    /// A string; the opening quote is already consumed. The input is known to be UTF-8, so
    /// copying non-escape bytes through keeps it UTF-8.
    fn string(&mut self) -> Result<String> {
        let mut out = Vec::new();
        loop {
            match self.next()? {
                b'"' => break,
                b'\\' => {
                    let c = match self.next()? {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hi = self.hex4()?;
                            let code = if (0xd800..0xdc00).contains(&hi) {
                                if self.next()? != b'\\' || self.next()? != b'u' {
                                    return Err(invalid("lone high surrogate"));
                                }
                                let lo = self.hex4()?;
                                if !(0xdc00..0xe000).contains(&lo) {
                                    return Err(invalid("bad low surrogate"));
                                }
                                0x10000 + ((hi - 0xd800) << 10) + (lo - 0xdc00)
                            } else {
                                hi
                            };
                            char::from_u32(code).ok_or_else(|| invalid("lone low surrogate"))?
                        }
                        _ => return Err(invalid("bad escape")),
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                }
                b if b < 0x20 => return Err(invalid("control character in string")),
                b => out.push(b),
            }
        }
        String::from_utf8(out).map_err(|_| invalid("not UTF-8"))
    }

    fn literal(&mut self, word: &[u8]) -> Result<()> {
        if self.input.get(self.pos..self.pos + word.len()) == Some(word) {
            self.pos += word.len();
            Ok(())
        } else {
            Err(invalid("bad literal"))
        }
    }

    fn value(&mut self) -> Result<Json> {
        self.skip_ws();
        match self.input.get(self.pos) {
            Some(b'"') => {
                self.pos += 1;
                self.string().map(Json::Str)
            }
            Some(b't') => self.literal(b"true").map(|()| Json::Bool(true)),
            Some(b'f') => self.literal(b"false").map(|()| Json::Bool(false)),
            _ => Err(invalid(format!(
                "only string and boolean values are accepted (offset {})",
                self.pos
            ))),
        }
    }

    fn object(mut self) -> Result<Vec<(String, Json)>> {
        let mut fields: Vec<(String, Json)> = Vec::new();
        self.expect(b'{')?;
        self.skip_ws();
        if self.input.get(self.pos) == Some(&b'}') {
            self.pos += 1;
        } else {
            loop {
                self.expect(b'"')?;
                let key = self.string()?;
                if fields.iter().any(|(k, _)| *k == key) {
                    return Err(invalid(format!("duplicate key {key:?}")));
                }
                self.expect(b':')?;
                let value = self.value()?;
                fields.push((key, value));
                self.skip_ws();
                match self.next()? {
                    b',' => {}
                    b'}' => break,
                    _ => return Err(invalid("expected ',' or '}'")),
                }
            }
        }
        self.skip_ws();
        if self.pos != self.input.len() {
            return Err(invalid("trailing data"));
        }
        Ok(fields)
    }
}

/// Checks `type`, `challenge`, `origin` and `crossOrigin` of a `clientDataJSON`.
///
/// # Errors
///
/// [`ErrorCode::PasskeyInvalid`] when it is not strict JSON of the accepted shape, too long, or
/// any check fails.
pub fn check(
    json: &[u8],
    expected_type: &str,
    expected_challenge: &[u8],
    expected_origin: &str,
) -> Result<()> {
    if json.len() > MAX_CLIENT_DATA_LEN {
        return Err(invalid(format!("{} bytes is too long", json.len())));
    }
    std::str::from_utf8(json).map_err(|_| invalid("not UTF-8"))?;
    let fields = Parser {
        input: json,
        pos: 0,
    }
    .object()?;
    let get = |key: &str| fields.iter().find(|(k, _)| k == key).map(|(_, v)| v);
    let string = |key: &str| match get(key) {
        Some(Json::Str(s)) => Ok(s.as_str()),
        _ => Err(invalid(format!("{key} missing or not a string"))),
    };
    let ty = string("type")?;
    if ty != expected_type {
        return Err(invalid(format!("type {ty:?}, want {expected_type:?}")));
    }
    let challenge = base64url::decode(string("challenge")?)
        .ok_or_else(|| invalid("challenge is not base64url"))?;
    if !constant_time_eq(&challenge, expected_challenge) {
        return Err(invalid("challenge does not match"));
    }
    let origin = string("origin")?;
    if origin != expected_origin {
        return Err(invalid(format!(
            "origin {origin:?}, want {expected_origin:?}"
        )));
    }
    match get("crossOrigin") {
        None | Some(Json::Bool(false)) => Ok(()),
        Some(Json::Bool(true)) => Err(invalid("crossOrigin is true")),
        Some(Json::Str(_)) => Err(invalid("crossOrigin is not a boolean")),
    }
}

#[cfg(test)]
mod tests {
    use super::{TYPE_CREATE, TYPE_GET, check};
    use crate::testutil::Prng;
    use plumblog_model::ErrorCode;

    const ORIGIN: &str = "https://blog.example";
    const CHALLENGE: [u8; 4] = [1, 2, 3, 4];
    // base64url of CHALLENGE
    const CH: &str = "AQIDBA";

    fn ok(json: &str) -> bool {
        check(json.as_bytes(), TYPE_GET, &CHALLENGE, ORIGIN).is_ok()
    }

    #[test]
    fn accepts_browser_shapes() {
        assert!(ok(&format!(
            r#"{{"type":"webauthn.get","challenge":"{CH}","origin":"{ORIGIN}","crossOrigin":false}}"#
        )));
        // Chrome adds a random extra key so nobody compares against a template.
        assert!(ok(&format!(
            r#" {{ "type" : "webauthn.get" , "challenge":"{CH}","origin":"https:\/\/blog.example","other_keys_can_be_added_here":"do not compare clientDataJSON against a template. See https://goo.gl/yabPex"}} "#
        )));
        // Escapes decode before comparing.
        assert!(ok(&format!(
            r#"{{"type":"webauthn\u002eget","challenge":"{CH}","origin":"{ORIGIN}","x":"\ud83d\ude00 \u00e9 \n"}}"#
        )));
        assert!(
            check(
                format!(r#"{{"type":"webauthn.create","challenge":"{CH}","origin":"{ORIGIN}"}}"#)
                    .as_bytes(),
                TYPE_CREATE,
                &CHALLENGE,
                ORIGIN
            )
            .is_ok()
        );
    }

    #[test]
    fn refuses_failed_checks_and_odd_json() {
        let body = |rest: &str| {
            format!(r#"{{"type":"webauthn.get","challenge":"{CH}","origin":"{ORIGIN}"{rest}}}"#)
        };
        let cases: Vec<(&str, String)> = vec![
            (
                "create for get",
                format!(r#"{{"type":"webauthn.create","challenge":"{CH}","origin":"{ORIGIN}"}}"#),
            ),
            (
                "wrong challenge",
                format!(r#"{{"type":"webauthn.get","challenge":"AQIDBQ","origin":"{ORIGIN}"}}"#),
            ),
            (
                "shorter challenge",
                format!(r#"{{"type":"webauthn.get","challenge":"AQID","origin":"{ORIGIN}"}}"#),
            ),
            (
                "padded challenge",
                format!(r#"{{"type":"webauthn.get","challenge":"AQIDBA==","origin":"{ORIGIN}"}}"#),
            ),
            (
                "base64 not url",
                format!(r#"{{"type":"webauthn.get","challenge":"AQ+DBA","origin":"{ORIGIN}"}}"#),
            ),
            (
                "other origin",
                format!(
                    r#"{{"type":"webauthn.get","challenge":"{CH}","origin":"https://evil.example"}}"#
                ),
            ),
            (
                "origin with slash",
                format!(r#"{{"type":"webauthn.get","challenge":"{CH}","origin":"{ORIGIN}/"}}"#),
            ),
            (
                "http origin",
                format!(
                    r#"{{"type":"webauthn.get","challenge":"{CH}","origin":"http://blog.example"}}"#
                ),
            ),
            (
                "no origin",
                format!(r#"{{"type":"webauthn.get","challenge":"{CH}"}}"#),
            ),
            (
                "no type",
                format!(r#"{{"challenge":"{CH}","origin":"{ORIGIN}"}}"#),
            ),
            ("cross origin", body(r#","crossOrigin":true"#)),
            ("cross origin string", body(r#","crossOrigin":"false""#)),
            ("dup type", body(r#","type":"webauthn.get""#)),
            ("dup other key", body(r#","a":"1","a":"2""#)),
            ("number", body(r#","n":1"#)),
            ("null", body(r#","n":null"#)),
            ("object", body(r#","tokenBinding":{"status":"present"}"#)),
            ("array", body(r#","a":[]"#)),
            ("trailing comma", body(",")),
            ("trailing data", format!("{}x", body(""))),
            ("two objects", format!("{}{{}}", body(""))),
            ("raw control char", body(",\"a\":\"\u{1}\"")),
            ("bad escape", body(r#","a":"\x""#)),
            ("lone high surrogate", body(r#","a":"\ud83d""#)),
            ("lone low surrogate", body(r#","a":"\ude00""#)),
            ("bad pair", body(r#","a":"\ud83d\u0041""#)),
            ("truncated", body("")[..20].to_string()),
            ("top-level array", "[]".to_string()),
            ("empty", String::new()),
            ("empty object", "{}".to_string()),
            (
                "too long",
                body(&format!(r#","pad":"{}""#, "a".repeat(5000))),
            ),
            ("truE", body(r#","a":truE"#)),
        ];
        for (name, json) in cases {
            let err = check(json.as_bytes(), TYPE_GET, &CHALLENGE, ORIGIN).unwrap_err();
            assert_eq!(err.code, ErrorCode::PasskeyInvalid, "{name}");
        }
        assert!(check(&[0x7b, 0xff, 0x7d], TYPE_GET, &CHALLENGE, ORIGIN).is_err());
    }

    #[test]
    fn truncations_and_random_mutations_never_panic() {
        let good = format!(
            r#"{{"type":"webauthn.get","challenge":"{CH}","origin":"{ORIGIN}","x":"\u00e9\ud83d\ude00","crossOrigin":false}}"#
        );
        let bytes = good.as_bytes();
        assert!(check(bytes, TYPE_GET, &CHALLENGE, ORIGIN).is_ok());
        for cut in 0..bytes.len() {
            assert!(check(&bytes[..cut], TYPE_GET, &CHALLENGE, ORIGIN).is_err());
        }
        let mut rng = Prng(7);
        for _ in 0..10_000 {
            let mut m = bytes.to_vec();
            for _ in 0..=rng.below(3) {
                let at = rng.below(m.len());
                m[at] = rng.bytes(1)[0];
            }
            let _ = check(&m, TYPE_GET, &CHALLENGE, ORIGIN);
        }
    }
}
