//! `clientDataJSON` checks with a tiny strict JSON reader: one top-level object whose values
//! are strings or booleans (all browsers send today), no duplicate keys, size-capped. Anything
//! else is refused rather than guessed at.

use crate::json::{self, Json};
use crate::{base64url, constant_time_eq};
use plumblog_model::{Error, ErrorCode, Result};

/// Real client data is a few hundred bytes.
pub const MAX_CLIENT_DATA_LEN: usize = 4096;

pub const TYPE_CREATE: &str = "webauthn.create";
pub const TYPE_GET: &str = "webauthn.get";

fn invalid(detail: impl Into<String>) -> Error {
    Error::new(
        ErrorCode::PasskeyInvalid,
        format!("clientDataJSON: {}", detail.into()),
    )
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
    // Same detail, passkey error code: the caller answers every passkey failure alike.
    let fields = json::parse_object(json, MAX_CLIENT_DATA_LEN).map_err(|e| invalid(e.detail))?;
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
