//! TypeScript for the admin, emitted from the types here so the browser code cannot drift
//! from the server. `crates/model/tests/ts.rs` keeps `web/admin/src/api/model.gen.ts` equal to
//! [`admin_types`] (`BLESS=1` rewrites it).

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
    out
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
