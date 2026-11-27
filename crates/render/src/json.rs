//! A tiny JSON writer for JSON Feed and JSON-LD. Output is also safe inside an HTML
//! `<script>`: `<`, `>`, `&` and the JS line separators are written as `\u` escapes.

use std::fmt::Write as _;

pub enum J {
    Str(String),
    Int(i64),
    Arr(Vec<J>),
    Obj(Vec<(&'static str, J)>),
}

impl J {
    pub fn s(v: impl Into<String>) -> Self {
        Self::Str(v.into())
    }

    /// An object without the entries whose value is `None`.
    pub fn obj(entries: Vec<(&'static str, Option<J>)>) -> Self {
        Self::Obj(
            entries
                .into_iter()
                .filter_map(|(k, v)| v.map(|v| (k, v)))
                .collect(),
        )
    }

    pub fn write(&self, out: &mut String) {
        match self {
            Self::Str(s) => string(out, s),
            Self::Int(n) => {
                let _ = write!(out, "{n}");
            }
            Self::Arr(items) => {
                out.push('[');
                for (i, v) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    v.write(out);
                }
                out.push(']');
            }
            Self::Obj(entries) => {
                out.push('{');
                for (i, (k, v)) in entries.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    string(out, k);
                    out.push(':');
                    v.write(out);
                }
                out.push('}');
            }
        }
    }
}

impl std::fmt::Display for J {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut s = String::new();
        self.write(&mut s);
        f.write_str(&s)
    }
}

fn string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' | '>' | '&' | '\u{2028}' | '\u{2029}' => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c if u32::from(c) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::J;

    #[test]
    fn escapes_for_json_and_script_tags() {
        let v = J::obj(vec![
            ("a", Some(J::s("x\"</script>\n\u{2028}"))),
            ("skip", None),
            ("n", Some(J::Int(-3))),
            ("l", Some(J::Arr(vec![J::Int(1), J::s("\u{1}")]))),
        ]);
        assert_eq!(
            v.to_string(),
            r#"{"a":"x\"\u003c/script\u003e\n\u2028","n":-3,"l":[1,"\u0001"]}"#
        );
    }
}
