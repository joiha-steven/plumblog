//! Request bodies in and JSON out, without serde: API bodies are flat objects of strings
//! (`plumblog_auth::json`), and responses are small objects written here.

use std::fmt::Write as _;

use plumblog_auth::json::{self, Json};
use plumblog_model::{Error, ErrorCode, Result};

/// Largest accepted request body outside post routes (the route table's default).
pub const MAX_BODY: usize = plumblog_model::api::MAX_BODY;

/// The fields of a request body. Unknown keys are refused, so a client typo fails loudly
/// instead of being silently ignored.
pub struct Fields(Vec<(String, Json)>);

fn invalid(detail: impl Into<String>) -> Error {
    Error::new(ErrorCode::InvalidInput, detail)
}

impl Fields {
    /// # Errors
    ///
    /// `InvalidInput` when the body is not a flat JSON object of strings and booleans, or
    /// has a key outside `allowed`.
    pub fn parse(body: &[u8], allowed: &[&str]) -> Result<Self> {
        let fields = json::parse_object(body, MAX_BODY)?;
        if let Some((k, _)) = fields.iter().find(|(k, _)| !allowed.contains(&k.as_str())) {
            return Err(invalid(format!("unknown field {k:?}")));
        }
        Ok(Self(fields))
    }

    /// # Errors
    ///
    /// `InvalidInput` when the field is a boolean.
    pub fn opt(&self, key: &str) -> Result<Option<&str>> {
        match self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v) {
            None => Ok(None),
            Some(Json::Str(s)) => Ok(Some(s.as_str())),
            Some(Json::Bool(_)) => Err(invalid(format!("{key} must be a string"))),
        }
    }

    /// # Errors
    ///
    /// `InvalidInput` when the field is missing or not a string.
    pub fn req(&self, key: &str) -> Result<&str> {
        self.opt(key)?
            .ok_or_else(|| invalid(format!("{key} is required")))
    }
}

/// The JSON value of a settings row (`value_json`) when it is a string; `None` otherwise.
#[must_use]
pub fn json_string(value_json: &str) -> Option<String> {
    // Wrapping the value lets the strict object reader check it is exactly one string.
    let wrapped = format!("{{\"v\":{value_json}}}");
    let mut fields = json::parse_object(wrapped.as_bytes(), MAX_BODY).ok()?;
    // More than one field means the value smuggled in extra keys.
    if fields.len() != 1 {
        return None;
    }
    match fields.pop()? {
        (_, Json::Str(s)) => Some(s),
        (_, Json::Bool(_)) => None,
    }
}

/// A JSON string literal.
#[must_use]
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // U+2028/2029 are valid JSON but break JS that embeds it; escape them too.
            c if u32::from(c) < 0x20 || c == '\u{2028}' || c == '\u{2029}' => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A JSON value to write.
pub enum Out {
    Str(String),
    Int(i64),
    Bool(bool),
    Null,
    /// Already-encoded JSON (a nested object from [`object`]).
    Raw(String),
}

/// `{"k":v,...}` in the order given.
#[must_use]
pub fn object(fields: &[(&str, Out)]) -> String {
    let parts: Vec<String> = fields
        .iter()
        .map(|(k, v)| {
            let v = match v {
                Out::Str(s) => quote(s),
                Out::Int(n) => n.to_string(),
                Out::Bool(b) => b.to_string(),
                Out::Null => "null".to_owned(),
                Out::Raw(r) => r.clone(),
            };
            format!("{}:{v}", quote(k))
        })
        .collect();
    format!("{{{}}}", parts.join(","))
}

#[cfg(test)]
mod tests {
    use super::{Fields, Out, json_string, object, quote};
    use plumblog_model::ErrorCode;

    #[test]
    fn fields_refuse_unknown_keys_and_wrong_types() {
        let f = Fields::parse(br#"{"a":"x","b":true}"#, &["a", "b"]).unwrap();
        assert_eq!(f.req("a").unwrap(), "x");
        assert_eq!(f.opt("c").unwrap(), None);
        assert_eq!(f.req("b").unwrap_err().code, ErrorCode::InvalidInput);
        assert_eq!(f.req("c").unwrap_err().code, ErrorCode::InvalidInput);
        let e = Fields::parse(br#"{"a":"x","z":"y"}"#, &["a"])
            .err()
            .unwrap();
        assert_eq!(e.code, ErrorCode::InvalidInput);
        for bad in [
            &b""[..],
            b"[]",
            b"{\"a\":1}",
            b"{\"a\":\"x\",\"a\":\"y\"}",
            b"{} x",
        ] {
            assert!(Fields::parse(bad, &["a"]).is_err(), "{bad:?}");
        }
        assert!(Fields::parse(&vec![b' '; super::MAX_BODY + 1], &[]).is_err());
    }

    #[test]
    fn json_round_trip() {
        assert_eq!(json_string("\"abc\""), Some("abc".into()));
        assert_eq!(json_string("1"), None);
        assert_eq!(json_string("\"a\"}"), None);
        assert_eq!(json_string("\"a\",\"w\":\"b\""), None);
        assert_eq!(
            quote("a\"\\\n\u{1}\u{2028}"),
            "\"a\\\"\\\\\\n\\u0001\\u2028\""
        );
        let o = object(&[
            ("s", Out::Str("x".into())),
            ("n", Out::Int(-3)),
            ("z", Out::Null),
            ("r", Out::Raw("{}".into())),
        ]);
        assert_eq!(o, r#"{"s":"x","n":-3,"z":null,"r":{}}"#);
    }
}
