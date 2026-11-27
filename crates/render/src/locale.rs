//! Theme locale files: nested TOML tables flattened to `section.key`, and `{name}` placeholders
//! filled from template arguments.

use std::collections::HashMap;

/// Flattens `[post] reading_time = "..."` into `post.reading_time`. Non-string leaves are an
/// error: a number or list where text belongs is a mistake a theme author must see.
pub fn flatten(src: &str) -> Result<HashMap<String, String>, String> {
    let table: toml::Table = toml::from_str(src).map_err(|e| e.to_string())?;
    let mut out = HashMap::new();
    walk("", &table, &mut out)?;
    Ok(out)
}

fn walk(
    prefix: &str,
    table: &toml::Table,
    out: &mut HashMap<String, String>,
) -> Result<(), String> {
    for (k, v) in table {
        let key = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match v {
            toml::Value::String(s) => {
                out.insert(key, s.clone());
            }
            toml::Value::Table(t) => walk(&key, t, out)?,
            _ => return Err(format!("{key}: locale values must be text")),
        }
    }
    Ok(())
}

/// Replaces each `{name}` with its argument. Unknown placeholders are left as written, so a
/// missing argument shows up on the page instead of vanishing.
pub fn fill(text: &str, args: &[(&str, String)]) -> String {
    if !text.contains('{') {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len() + 16);
    let mut rest = text;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let name = &after[..end];
        if let Some((_, v)) = args.iter().find(|(k, _)| *k == name) {
            out.push_str(v);
        } else {
            out.push('{');
            out.push_str(name);
            out.push('}');
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::{fill, flatten};

    #[test]
    fn flattens_sections_and_rejects_non_text() {
        let m = flatten("top = \"a\"\n[post]\nnext = \"Next\"\n[x.y]\nz = \"deep\"\n").unwrap();
        assert_eq!(m["top"], "a");
        assert_eq!(m["post.next"], "Next");
        assert_eq!(m["x.y.z"], "deep");
        assert!(flatten("[post]\nn = 3\n").unwrap_err().contains("post.n"));
    }

    #[test]
    fn fills_known_and_keeps_unknown_placeholders() {
        let args = [("minutes", "4".to_owned())];
        assert_eq!(fill("{minutes} min read", &args), "4 min read");
        assert_eq!(fill("{missing} x", &args), "{missing} x");
        assert_eq!(fill("open { brace", &args), "open { brace");
    }
}
