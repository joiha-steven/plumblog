//! The value cap (`MAX_VALUES`) must never refuse a real request: the largest legitimate body
//! of every `/api/v1` route, built from the route table, stays far under it.

use plumblog_auth::json::{MAX_ITEMS, MAX_VALUES, Value, parse_document};
use plumblog_model::api::{Body, In, ROUTES, Type};

/// The longest list a theme or plugin option may hold (`plumblog_render::options`,
/// `LIST_MAX_ITEMS`): the largest JSON a setting or option value takes in practice.
const OPTION_LIST_ITEMS: usize = 20;

fn values(v: &Value) -> usize {
    match v {
        Value::Arr(items) => items.iter().map(|i| 1 + values(i)).sum(),
        Value::Obj(members) => members.iter().map(|(_, m)| 1 + values(m)).sum(),
        _ => 0,
    }
}

/// Every body field present, lists at their largest.
fn largest_body(fields: &[plumblog_model::api::Field]) -> String {
    let members: Vec<String> = fields
        .iter()
        .filter(|f| f.at == In::Body)
        .map(|f| {
            let v = match f.ty {
                Type::Str => "\"x\"".to_owned(),
                Type::Int | Type::IntOrNull => "1".to_owned(),
                Type::Bool => "true".to_owned(),
                // No tag cap but the container cap: a post may name MAX_ITEMS tags.
                Type::IntList => format!("[{}]", vec!["1"; MAX_ITEMS].join(",")),
                Type::Json => format!("[{}]", vec!["\"x\""; OPTION_LIST_ITEMS].join(",")),
            };
            format!("\"{}\":{v}", f.name)
        })
        .collect();
    format!("{{{}}}", members.join(","))
}

#[test]
fn largest_legitimate_bodies_stay_far_under_the_value_cap() {
    let mut checked = 0;
    for r in ROUTES.iter().filter(|r| r.body == Body::Json) {
        let body = largest_body(r.fields);
        let doc = parse_document(body.as_bytes(), usize::MAX).expect(r.path);
        let n: usize = doc.iter().map(|(_, v)| 1 + values(v)).sum();
        eprintln!("{} {}: {n} values", r.method.as_str(), r.path);
        let has_list = r.fields.iter().any(|f| f.ty == Type::IntList);
        // A full tag list is a quarter of the cap; everything else under 1%.
        let limit = if has_list {
            MAX_VALUES / 4
        } else {
            MAX_VALUES / 100
        };
        assert!(n <= limit, "{} {}: {n} values", r.method.as_str(), r.path);
        checked += 1;
    }
    assert!(checked > 10, "{checked} routes");
}
