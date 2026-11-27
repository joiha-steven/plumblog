//! A request's path, query and body values, checked against its route's fields before any
//! handler runs: unknown, duplicate, missing and mistyped fields are 400 naming the field.
//! Handlers then read typed values that cannot fail.

use plumblog_auth::json::{self, Value};
use plumblog_model::api::{Field, In, PathValues, Route, Type};
use plumblog_model::{Error, ErrorCode, Result};

/// A nullable integer field of a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Nullable {
    /// Not sent: keep what is stored.
    Absent,
    /// `null`: clear it.
    Null,
    Value(i64),
}

impl Nullable {
    /// The value to store, given the stored one.
    #[must_use]
    pub fn or(self, stored: Option<i64>) -> Option<i64> {
        match self {
            Self::Absent => stored,
            Self::Null => None,
            Self::Value(n) => Some(n),
        }
    }
}

pub struct Input {
    route: &'static Route,
    values: Vec<(&'static str, Value)>,
}

fn invalid(field: &str, why: &str) -> Error {
    Error::new(ErrorCode::InvalidInput, format!("{field}: {why}")).at(field)
}

/// A decimal integer in text (path and query): the JSON integer rules, nothing else.
fn int_text(s: &str) -> Option<i64> {
    let digits = s.strip_prefix('-').unwrap_or(s);
    let ok = !digits.is_empty()
        && digits.len() <= 19
        && digits.bytes().all(|b| b.is_ascii_digit())
        && !(digits.len() > 1 && digits.starts_with('0'));
    if ok { s.parse().ok() } else { None }
}

fn type_ok(ty: Type, v: &Value) -> bool {
    match (ty, v) {
        (Type::Str, Value::Str(_))
        | (Type::Int | Type::IntOrNull, Value::Int(_))
        | (Type::IntOrNull, Value::Null)
        | (Type::Bool, Value::Bool(_)) => true,
        (Type::IntList, Value::Arr(items)) => items.iter().all(|i| matches!(i, Value::Int(_))),
        _ => false,
    }
}

impl Input {
    /// # Errors
    ///
    /// `NotFound` for a path id that is not a positive integer (the resource cannot exist);
    /// `InvalidInput` naming the field for any other mismatch with the route's fields.
    pub fn parse(
        route: &'static Route,
        path: &PathValues<'_>,
        query: &[(String, String)],
        body: &[u8],
    ) -> Result<Self> {
        let field = |name: &str, at: In| -> Option<&'static Field> {
            route.fields.iter().find(|f| f.name == name && f.at == at)
        };
        let mut values: Vec<(&'static str, Value)> = Vec::new();
        for (name, raw) in path {
            let id = int_text(raw).filter(|n| *n > 0).ok_or_else(|| {
                Error::new(ErrorCode::NotFound, format!("{name}: {raw:?} is not an id"))
            })?;
            values.push((name, Value::Int(id)));
        }
        for (name, raw) in query {
            let f = field(name, In::Query).ok_or_else(|| invalid(name, "unknown field"))?;
            if values.iter().any(|(n, _)| *n == f.name) {
                return Err(invalid(f.name, "given twice"));
            }
            let v = match f.ty {
                Type::Str => Value::Str(raw.clone()),
                Type::Int => Value::Int(int_text(raw).ok_or_else(|| invalid(f.name, "integer"))?),
                Type::Bool => match raw.as_str() {
                    "true" => Value::Bool(true),
                    "false" => Value::Bool(false),
                    _ => return Err(invalid(f.name, "boolean")),
                },
                Type::IntOrNull | Type::IntList => return Err(invalid(f.name, "not a query type")),
            };
            values.push((f.name, v));
        }
        let has_body_fields = route.fields.iter().any(|f| f.at == In::Body);
        if !body.is_empty() || has_body_fields {
            // A body-less write (trash, restore) still accepts `{}`.
            let members = json::parse_document(body, route.max_body)?;
            for (name, v) in members {
                let f = field(&name, In::Body).ok_or_else(|| invalid(&name, "unknown field"))?;
                if !type_ok(f.ty, &v) {
                    return Err(invalid(f.name, f.ty.as_str()));
                }
                values.push((f.name, v));
            }
        }
        for f in route.fields.iter().filter(|f| f.required) {
            if !values.iter().any(|(n, _)| *n == f.name) {
                return Err(invalid(f.name, "required"));
            }
        }
        Ok(Self { route, values })
    }

    fn get(&self, name: &str) -> Option<&Value> {
        debug_assert!(
            self.route.fields.iter().any(|f| f.name == name),
            "{name} is not a field of {}",
            self.route.path
        );
        self.values.iter().find(|(n, _)| *n == name).map(|(_, v)| v)
    }

    #[must_use]
    pub fn has(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    #[must_use]
    pub fn str(&self, name: &str) -> Option<&str> {
        match self.get(name) {
            Some(Value::Str(s)) => Some(s),
            _ => None,
        }
    }

    #[must_use]
    pub fn int(&self, name: &str) -> Option<i64> {
        match self.get(name) {
            Some(Value::Int(n)) => Some(*n),
            _ => None,
        }
    }

    #[must_use]
    pub fn bool(&self, name: &str) -> Option<bool> {
        match self.get(name) {
            Some(Value::Bool(b)) => Some(*b),
            _ => None,
        }
    }

    #[must_use]
    pub fn int_or_null(&self, name: &str) -> Nullable {
        match self.get(name) {
            Some(Value::Int(n)) => Nullable::Value(*n),
            Some(Value::Null) => Nullable::Null,
            _ => Nullable::Absent,
        }
    }

    #[must_use]
    pub fn ints(&self, name: &str) -> Option<Vec<i64>> {
        match self.get(name) {
            Some(Value::Arr(items)) => Some(
                items
                    .iter()
                    .filter_map(|v| match v {
                        Value::Int(n) => Some(*n),
                        _ => None,
                    })
                    .collect(),
            ),
            _ => None,
        }
    }

    /// The route's path id (validated positive by [`Input::parse`]).
    #[must_use]
    pub fn id(&self) -> i64 {
        self.int("id").unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::{Input, Nullable};
    use plumblog_model::ErrorCode;
    use plumblog_model::api::{Method, find};

    fn parse(
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: &str,
    ) -> Result<Input, (ErrorCode, Option<String>)> {
        let (route, values) = find(method, path).unwrap();
        let q: Vec<(String, String)> = query
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect();
        Input::parse(route, &values, &q, body.as_bytes()).map_err(|e| (e.code, e.field))
    }

    fn field(r: Result<Input, (ErrorCode, Option<String>)>) -> Option<String> {
        let (code, field) = r.err().expect("refused");
        assert_eq!(code, ErrorCode::InvalidInput);
        field
    }

    #[test]
    fn body_fields_are_checked_against_the_table() {
        let ok = parse(
            Method::Post,
            "/api/v1/posts",
            &[],
            r#"{"lang":"en","slug":"a","title":"T","markdown":"x","tags":[1,2],"series_id":null,"pinned":true}"#,
        )
        .unwrap();
        assert_eq!(ok.ints("tags"), Some(vec![1, 2]));
        assert_eq!(ok.int_or_null("series_id"), Nullable::Null);
        assert_eq!(ok.int_or_null("cover_media_id"), Nullable::Absent);
        assert_eq!(ok.bool("pinned"), Some(true));
        let post = |b: &str| parse(Method::Post, "/api/v1/posts", &[], b);
        let base = r#""lang":"en","slug":"a","title":"T","markdown":"x""#;
        for (extra, want) in [
            (r#","zzz":1"#, "zzz"),
            (r#","tags":[1,"2"]"#, "tags"),
            (r#","tags":[1.5]"#, "tags"),
            (r#","pinned":"yes""#, "pinned"),
            (r#","published_at":1e3"#, "published_at"),
            (r#","published_at":99999999999999999999"#, "published_at"),
            (r#","title":"U""#, "title"),
            (r#","tags":[[[[[[[[[[[[[[[[[[1]]]]]]]]]]]]]]]]]]"#, "tags"),
        ] {
            assert_eq!(
                field(post(&format!("{{{base}{extra}}}"))).as_deref(),
                Some(want),
                "{extra}"
            );
        }
        assert_eq!(
            field(post(r#"{"lang":"en","slug":"a","title":"T"}"#)).as_deref(),
            Some("markdown")
        );
        assert_eq!(field(post("")), None);
        assert_eq!(field(post("[]")), None);
    }

    #[test]
    fn query_and_path() {
        let q = |pairs: &[(&str, &str)]| parse(Method::Get, "/api/v1/posts", pairs, "");
        assert_eq!(
            q(&[("limit", "5"), ("tag", "3")]).unwrap().int("limit"),
            Some(5)
        );
        assert_eq!(field(q(&[("nope", "1")])).as_deref(), Some("nope"));
        assert_eq!(field(q(&[("limit", "05")])).as_deref(), Some("limit"));
        assert_eq!(field(q(&[("limit", "1.0")])).as_deref(), Some("limit"));
        assert_eq!(
            field(q(&[("tag", "1"), ("tag", "2")])).as_deref(),
            Some("tag")
        );
        // GET takes no body fields, but a body is refused only if it carries any.
        assert!(parse(Method::Get, "/api/v1/posts/7", &[], "").is_ok());
        for bad in ["0", "-1", "x", "01", "99999999999999999999"] {
            let r = parse(Method::Get, &format!("/api/v1/posts/{bad}"), &[], "");
            assert_eq!(r.err().unwrap().0, ErrorCode::NotFound, "{bad}");
        }
        assert!(parse(Method::Post, "/api/v1/posts/7/trash", &[], "").is_ok());
        assert!(parse(Method::Post, "/api/v1/posts/7/trash", &[], "{}").is_ok());
        assert_eq!(
            field(parse(
                Method::Post,
                "/api/v1/posts/7/trash",
                &[],
                r#"{"id":1}"#
            ))
            .as_deref(),
            Some("id")
        );
    }
}
