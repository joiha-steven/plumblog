//! URL safety, bare-URL autolinks and embed detection.

/// Returns the URL if it is safe in `href`/`src`, else `None`. Allowed: relative URLs,
/// fragments and the schemes below. Browsers ignore tabs and newlines inside a scheme
/// ("java\tscript:"), so those are removed before the scheme is read.
pub fn safe_url(url: &str, for_image: bool) -> Option<&str> {
    let url = url.trim();
    let cleaned: String = url
        .chars()
        .take_while(|c| !matches!(c, '/' | '?' | '#'))
        .filter(|c| !c.is_ascii_whitespace() && !c.is_ascii_control())
        .collect();
    let Some((scheme, _)) = cleaned.split_once(':') else {
        return Some(url);
    };
    let scheme = scheme.to_ascii_lowercase();
    let ok = if for_image {
        matches!(scheme.as_str(), "http" | "https")
    } else {
        matches!(scheme.as_str(), "http" | "https" | "mailto" | "tel")
    };
    ok.then_some(url)
}

/// Splits text into plain and URL parts for GFM extended autolinks (`https://...`,
/// `www....`), which pulldown-cmark does not do. Trailing punctuation and an unbalanced
/// closing parenthesis stay outside the link, as in GFM.
pub fn autolinks(text: &str) -> Vec<(&str, bool)> {
    let mut parts = Vec::new();
    let mut rest = text;
    while let Some(start) = find_url_start(rest) {
        let tail = &rest[start..];
        let mut end = tail
            .find(|c: char| c.is_whitespace() || c == '<')
            .unwrap_or(tail.len());
        loop {
            let candidate = &tail[..end];
            let Some(last) = candidate.chars().last() else {
                break;
            };
            let unbalanced_paren =
                last == ')' && candidate.matches(')').count() > candidate.matches('(').count();
            if matches!(
                last,
                '?' | '!' | '.' | ',' | ':' | '*' | '_' | '~' | '\'' | '"'
            ) || unbalanced_paren
            {
                end -= last.len_utf8();
            } else {
                break;
            }
        }
        let url = &tail[..end];
        // "https://" or "www." alone is not a link.
        let host = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))
            .unwrap_or(url);
        if host.is_empty() || host == "www." || !host.contains('.') {
            parts.push((&rest[..start + end.max(1)], false));
            rest = &rest[start + end.max(1)..];
            continue;
        }
        if start > 0 {
            parts.push((&rest[..start], false));
        }
        parts.push((url, true));
        rest = &tail[end..];
    }
    if !rest.is_empty() {
        parts.push((rest, false));
    }
    parts
}

/// Byte offset of the next `http://`, `https://` or `www.` that starts a word.
fn find_url_start(s: &str) -> Option<usize> {
    let bytes = s.as_bytes();
    (0..s.len()).find(|&i| {
        let at_word_start = i == 0 || !bytes[i - 1].is_ascii_alphanumeric();
        at_word_start
            && s.is_char_boundary(i)
            && ["https://", "http://", "www."].iter().any(|p| {
                s[i..]
                    .get(..p.len())
                    .is_some_and(|h| h.eq_ignore_ascii_case(p))
            })
    })
}

/// `www.` links get a scheme so they are not read as relative paths.
pub fn autolink_href(url: &str) -> String {
    if url.get(..4).is_some_and(|p| p.eq_ignore_ascii_case("www.")) {
        format!("https://{url}")
    } else {
        url.to_owned()
    }
}

/// A video link on its own line becomes a click-to-load facade.
#[derive(Debug, PartialEq, Eq)]
pub struct Embed {
    pub provider: &'static str,
    pub id: String,
    /// Canonical page URL: what the facade links to before (and without) JS.
    pub url: String,
}

pub fn embed(url: &str) -> Option<Embed> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))?;
    let rest = rest.strip_prefix("www.").unwrap_or(rest);
    let rest = rest.strip_prefix("m.").unwrap_or(rest);
    let youtube_id = if let Some(r) = rest.strip_prefix("youtube.com/watch?") {
        r.split('&').find_map(|kv| kv.strip_prefix("v="))
    } else if let Some(r) = rest.strip_prefix("youtu.be/") {
        Some(r)
    } else {
        rest.strip_prefix("youtube.com/shorts/")
            .or_else(|| rest.strip_prefix("youtube.com/embed/"))
    };
    if let Some(id) = youtube_id {
        let id = id.split(['?', '&', '#', '/']).next().unwrap_or("");
        let valid = id.len() == 11
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        return valid.then(|| Embed {
            provider: "youtube",
            id: id.to_owned(),
            url: format!("https://www.youtube.com/watch?v={id}"),
        });
    }
    let id = rest.strip_prefix("vimeo.com/")?;
    let id = id.split(['?', '#', '/']).next().unwrap_or("");
    let valid = (1..=12).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit());
    valid.then(|| Embed {
        provider: "vimeo",
        id: id.to_owned(),
        url: format!("https://vimeo.com/{id}"),
    })
}

#[cfg(test)]
mod tests {
    use super::{autolinks, embed, safe_url};

    #[test]
    fn unsafe_schemes_are_refused() {
        for bad in [
            "javascript:alert(1)",
            "JaVaScRiPt:alert(1)",
            "java\tscript:alert(1)",
            " javascript:alert(1)",
            "vbscript:x",
            "data:text/html;base64,PHNjcmlwdD4=",
        ] {
            assert_eq!(safe_url(bad, false), None, "{bad:?}");
        }
        assert_eq!(safe_url("data:image/png;base64,AAAA", true), None);
        for good in [
            "https://a.b/c",
            "/posts/x",
            "#fn-1",
            "mailto:a@b.c",
            "x/y:z",
        ] {
            assert_eq!(safe_url(good, false), Some(good), "{good:?}");
        }
    }

    #[test]
    fn autolinks_trim_trailing_punctuation() {
        assert_eq!(
            autolinks("see https://a.com/x). ok"),
            [("see ", false), ("https://a.com/x", true), ("). ok", false)]
        );
        assert_eq!(
            autolinks("(www.a.com/b_(c))."),
            [("(", false), ("www.a.com/b_(c)", true), (").", false)]
        );
        assert_eq!(autolinks("nowww.a.com"), [("nowww.a.com", false)]);
    }

    #[test]
    fn video_links_become_embeds() {
        let e = embed("https://youtu.be/dQw4w9WgXcQ?t=1").unwrap();
        assert_eq!((e.provider, e.id.as_str()), ("youtube", "dQw4w9WgXcQ"));
        assert!(embed("https://www.youtube.com/watch?list=x&v=dQw4w9WgXcQ").is_some());
        assert_eq!(
            embed("https://vimeo.com/76979871").unwrap().provider,
            "vimeo"
        );
        assert!(embed("https://youtube.com/watch?v=short").is_none());
        assert!(embed("https://example.com/watch?v=dQw4w9WgXcQ").is_none());
    }
}
