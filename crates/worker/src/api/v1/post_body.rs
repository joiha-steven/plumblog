//! What a post save derives from the rendered HTML (the store never parses HTML): the media
//! it uses, its plain text for search, a word count, and the table of contents as JSON.

use plumblog_markdown::TocEntry;

use crate::api::body::{Out, object};

/// Our media URL: `/media/<id>` then `/`, `.` or the end (`/media/12/heron.avif`,
/// `/media/12.avif`), site-relative. Anything else (other hosts, names without an id) is not
/// a library item, so it does not hold media against deletion.
const MEDIA_PREFIX: &str = "/media/";

fn media_id(src: &str) -> Option<i64> {
    let rest = src.strip_prefix(MEDIA_PREFIX)?;
    let end = rest.find(['/', '.']).unwrap_or(rest.len());
    let digits = &rest[..end];
    let ok = !digits.is_empty()
        && digits.len() <= 18
        && digits.bytes().all(|b| b.is_ascii_digit())
        && !digits.starts_with('0');
    if ok { digits.parse().ok() } else { None }
}

/// Ids of our media in `<img src>` of rendered HTML, ascending, without duplicates. The
/// renderer writes attributes as `src="..."`, so no other quoting needs handling.
#[must_use]
pub fn media_ids(html: &str) -> Vec<i64> {
    let mut ids: Vec<i64> = html
        .split("<img ")
        .skip(1)
        .filter_map(|tag| {
            let tag = &tag[..tag.find('>').unwrap_or(tag.len())];
            let at = tag
                .find(" src=\"")
                .map(|i| i + 6)
                .or_else(|| tag.starts_with("src=\"").then_some(5))?;
            let value = &tag[at..];
            media_id(&value[..value.find('"')?])
        })
        .collect();
    ids.sort_unstable();
    ids.dedup();
    ids
}

/// Visible text of rendered HTML: tags dropped (each one a word break), the escapes the
/// renderer writes undone. Same rule as the sample-post seeder (pages/seed.rs).
#[must_use]
pub fn plain_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// Words as the renderer counts them for reading time: each CJK or Thai-like character is
/// one, so unspaced scripts are not counted as a single word per sentence.
#[must_use]
pub fn word_count(text: &str) -> i64 {
    i64::try_from(plumblog_markdown::word_count(text)).unwrap_or(i64::MAX)
}

/// `[{"level":2,"id":"...","text":"..."}, ...]`.
#[must_use]
pub fn toc_json(toc: &[TocEntry]) -> String {
    let items: Vec<String> = toc
        .iter()
        .map(|t| {
            object(&[
                ("level", Out::Int(i64::from(t.level))),
                ("id", Out::Str(t.id.clone())),
                ("text", Out::Str(t.text.clone())),
            ])
        })
        .collect();
    format!("[{}]", items.join(","))
}

#[cfg(test)]
mod tests {
    use super::{media_ids, plain_text, toc_json, word_count};

    #[test]
    fn media_ids_from_rendered_markdown() {
        let md = "![a](/media/12/heron.avif \"Cap\")\n\nText ![b](/media/3.avif) and \
                  ![c](/media/12.jpg) ![d](/media/heron.avif) ![e](https://x.test/media/9.avif) \
                  ![f](/media/007.avif) ![g](/media/5)\n\n[link](/media/8.avif)";
        let html = plumblog_markdown::render(md, plumblog_markdown::Options::default()).html;
        assert_eq!(media_ids(&html), vec![3, 5, 12], "{html}");
        assert!(media_ids("<p>no images</p>").is_empty());
        assert!(media_ids("<img src=\"/media/4.avif").is_empty());
    }

    #[test]
    fn text_and_words() {
        let html = plumblog_markdown::render(
            "# Hi & bye\n\nOne <b>two</b> `x<y`\n",
            plumblog_markdown::Options::default(),
        );
        let text = plain_text(&html.html);
        assert!(text.contains("Hi & bye"), "{text}");
        assert!(text.contains("x<y"), "{text}");
        assert_eq!(word_count(&text), 6, "{text}");
        // Unspaced scripts: one word per character, not one per run.
        assert_eq!(word_count("\u{65e5}\u{672c}\u{8a9e} \u{4e2d}\u{6587}"), 5);
        assert_eq!(word_count("\u{3053}\u{3093}\u{306b}\u{3061}\u{306f}"), 5);
        assert_eq!(word_count("\u{e2a}\u{e27}\u{e31}\u{e2a}\u{e14}\u{e35}"), 4);
        assert_eq!(word_count("Hello \u{4e16}\u{754c} ok"), 4);
        let toc = toc_json(&html.toc);
        assert!(toc.starts_with("[{\"level\":1,"), "{toc}");
    }
}
