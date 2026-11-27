//! TypeScript for the admin, emitted from the types here so the browser code cannot drift
//! from the server. `crates/model/tests/ts.rs` keeps `web/admin/src/api/model.gen.ts` equal to
//! [`admin_types`] (`BLESS=1` rewrites it).

use crate::api::{In, ROUTES, RouteId, Type};
use crate::shape::{CODE_LISTS, Prop, SHAPES, Ty, all_props, code_list};
use crate::{ErrorCode, Permission, Role};
use std::fmt::Write;

/// The generated file, header included.
#[must_use]
pub fn admin_types() -> String {
    let mut out = String::from(
        "// Generated from crates/model by crates/model/tests/ts.rs. Do not edit;\n\
         // run `BLESS=1 cargo test -p plumblog-model --test ts` after changing the Rust types.\n",
    );
    codes(
        &mut out,
        "ErrorCode",
        "ERROR_CODES",
        ErrorCode::ALL.iter().map(|c| c.as_str()),
    );
    codes(
        &mut out,
        "Role",
        "ROLES",
        Role::ALL.iter().map(|r| r.as_str()),
    );
    codes(
        &mut out,
        "Permission",
        "PERMISSIONS",
        Permission::ALL.iter().map(|p| p.as_str()),
    );
    out.push_str("\n/** Which roles hold each permission (`Permission::roles`). */\n");
    out.push_str(
        "export const PERMISSION_ROLES: Readonly<Record<Permission, readonly Role[]>> = {\n",
    );
    for p in Permission::ALL {
        let roles: Vec<String> = p.roles().iter().map(|r| lit(r.as_str())).collect();
        let _ = writeln!(out, "  {}: [{}],", lit(p.as_str()), roles.join(", "));
    }
    out.push_str("};\n");
    for l in CODE_LISTS {
        codes(&mut out, l.ty, l.constant, l.values.iter().copied());
    }
    out.push_str("\n/** Response bodies of `/api/v1`, one interface per shape. */\n");
    for s in SHAPES {
        shape_interface(&mut out, s);
    }
    out.push_str(
        "\n/** Request shapes of the post routes. Optional means the client may omit it. */\n",
    );
    input_interface(&mut out, "PostCreateInput", RouteId::PostCreate, In::Body);
    input_interface(&mut out, "PostUpdateInput", RouteId::PostUpdate, In::Body);
    input_interface(&mut out, "PostListQuery", RouteId::PostList, In::Query);
    shapes_table(&mut out);
    out
}

fn pascal(snake: &str) -> String {
    snake
        .split('_')
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
                .unwrap_or_default()
        })
        .collect()
}

fn prop_ts(p: &Prop) -> String {
    let base = match p.ty {
        Ty::Str => "string".to_owned(),
        Ty::Int => "number".to_owned(),
        Ty::Bool => "boolean".to_owned(),
        Ty::Code(c) => code_list(c).map_or_else(|| "string".to_owned(), |l| l.ty.to_owned()),
        Ty::Shape(s) => pascal(s),
    };
    let mut t = if p.list { format!("{base}[]") } else { base };
    if p.null {
        t.push_str(" | null");
    }
    t
}

fn shape_interface(out: &mut String, s: &crate::shape::Shape) {
    let name = pascal(s.name);
    match s.extends {
        Some(parent) => {
            let _ = writeln!(out, "export interface {name} extends {} {{", pascal(parent));
        }
        None => {
            let _ = writeln!(out, "export interface {name} {{");
        }
    }
    for p in s.props {
        let _ = writeln!(out, "  {}: {};", p.name, prop_ts(p));
    }
    out.push_str("}\n");
}

/// The body or query fields of a route as an interface.
fn input_interface(out: &mut String, name: &str, route: RouteId, at: In) {
    let _ = writeln!(out, "export interface {name} {{");
    for r in ROUTES.iter().filter(|r| r.id == route) {
        for f in r.fields.iter().filter(|f| f.at == at) {
            let ty = match (f.name, f.ty) {
                ("status", Type::Str) => "PostStatus",
                ("kind", Type::Str) => "PostKind",
                (_, Type::Str) => "string",
                (_, Type::Int) => "number",
                (_, Type::Bool) => "boolean",
                (_, Type::IntOrNull) => "number | null",
                (_, Type::IntList) => "number[]",
                (_, Type::Json) => "unknown",
            };
            let q = if f.required { "" } else { "?" };
            let _ = writeln!(out, "  {}{q}: {ty};", f.name);
        }
    }
    out.push_str("}\n");
}

/// The shapes as data, so JS tests can check a JSON body without TypeScript.
fn shapes_table(out: &mut String) {
    out.push_str(
        "\n/**\n * Each shape's props in the order the Worker writes them, inherited ones first.\n \
         * Descriptor: `string`, `integer`, `boolean`, `code:<CONSTANT>` (a list above, e.g.\n \
         * `code:POST_STATUSES`) or `shape:<name>`; then `[]` for a list; then `|null` for\n \
         * nullable (`integer[]|null`).\n */\n",
    );
    out.push_str(
        "export const SHAPES: Readonly<Record<string, Readonly<Record<string, string>>>> = {\n",
    );
    for s in SHAPES {
        let _ = writeln!(out, "  {}: {{", lit(s.name));
        for p in all_props(s) {
            let _ = writeln!(out, "    {}: {},", lit(p.name), lit(&p.descriptor()));
        }
        out.push_str("  },\n");
    }
    out.push_str("};\n");
}

/// A string union type plus the list of its values.
fn codes<'a>(out: &mut String, ty: &str, list: &str, values: impl Iterator<Item = &'a str>) {
    let values: Vec<String> = values.map(lit).collect();
    let _ = write!(out, "\nexport const {list} = [\n");
    for v in &values {
        let _ = writeln!(out, "  {v},");
    }
    let _ = writeln!(
        out,
        "] as const;\nexport type {ty} = (typeof {list})[number];"
    );
}

/// A TypeScript string literal. Codes are ASCII by contract, but escape anyway.
fn lit(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            c if c.is_ascii_graphic() || c == ' ' => out.push(c),
            c => {
                let _ = write!(out, "\\u{{{:x}}}", u32::from(c));
            }
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::lit;

    #[test]
    fn literals_escape() {
        assert_eq!(lit("a\"b\\c\u{e9}\n"), "\"a\\\"b\\\\c\\u{e9}\\u{a}\"");
    }
}
