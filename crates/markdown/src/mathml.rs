//! Re-serialises pulldown-latex output through an allowlist. pulldown-latex copies the
//! author's text into `MathML` without escaping (`\operatorname{<img onerror=...>}` emits a
//! live `<img>`), so its output is untrusted: every tag is checked against `MathML` Core
//! elements and attributes, every text character is escaped, and anything that does not
//! fit makes the caller fall back to showing the TeX source.

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
    "style",
    "notation",
];

/// Safe copy of `raw`, or `None` if it holds anything outside the allowlist.
pub fn sanitize(raw: &str) -> Option<String> {
    let mut out = String::with_capacity(raw.len() + raw.len() / 8);
    let mut stack: Vec<&str> = Vec::new();
    let mut rest = raw;
    while let Some(c) = rest.chars().next() {
        if c == '<'
            && let Some((tag, len)) = parse_tag(rest)
        {
            check(&tag, &mut stack)?;
            out.push_str(&rest[..len]);
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

/// Parses `<name a="v" ...>`, `</name>` or `<name .../>` at the start of `s`; `None` when
/// it is not a well-formed tag (then the `<` is text).
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
    let mut attrs = Vec::new();
    loop {
        match bytes.get(pos)? {
            b'>' => {
                return Some((
                    Tag {
                        name,
                        closing,
                        self_closing: false,
                        attrs,
                    },
                    pos + 1,
                ));
            }
            b'/' if bytes.get(pos + 1) == Some(&b'>') && !closing => {
                return Some((
                    Tag {
                        name,
                        closing,
                        self_closing: true,
                        attrs,
                    },
                    pos + 2,
                ));
            }
            b' ' if !closing => {
                pos += 1;
                let attr_start = pos;
                while bytes
                    .get(pos)
                    .is_some_and(|c| c.is_ascii_lowercase() || *c == b'-')
                {
                    pos += 1;
                }
                let attr = &s[attr_start..pos];
                if attr.is_empty()
                    || bytes.get(pos) != Some(&b'=')
                    || bytes.get(pos + 1) != Some(&b'"')
                {
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
                attrs.push((attr, &s[value_start..pos]));
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
        if !ATTRIBUTES.contains(name) || !safe_value(name, value) {
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

/// Values pulldown-latex writes are lengths, numbers, keywords and `rgb()` colours; a
/// narrow charset keeps `url(`, escapes and quotes out of `style`.
fn safe_value(name: &str, value: &str) -> bool {
    let charset = value
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || b" .-%#:;,()".contains(&c));
    let lower = value.to_ascii_lowercase();
    charset && !(name == "style" && (lower.contains("url") || lower.contains("expression")))
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
    use super::sanitize;

    #[test]
    fn hostile_mathml_is_escaped_or_refused() {
        assert_eq!(
            sanitize("<math><mo><</mo><mi>a</mi></math>").as_deref(),
            Some("<math><mo>&lt;</mo><mi>a</mi></math>")
        );
        // Unquoted attributes are not a tag pulldown-latex writes, so they become text.
        assert_eq!(
            sanitize("<math><mi><img src=x onerror=alert(1)></mi></math>").as_deref(),
            Some("<math><mi>&lt;img src=x onerror=alert(1)&gt;</mi></math>")
        );
        assert_eq!(sanitize("<math><mi><img src=\"x\"></mi></math>"), None);
        assert_eq!(sanitize("<math><mi>x</mi></math><img src=x>"), None);
        assert_eq!(sanitize("<math><mi>x</math>"), None);
        assert_eq!(sanitize("<math><mi onclick=\"x\">x</mi></math>"), None);
        assert_eq!(
            sanitize("<math><mi style=\"background:url(x)\">x</mi></math>"),
            None
        );
        assert!(sanitize("<math><mspace width=\"0.5em\"/><mi>a&amp;b</mi></math>").is_some());
    }
}
