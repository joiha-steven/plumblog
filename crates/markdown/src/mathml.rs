//! Keeps author text from ever becoming markup in math output.
//!
//! pulldown-latex copies the author's text into `MathML` without escaping
//! (`\operatorname{<img onerror=...>}` emits a live `<img>`). Two walls:
//! 1. `neutralize` rewrites `<` and `>` to `\lt` / `\gt` before parsing (outside text mode,
//!    where pulldown-latex escapes them itself), so every tag in its output is its own.
//! 2. `sanitize` re-serialises that output through `MathML` Core elements and attributes,
//!    escapes every text character, drops `style` (no restyling the page, strict CSP) and
//!    accepts only numeric `em` widths. Anything else makes the caller show the TeX source.

const ELEMENTS: &[&str] = &[
    "math",
    "mi",
    "mn",
    "mo",
    "ms",
    "mtext",
    "mspace",
    "mrow",
    "mfrac",
    "msqrt",
    "mroot",
    "mstyle",
    "merror",
    "mpadded",
    "mphantom",
    "msub",
    "msup",
    "msubsup",
    "munder",
    "mover",
    "munderover",
    "mmultiscripts",
    "mprescripts",
    "none",
    "mtable",
    "mtr",
    "mtd",
    "semantics",
    "annotation",
    "menclose",
];

const ATTRIBUTES: &[&str] = &[
    "class",
    "display",
    "displaystyle",
    "scriptlevel",
    "mathvariant",
    "stretchy",
    "symmetric",
    "largeop",
    "movablelimits",
    "linethickness",
    "maxsize",
    "minsize",
    "width",
    "height",
    "depth",
    "lspace",
    "rspace",
    "accent",
    "accentunder",
    "form",
    "fence",
    "separator",
    "columnalign",
    "rowspacing",
    "columnspacing",
    "voffset",
    "encoding",
    "notation",
];

/// Commands whose argument pulldown-latex treats as text and escapes itself.
const TEXT_COMMANDS: &[&str] = &[
    "text",
    "textrm",
    "textit",
    "textbf",
    "texttt",
    "textsf",
    "textnormal",
    "mbox",
];

/// Commands whose argument pulldown-latex copies into the output as raw text, without
/// interpreting commands (so `\lt` would show as the letters "\lt").
const RAW_TEXT_COMMANDS: &[&str] = &["operatorname", "mathop"];

/// TeX with `<` and `>` turned into `\lt` / `\gt` outside text-mode arguments, and into
/// the `&lt;` / `&gt;` entities inside raw-text arguments. Either way no tag can form.
pub fn neutralize(tex: &str) -> String {
    let mut out = String::with_capacity(tex.len() + 8);
    let mut chars = tex.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        match c {
            '<' => out.push_str("\\lt "),
            '>' => out.push_str("\\gt "),
            '\\' => {
                out.push(c);
                let name_start = i + 1;
                let mut name_end = name_start;
                while let Some(&(j, n)) = chars.peek() {
                    if !n.is_ascii_alphabetic() {
                        break;
                    }
                    out.push(n);
                    name_end = j + 1;
                    chars.next();
                }
                if name_end == name_start {
                    // `\<`, `\{`...: a one-character control symbol, copied as is unless it
                    // is `<`/`>`, which the next iteration rewrites.
                    continue;
                }
                let name = &tex[name_start..name_end];
                let raw = RAW_TEXT_COMMANDS.contains(&name);
                if raw {
                    raw_argument(&mut chars, &mut out);
                } else if TEXT_COMMANDS.contains(&name) {
                    while chars.peek().is_some_and(|&(_, n)| n == ' ') {
                        out.push(' ');
                        chars.next();
                    }
                    if chars.peek().is_some_and(|&(_, n)| n == '{') {
                        copy_group(&mut chars, &mut out, raw);
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out
}

/// The argument of a raw-text command, always written as a braced group. pulldown-latex
/// handles only `\\operatorname{...}`: the star form and a brace-less argument make it
/// copy the rest of the formula as raw text and parse it again. So the `*` (limits placement,
/// which it does not support) is dropped, and a single-token argument (`\\operatorname a`,
/// as in TeX) gets braces.
fn raw_argument(chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>, out: &mut String) {
    if chars.peek().is_some_and(|&(_, n)| n == '*') {
        chars.next();
    }
    while chars.peek().is_some_and(|&(_, n)| n == ' ') {
        chars.next();
    }
    match chars.peek().map(|&(_, n)| n) {
        Some('{') => copy_group(chars, out, true),
        Some('\\') => {
            // A command as the whole argument: pulldown-latex rejects it either way, and the
            // caller then shows the source; copy it so that message names the real input.
            out.push(' ');
        }
        Some(c) => {
            chars.next();
            out.push('{');
            match c {
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                _ => out.push(c),
            }
            out.push('}');
        }
        None => {}
    }
}

/// Copies a balanced `{...}` group (escaped braces do not count). With `raw`, `<` and `>`
/// become entities: pulldown-latex copies raw-text arguments as they are, and the sanitizer
/// keeps entities as text.
fn copy_group(
    chars: &mut std::iter::Peekable<std::str::CharIndices<'_>>,
    out: &mut String,
    raw: bool,
) {
    let mut depth = 0usize;
    while let Some((_, c)) = chars.next() {
        match c {
            '<' if raw => out.push_str("&lt;"),
            '>' if raw => out.push_str("&gt;"),
            _ => out.push(c),
        }
        match c {
            '\\' => {
                if let Some((_, n)) = chars.next() {
                    out.push(n);
                }
            }
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return;
                }
            }
            _ => {}
        }
    }
}

/// Safe copy of `raw`, or `None` if it holds anything outside the allowlist.
pub fn sanitize(raw: &str) -> Option<String> {
    // pulldown-latex writes `\ ` as an escaped entity (`&amp;nbsp;`), which would show the
    // letters "&nbsp;"; a real no-break space is what TeX means.
    let raw = raw.replace("<mtext>&amp;nbsp;</mtext>", "<mtext>\u{a0}</mtext>");
    let mut out = String::with_capacity(raw.len() + raw.len() / 8);
    let mut stack: Vec<&str> = Vec::new();
    let mut rest = raw.as_str();
    while let Some(c) = rest.chars().next() {
        if c == '<'
            && let Some((tag, len)) = parse_tag(rest)
        {
            check(&tag, &mut stack)?;
            write_tag(&mut out, &tag);
            rest = &rest[len..];
            continue;
        }
        if stack.is_empty() {
            // Text outside <math> means the structure was not what pulldown-latex writes.
            return None;
        }
        match c {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '&' if is_entity(rest) => out.push('&'),
            '&' => out.push_str("&amp;"),
            _ => out.push(c),
        }
        rest = &rest[c.len_utf8()..];
    }
    stack.is_empty().then_some(out)
}

struct Tag<'a> {
    name: &'a str,
    closing: bool,
    self_closing: bool,
    attrs: Vec<(&'a str, &'a str)>,
}

/// Parses `<name a="v" ...>`, `</name>`, `<name .../>` or `<name ... />` at the start of
/// `s`; `None` when it is not a well-formed tag (then the `<` is text).
fn parse_tag(s: &str) -> Option<(Tag<'_>, usize)> {
    let bytes = s.as_bytes();
    let mut pos = 1;
    let closing = bytes.get(pos) == Some(&b'/');
    if closing {
        pos += 1;
    }
    let start = pos;
    while bytes.get(pos).is_some_and(u8::is_ascii_lowercase) {
        pos += 1;
    }
    let name = &s[start..pos];
    if name.is_empty() {
        return None;
    }
    let mut tag = Tag {
        name,
        closing,
        self_closing: false,
        attrs: Vec::new(),
    };
    loop {
        let spaced = bytes.get(pos) == Some(&b' ');
        while bytes.get(pos) == Some(&b' ') {
            pos += 1;
        }
        match bytes.get(pos)? {
            b'>' => return Some((tag, pos + 1)),
            b'/' if bytes.get(pos + 1) == Some(&b'>') && !closing => {
                tag.self_closing = true;
                return Some((tag, pos + 2));
            }
            c if c.is_ascii_lowercase() && spaced && !closing => {
                let attr_start = pos;
                while bytes
                    .get(pos)
                    .is_some_and(|c| c.is_ascii_lowercase() || *c == b'-')
                {
                    pos += 1;
                }
                let attr = &s[attr_start..pos];
                if bytes.get(pos) != Some(&b'=') || bytes.get(pos + 1) != Some(&b'"') {
                    return None;
                }
                pos += 2;
                let value_start = pos;
                while bytes
                    .get(pos)
                    .is_some_and(|c| !matches!(c, b'"' | b'<' | b'>'))
                {
                    pos += 1;
                }
                if bytes.get(pos) != Some(&b'"') {
                    return None;
                }
                tag.attrs.push((attr, &s[value_start..pos]));
                pos += 1;
            }
            _ => return None,
        }
    }
}

fn check<'a>(tag: &Tag<'a>, stack: &mut Vec<&'a str>) -> Option<()> {
    if !ELEMENTS.contains(&tag.name) {
        return None;
    }
    // `<math>` opens only at the top level and nothing else does.
    if !tag.closing && (tag.name == "math") != stack.is_empty() {
        return None;
    }
    for (name, value) in &tag.attrs {
        // `style` is dropped on output, never trusted (see module docs).
        if *name != "style" && (!ATTRIBUTES.contains(name) || !safe_value(name, value)) {
            return None;
        }
    }
    if tag.closing {
        (stack.pop()? == tag.name).then_some(())
    } else {
        if !tag.self_closing {
            stack.push(tag.name);
        }
        Some(())
    }
}

/// Writes the tag from its parsed parts, so only checked names and values reach the page.
fn write_tag(out: &mut String, tag: &Tag<'_>) {
    out.push('<');
    if tag.closing {
        out.push('/');
    }
    out.push_str(tag.name);
    for (name, value) in tag.attrs.iter().filter(|(n, _)| *n != "style") {
        out.push(' ');
        out.push_str(name);
        out.push_str("=\"");
        out.push_str(value);
        out.push('"');
    }
    out.push_str(if tag.self_closing { " />" } else { ">" });
}

/// Values pulldown-latex writes are numbers, keywords and `em` lengths. `width` must be a
/// plain number of `em`, so no space can cover the page.
fn safe_value(name: &str, value: &str) -> bool {
    if name == "width" {
        let digits = value.strip_suffix("em").unwrap_or("x");
        let digits = digits.strip_prefix('-').unwrap_or(digits);
        return !digits.is_empty()
            && digits.bytes().all(|c| c.is_ascii_digit() || c == b'.')
            && digits.parse::<f32>().is_ok_and(|w| w <= 4.0);
    }
    value
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || b" .-%".contains(&c))
}

fn is_entity(s: &str) -> bool {
    let Some(end) = s.get(..12).unwrap_or(s).find(';') else {
        return false;
    };
    let body = &s[1..end];
    !body.is_empty()
        && (body.bytes().all(|c| c.is_ascii_alphanumeric())
            || body.strip_prefix('#').is_some_and(|n| {
                !n.is_empty() && n.bytes().all(|c| c.is_ascii_digit())
                    || n.strip_prefix('x')
                        .is_some_and(|h| !h.is_empty() && h.bytes().all(|c| c.is_ascii_hexdigit()))
            }))
}

#[cfg(test)]
mod tests {
    use super::{neutralize, sanitize};

    #[test]
    fn author_angle_brackets_never_reach_the_parser_outside_text() {
        assert_eq!(neutralize("a<b>c"), "a\\lt b\\gt c");
        assert_eq!(neutralize("\\text{a<b}<c"), "\\text{a<b}\\lt c");
        assert_eq!(neutralize("\\text {x{<}y}>"), "\\text {x{<}y}\\gt ");
        assert_eq!(
            neutralize("\\operatorname{<mtext style=x>}"),
            "\\operatorname{&lt;mtext style=x&gt;}"
        );
        assert_eq!(neutralize("\\mathop {a>b}<c"), "\\mathop{a&gt;b}\\lt c");
        assert_eq!(neutralize("\\operatorname*{a<b}"), "\\operatorname{a&lt;b}");
        assert_eq!(neutralize("\\operatorname a<b"), "\\operatorname{a}\\lt b");
        assert_eq!(neutralize("\\operatorname*  <b"), "\\operatorname{&lt;}b");
    }

    #[test]
    fn output_is_rebuilt_from_checked_parts() {
        assert_eq!(
            sanitize("<math><mo><</mo><mi>a</mi></math>").as_deref(),
            Some("<math><mo>&lt;</mo><mi>a</mi></math>")
        );
        // pulldown-latex's own spacing, with and without the space before `/>`.
        assert_eq!(
            sanitize("<math><mspace width=\"0.16666667em\" /><mspace width=\"1em\"/></math>")
                .as_deref(),
            Some("<math><mspace width=\"0.16666667em\" /><mspace width=\"1em\" /></math>")
        );
        // `style` is dropped, not copied; `\!` keeps its negative width.
        assert_eq!(
            sanitize("<math><mspace width=\"-0.16666667em\" style=\"margin-left: -0.16666667em\" /><mrow style=\"color: rgb(255 0 0)\"><mi>x</mi></mrow></math>")
                .as_deref(),
            Some("<math><mspace width=\"-0.16666667em\" /><mrow><mi>x</mi></mrow></math>")
        );
        assert_eq!(
            sanitize("<math><mtext>&amp;nbsp;</mtext></math>").as_deref(),
            Some("<math><mtext>\u{a0}</mtext></math>")
        );
        assert_eq!(sanitize("<math><mi><img src=\"x\"></mi></math>"), None);
        assert_eq!(sanitize("<math><mi>x</mi></math><img src=\"x\">"), None);
        assert_eq!(sanitize("<math><mi>x</math>"), None);
        assert_eq!(sanitize("<math><mi onclick=\"x\">x</mi></math>"), None);
        assert_eq!(sanitize("<math><mspace width=\"999em\" /></math>"), None);
        assert_eq!(sanitize("<math><mspace width=\"100%\" /></math>"), None);
    }
}
