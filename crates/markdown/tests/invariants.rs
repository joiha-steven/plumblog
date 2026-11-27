//! Safety invariants over every output the renderer can produce: whatever the input, the
//! HTML holds only allowlisted elements and attributes, no event handlers, no script URLs,
//! and every `<` and `&` in text is escaped. Byte-exact fixtures cannot catch a new hole;
//! this test re-parses the output and checks each token.

use plumblog_markdown::{COMMENT_LINK_REL, Options, render, render_comment};
use std::fs;
use std::path::Path;

const HTML_ELEMENTS: &[&str] = &[
    "p",
    "em",
    "strong",
    "del",
    "code",
    "a",
    "img",
    "figure",
    "figcaption",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "ul",
    "ol",
    "li",
    "label",
    "input",
    "blockquote",
    "div",
    "pre",
    "br",
    "hr",
    "table",
    "thead",
    "tbody",
    "tr",
    "th",
    "td",
    "sup",
    "section",
];

const MATH_ELEMENTS: &[&str] = &[
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
    "id",
    "role",
    "data-kind",
    "data-lang",
    "data-align",
    "data-provider",
    "data-id",
    "href",
    // Comment links only (`nofollow ugc noopener`).
    "rel",
    "title",
    "src",
    "alt",
    "loading",
    "decoding",
    "type",
    "disabled",
    "checked",
    "start",
];

const MATH_ATTRIBUTES: &[&str] = &[
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

/// Checks one rendered document; returns the first violation.
fn violations(html: &str) -> Result<(), String> {
    let mut rest = html;
    let mut in_math = 0usize;
    while let Some(i) = rest.find(['<', '&']) {
        let at = &rest[i..];
        if let Some(entity) = at.strip_prefix('&') {
            let ok = entity.find(';').is_some_and(|end| {
                let name = &entity[..end];
                end < 10
                    && (name.chars().all(|c| c.is_ascii_alphabetic())
                        || name.strip_prefix('#').is_some_and(|n| {
                            n.chars().all(|c| c.is_ascii_digit())
                                || n.strip_prefix('x')
                                    .is_some_and(|h| h.chars().all(|c| c.is_ascii_hexdigit()))
                        }))
                    && end > 0
            });
            if !ok {
                return Err(format!("unescaped & at {:?}", &at[..at.len().min(30)]));
            }
            rest = &at[1..];
            continue;
        }
        let end = at
            .find('>')
            .ok_or_else(|| format!("unterminated < at {at:?}"))?;
        let tag = &at[1..end];
        let (closing, body) = match tag.strip_prefix('/') {
            Some(b) => (true, b),
            None => (false, tag),
        };
        let name_end = body.find(' ').unwrap_or(body.len());
        let name = &body[..name_end];
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err(format!("unescaped < or bad tag {:?}", &at[..=end]));
        }
        let math = MATH_ELEMENTS.contains(&name);
        if !(HTML_ELEMENTS.contains(&name) || math) {
            return Err(format!("element not allowed: <{tag}>"));
        }
        if math && in_math == 0 && name != "math" {
            return Err(format!("MathML element outside <math>: <{tag}>"));
        }
        if !math && in_math > 0 {
            return Err(format!("HTML element inside <math>: <{tag}>"));
        }
        if name == "math" {
            if closing { in_math -= 1 } else { in_math += 1 }
        }
        check_attributes(&body[name_end..], math).map_err(|e| format!("{e} in <{tag}>"))?;
        rest = &at[end + 1..];
    }
    if in_math != 0 {
        return Err("unbalanced <math>".into());
    }
    Ok(())
}

fn check_attributes(attrs: &str, math: bool) -> Result<(), String> {
    // A self-closing tag (`<mspace width="1em" />`) ends in `/`, which is not an attribute.
    let mut attrs = attrs.trim_end().strip_suffix('/').unwrap_or(attrs);
    loop {
        attrs = attrs.trim_start();
        if attrs.is_empty() {
            return Ok(());
        }
        let name_end = attrs.find(['=', ' ']).unwrap_or(attrs.len());
        let name = &attrs[..name_end];
        // `style` is never allowed: it would let an author restyle the page (overlays).
        let allowed = if math { MATH_ATTRIBUTES } else { ATTRIBUTES };
        if name.starts_with("on") || !allowed.contains(&name) {
            return Err(format!("attribute not allowed: {name}"));
        }
        attrs = &attrs[name_end..];
        let Some(v) = attrs.strip_prefix("=\"") else {
            continue;
        };
        let close = v.find('"').ok_or("unterminated attribute value")?;
        let value = &v[..close];
        if value.contains(['<', '>']) {
            return Err(format!("raw < or > in {name}={value:?}"));
        }
        if matches!(name, "href" | "src") {
            // What the browser sees: entities decoded, control and space characters gone.
            let seen: String = decode_entities(value)
                .chars()
                .filter(|c| !c.is_whitespace() && !c.is_control())
                .collect::<String>()
                .to_ascii_lowercase();
            for bad in ["javascript:", "data:", "vbscript:"] {
                if seen.starts_with(bad) {
                    return Err(format!("{name} with {bad} in {value:?}"));
                }
            }
        }
        attrs = &v[close + 1..];
    }
}

/// Decodes numeric and the few named entities that matter for URL schemes.
fn decode_entities(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        let decoded = tail.find(';').and_then(|end| {
            let body = &tail[1..end];
            let c = if let Some(hex) = body.strip_prefix("#x").or_else(|| body.strip_prefix("#X")) {
                u32::from_str_radix(hex, 16).ok().and_then(char::from_u32)
            } else if let Some(dec) = body.strip_prefix('#') {
                dec.parse().ok().and_then(char::from_u32)
            } else {
                match body {
                    "colon" => Some(':'),
                    "Tab" => Some('\t'),
                    "NewLine" => Some('\n'),
                    "amp" => Some('&'),
                    "quot" => Some('"'),
                    "lt" => Some('<'),
                    "gt" => Some('>'),
                    _ => None,
                }
            };
            c.map(|c| (c, end + 1))
        });
        if let Some((c, len)) = decoded {
            out.push(c);
            rest = &tail[len..];
        } else {
            out.push('&');
            rest = &tail[1..];
        }
    }
    out.push_str(rest);
    out
}

#[test]
fn checker_sees_through_obfuscated_schemes() {
    for href in [
        "&#106;avascript:alert(1)",
        "&#x6A;avascript&colon;alert(1)",
        "java&#9;script:alert(1)",
        "\u{1}javascript:alert(1)",
        " JaVaScRiPt:alert(1)",
        "data:text/html,x",
    ] {
        let attrs = format!(" href=\"{href}\"");
        assert!(
            check_attributes(&attrs, false).is_err(),
            "checker missed {href:?}"
        );
    }
    assert!(check_attributes(" width=\"1em\" /", true).is_ok());
    assert!(check_attributes(" style=\"color:red\"", true).is_err());
}

#[test]
fn every_fixture_output_is_safe() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    for entry in fs::read_dir(dir).expect("fixtures dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_some_and(|x| x == "md") {
            let src = fs::read_to_string(&path).expect("UTF-8 fixture");
            let html = render(&src, Options::default()).html;
            if let Err(e) = violations(&html) {
                panic!("{}: {e}", path.display());
            }
        }
    }
}

const PAYLOADS: &[&str] = &[
    "<img src=x onerror=alert(1)>",
    "<svg onload=alert(1)>",
    "<script>alert(1)</script>",
    "\"><img src=x onerror=alert(1)>",
    "javascript:alert(1)",
    "a < b & c > d",
    "</math><img src=x onerror=alert(1)>",
    "&#60;script&#62;",
    "<mtext style=\"position:fixed;inset:0;font-size:500px\">PHISH</mtext>",
    "</mi></math><math><mi>",
    "<mspace width=\"999em\" />",
];

/// Every payload in every context the renderer treats specially.
fn hostile_inputs() -> Vec<String> {
    let contexts: &[fn(&str) -> String] = &[
        |p| p.to_owned(),
        |p| format!("*{p}*"),
        |p| format!("[{p}]({p})"),
        |p| format!("[x](https://a.com \"{p}\")"),
        |p| format!("![{p}]({p} \"{p}\")"),
        |p| format!("![{p}](/i.png \"{p}\")"),
        |p| format!("## {p}"),
        |p| format!("`{p}`"),
        |p| format!("```{p}\n{p}\n```"),
        |p| format!("```math\n{p}\n```"),
        |p| format!("${p}$"),
        |p| format!("$$\n{p}\n$$"),
        |p| format!("$\\operatorname{{{p}}}$"),
        |p| format!("$\\mathop{{{p}}}$"),
        |p| format!("$\\mathrm{{{p}}}$"),
        |p| format!("$\\text{{{p}}}$"),
        |p| format!("$$\\operatorname{{{p}}}$$"),
        |p| format!("```math\n\\mathop{{{p}}}\n```"),
        |p| format!("| {p} |\n|---|\n| {p} |"),
        |p| format!("> [!NOTE]\n> {p}"),
        |p| format!("x[^a]\n\n[^a]: {p}"),
        |p| format!("- [ ] {p}"),
        |p| format!("<{p}>"),
        |p| format!("https://a.com/{p}"),
        |p| format!("![a ${p}$ b](https://x/i.png)"),
    ];
    let mut out = Vec::new();
    for p in PAYLOADS {
        for c in contexts {
            out.push(c(p));
        }
    }
    out
}

#[test]
fn hostile_inputs_render_safely() {
    for src in hostile_inputs() {
        let html = render(&src, Options::default()).html;
        if let Err(e) = violations(&html) {
            panic!("input {src:?}\noutput {html:?}\n{e}");
        }
        // Author-typed tags must never reach the MathML at all, not even as escaped text
        // (that would mean they only failed to parse). Inside `\text{}`, `\operatorname{}`
        // and `\mathop{}` a `<` is a literal character and correctly shown as one, so those
        // are exempt.
        if !["\\text", "\\operatorname", "\\mathop"]
            .iter()
            .any(|c| src.contains(c))
        {
            for part in html.split("<math").skip(1) {
                let math = part.split("</math>").next().unwrap_or_default();
                assert!(
                    !math.contains("&lt;m") && !math.contains("&lt;/m"),
                    "author tag text inside <math>\ninput {src:?}\noutput {html:?}"
                );
            }
        }
    }
}

/// Comment-only payloads on top of [`PAYLOADS`]: scheme obfuscation, reference links and
/// autolinks, entities, invisible and bidi characters.
const COMMENT_PAYLOADS: &[&str] = &[
    "[x](jav&#x09;ascript:alert(1))",
    "[x](JaVaScRiPt:alert(1))",
    "[x](   javascript:alert(1))",
    "[x](&#106;avascript:alert(1))",
    "[x](data:text/html;base64,PHNjcmlwdD4=)",
    "[x](vbscript:msgbox(1))",
    "[x][r]\n\n[r]: javascript:alert(1)",
    "[x][r]\n\n[r]: https://ok.example/",
    "<javascript:alert(1)>",
    "<https://a.example/\"onmouseover=\"alert(1)>",
    "![x\" onerror=\"alert(1)](https://i.example/x.png)",
    "![<img src=x onerror=alert(1)>](javascript:alert(1))",
    "<style>body{display:none}</style>",
    "<iframe src=https://evil.example></iframe>",
    "&lt;script&gt;alert(1)&lt;/script&gt;",
    "&#x3C;script&#x3E;",
    "\u{202e}gnp.exe\u{200b}\u{2066}x\u{2069}",
    "$$x$$ \\(y\\)",
];

fn comment_inputs() -> Vec<String> {
    let contexts: &[fn(&str) -> String] = &[
        |p| p.to_owned(),
        |p| format!("*{p}*"),
        |p| format!("[{p}]({p})"),
        |p| format!("[x](https://a.com \"{p}\")"),
        |p| format!("![{p}]({p} \"{p}\")"),
        |p| format!("## {p}"),
        |p| format!("`{p}`"),
        |p| format!("```{p}\n{p}\n```"),
        |p| format!("> {p}"),
        |p| format!("- {p}"),
        |p| format!("<{p}>"),
        |p| format!("https://a.com/{p}"),
    ];
    let mut out = Vec::new();
    for p in PAYLOADS.iter().chain(COMMENT_PAYLOADS) {
        for c in contexts {
            out.push(c(p));
        }
    }
    out.push(format!("{}x{}", "*_".repeat(2500), "_*".repeat(2500)));
    out.push("[".repeat(5000));
    out.push("> ".repeat(2500));
    out.push(format!("{}{}", "<script>".repeat(600), "x"));
    out
}

/// What a browser would see as a scheme: entities decoded, spaces and controls gone.
fn seen(html: &str) -> String {
    decode_entities(html)
        .chars()
        .filter(|c| !c.is_whitespace() && !c.is_control())
        .collect::<String>()
        .to_ascii_lowercase()
}

#[test]
fn hostile_comments_render_safely() {
    for src in comment_inputs() {
        let html = render_comment(&src).html;
        if let Err(e) = violations(&html) {
            panic!("input {src:?}\noutput {html:?}\n{e}");
        }
        for banned in [
            "<script", "<style", "<iframe", "<img", "<math", "<input", "<table", " id=",
        ] {
            assert!(!html.contains(banned), "{banned} in {html:?} from {src:?}");
        }
        for href in html.split("href=\"").skip(1) {
            let value = href.split('"').next().unwrap_or_default();
            let v = seen(value);
            assert!(
                v.starts_with("http://") || v.starts_with("https://"),
                "comment link {value:?} from {src:?}"
            );
        }
        assert_eq!(
            html.matches("<a ").count(),
            html.matches(&format!("rel=\"{COMMENT_LINK_REL}\"")).count(),
            "every comment link carries rel: {html:?}"
        );
    }
}

#[test]
fn comment_body_at_the_limit_renders() {
    let body = "word *em* [l](https://a.example) ".repeat(150);
    let body: String = body.chars().take(5000).collect();
    let r = render_comment(&body);
    assert!(violations(&r.html).is_ok());
    assert!(r.links >= 140, "{} links", r.links);
}
