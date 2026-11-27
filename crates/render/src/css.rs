//! Theme CSS is inlined into every page (Budgets: <= 15 KB), so comments and layout whitespace
//! are dropped once at theme load. Deliberately conservative: only whitespace that can never
//! carry meaning is removed, so selectors like `a :hover` keep their space.

/// Strips `/* */` comments, collapses whitespace runs to one space and drops spaces around
/// `{`, `}`, `;` and `,`. Quoted strings are copied untouched (`content: "/*"` stays).
pub fn minify(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut chars = src.chars().peekable();
    let mut pending_space = false;
    while let Some(c) = chars.next() {
        match c {
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                for c in chars.by_ref() {
                    if prev == '*' && c == '/' {
                        break;
                    }
                    prev = c;
                }
                pending_space = true;
            }
            '"' | '\'' => {
                flush_space(&mut out, &mut pending_space);
                out.push(c);
                let mut escaped = false;
                for s in chars.by_ref() {
                    out.push(s);
                    if escaped {
                        escaped = false;
                    } else if s == '\\' {
                        escaped = true;
                    } else if s == c {
                        break;
                    }
                }
            }
            c if c.is_whitespace() => pending_space = true,
            '{' | '}' | ';' | ',' => {
                pending_space = false;
                while out.ends_with(' ') {
                    out.pop();
                }
                // `;}` is redundant; dropping it is safe and common.
                if c == '}' && out.ends_with(';') {
                    out.pop();
                }
                out.push(c);
            }
            c => {
                flush_space(&mut out, &mut pending_space);
                out.push(c);
            }
        }
    }
    out
}

fn flush_space(out: &mut String, pending: &mut bool) {
    // After `:` a space never matters (`color: red`, `(min-width: 1px)`); before it, it does
    // (`a :hover`), and that case is not touched.
    if *pending && !out.is_empty() && !out.ends_with(['{', '}', ';', ',', ':', ' ']) {
        out.push(' ');
    }
    *pending = false;
}

#[cfg(test)]
mod tests {
    use super::minify;

    #[test]
    fn keeps_meaning_and_drops_noise() {
        let src = "/* head */\n:root {\n  --a: 1px; /* why */\n}\na :hover , b>c { content: \"/* x */\"; }\n.x::before{content:'a  b';}\n";
        assert_eq!(
            minify(src),
            ":root{--a:1px}a :hover,b>c{content:\"/* x */\"}.x::before{content:'a  b'}"
        );
    }

    #[test]
    fn media_queries_keep_required_spaces() {
        assert_eq!(
            minify("@media (min-width: 64rem) and (hover: hover) {\n  .a { b: c }\n}"),
            "@media (min-width:64rem) and (hover:hover){.a{b:c}}"
        );
    }
}
