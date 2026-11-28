//! Fixed user-facing text in templates and partials: words outside `{{ }}`, `{% %}` and
//! tags, and literal `alt`, `title`, `aria-label` and `placeholder` values. Text belongs in
//! `locales/` (docs/ARCHITECTURE.md, Themes).

/// `(line, text)` of each piece of fixed text in `src`.
#[must_use]
pub fn find(src: &str) -> Vec<(usize, String)> {
    let blanked = blank_template(src);
    let b = blanked.as_bytes();
    let line_at = |i: usize| blanked[..i].matches('\n').count() + 1;
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if blanked[i..].starts_with("<!--") {
            i = blanked[i..].find("-->").map_or(b.len(), |e| i + e + 3);
        } else if b[i] == b'<'
            && b.get(i + 1)
                .is_some_and(|c| c.is_ascii_alphabetic() || *c == b'/' || *c == b'!')
        {
            let end = tag_end(&blanked, i);
            attrs(&blanked[i..end], line_at(i), &mut out);
            let name: String = blanked[i + 1..]
                .chars()
                .take_while(char::is_ascii_alphanumeric)
                .collect::<String>()
                .to_ascii_lowercase();
            i = end;
            if name == "script" || name == "style" {
                let close = format!("</{name}");
                i = blanked[i..]
                    .to_ascii_lowercase()
                    .find(&close)
                    .map_or(b.len(), |e| i + e);
            }
        } else {
            let next = blanked[i..].find('<').map_or(b.len(), |e| i + e.max(1));
            let text = &blanked[i..next];
            let words = text_words(text);
            if !words.is_empty() {
                let offset = text.len() - text.trim_start().len();
                out.push((line_at(i + offset), words));
            }
            i = next;
        }
    }
    out
}

/// Template code replaced by one `\u{1}` per block (newlines kept, so line numbers hold).
fn blank_template(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(i) = rest.find('{') {
        let close = match rest.as_bytes().get(i + 1) {
            Some(b'{') => "}}",
            Some(b'%') => "%}",
            Some(b'#') => "#}",
            _ => {
                out.push_str(&rest[..=i]);
                rest = &rest[i + 1..];
                continue;
            }
        };
        out.push_str(&rest[..i]);
        let body = &rest[i + 2..];
        let end = body.find(close).map_or(body.len(), |e| e + 2);
        out.push('\u{1}');
        out.extend(body[..end].chars().filter(|c| *c == '\n'));
        rest = &body[end.min(body.len())..];
    }
    out.push_str(rest);
    out
}

/// End of the tag starting at `start` (quote-aware).
fn tag_end(s: &str, start: usize) -> usize {
    let mut quote = None;
    for (i, c) in s[start..].char_indices() {
        match (quote, c) {
            (None, '"' | '\'') => quote = Some(c),
            (Some(q), c) if c == q => quote = None,
            (None, '>') => return start + i + 1,
            _ => {}
        }
    }
    s.len()
}

fn attrs(tag: &str, line: usize, out: &mut Vec<(usize, String)>) {
    for name in ["alt", "title", "aria-label", "placeholder"] {
        for (i, _) in tag.match_indices(name) {
            let before_ok = tag[..i].ends_with(|c: char| c.is_ascii_whitespace());
            let rest = tag[i + name.len()..].trim_start();
            let Some(rest) = rest.strip_prefix('=').map(str::trim_start) else {
                continue;
            };
            let Some(q) = rest.chars().next().filter(|c| *c == '"' || *c == '\'') else {
                continue;
            };
            let value = rest[1..].split(q).next().unwrap_or_default();
            if before_ok && value.chars().any(char::is_alphabetic) {
                out.push((
                    line,
                    format!("{name}=\"{}\"", value.replace('\u{1}', "{..}")),
                ));
            }
        }
    }
}

/// The letters-bearing text of a text node, entities dropped; empty when it is only
/// punctuation, numbers or template output.
fn text_words(text: &str) -> String {
    let mut plain = String::new();
    let mut rest = text;
    while let Some(i) = rest.find('&') {
        plain.push_str(&rest[..i]);
        rest = match rest[i..].find(';') {
            Some(e) if e <= 10 => &rest[i + e + 1..],
            _ => &rest[i + 1..],
        };
    }
    plain.push_str(rest);
    if plain.chars().any(char::is_alphabetic) {
        plain
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .replace('\u{1}', "{..}")
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::find;

    #[test]
    fn finds_fixed_words_only() {
        let src = "<p class=\"x\">{{ t(\"a.b\") }}</p>\n<a href=\"/\" title=\"Home\">&middot; {{ x }} 5</a>\n<p>Read more</p>\n<img alt=\"{{ t('a') }}\">\n<!-- Note --><script>var a = 'word';</script>{# c #}";
        let hits = find(src);
        assert_eq!(
            hits,
            vec![
                (2, "title=\"Home\"".to_owned()),
                (3, "Read more".to_owned())
            ]
        );
    }

    #[test]
    fn template_logic_is_not_text() {
        assert!(find("{% if a == \"yes\" %}<b>{{ n }}</b>{% endif %}").is_empty());
    }
}
