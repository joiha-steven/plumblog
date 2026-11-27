//! Restricted Markdown for reader comments (docs/ARCHITECTURE.md, Reader site). A separate
//! entry point with its own small writer, not a flag on the post renderer, so nothing a reader
//! types can switch on post features (math, embeds, callouts, heading ids, tables, footnotes).
//!
//! Kept: paragraphs, emphasis, strong, inline code, code blocks, blockquotes, lists, links.
//! Links are absolute `http(s)` only and carry `rel="nofollow ugc noopener"`; any other link
//! is its plain text. Headings become paragraphs, images their alt text, raw HTML is dropped
//! (text between inline tags stays, as in posts).

use crate::html::escape;
use crate::link;
use pulldown_cmark::{Event, Options as Ext, Parser, Tag, TagEnd, TextMergeStream};

/// Rendered comment HTML and how many links the source tried to make (allowed or not), so
/// the caller can refuse link spam.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommentHtml {
    pub html: String,
    pub links: usize,
}

/// `rel` on every comment link: no ranking credit for spam, no `window.opener`.
pub const COMMENT_LINK_REL: &str = "nofollow ugc noopener";

/// Renders a reader comment. Never fails.
#[must_use]
pub fn render_comment(src: &str) -> CommentHtml {
    // No extensions: tables, footnotes, task lists, math, strikethrough and metadata blocks
    // are never parsed, so they stay plain text.
    let mut w = Writer::default();
    for event in TextMergeStream::new(Parser::new_ext(src, Ext::empty())) {
        w.event(event);
    }
    CommentHtml {
        html: w.out,
        links: w.links,
    }
}

#[derive(Default)]
struct Writer {
    out: String,
    links: usize,
    /// One entry per open link: whether it was written as `<a>`.
    open_links: Vec<bool>,
    /// Nesting depth inside an image: only its alt text is written.
    image: usize,
    in_code_block: bool,
}

impl Writer {
    fn event(&mut self, event: Event<'_>) {
        if self.image > 0 {
            match event {
                Event::Start(Tag::Image { .. }) => self.image += 1,
                Event::End(TagEnd::Image) => self.image -= 1,
                Event::Text(t) | Event::Code(t) => escape(&mut self.out, &t),
                _ => {}
            }
            return;
        }
        match event {
            Event::Start(tag) => self.start(&tag),
            Event::End(tag) => self.end(tag),
            Event::Text(t) => self.text(&t),
            Event::Code(t) => {
                self.out.push_str("<code>");
                escape(&mut self.out, &t);
                self.out.push_str("</code>");
            }
            Event::SoftBreak => self.out.push('\n'),
            Event::HardBreak => self.out.push_str("<br>\n"),
            // Raw HTML is dropped; thematic breaks are not part of comments. Anything else
            // (math, footnotes, task markers) needs an extension that is off.
            _ => {}
        }
    }

    fn start(&mut self, tag: &Tag<'_>) {
        let open = match tag {
            Tag::Paragraph | Tag::Heading { .. } => "<p>",
            Tag::BlockQuote(_) => "<blockquote>\n",
            Tag::CodeBlock(_) => {
                self.in_code_block = true;
                "<pre><code>"
            }
            Tag::List(Some(start)) if *start != 1 => {
                self.out.push_str("<ol start=\"");
                self.out.push_str(&start.to_string());
                self.out.push_str("\">\n");
                return;
            }
            Tag::List(Some(_)) => "<ol>\n",
            Tag::List(None) => "<ul>\n",
            Tag::Item => "<li>",
            Tag::Emphasis => "<em>",
            Tag::Strong => "<strong>",
            Tag::Link { dest_url, .. } => {
                self.links += 1;
                let href = comment_url(dest_url);
                self.open_links.push(href.is_some());
                if let Some(href) = href {
                    self.out.push_str("<a href=\"");
                    escape(&mut self.out, href);
                    self.out.push_str("\" rel=\"");
                    self.out.push_str(COMMENT_LINK_REL);
                    self.out.push_str("\">");
                }
                return;
            }
            Tag::Image { .. } => {
                self.image = 1;
                return;
            }
            // Tables, footnotes and the like are not parsed; their text flows on.
            _ => return,
        };
        self.out.push_str(open);
    }

    fn end(&mut self, tag: TagEnd) {
        let close = match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) => "</p>\n",
            TagEnd::BlockQuote(_) => "</blockquote>\n",
            TagEnd::CodeBlock => {
                self.in_code_block = false;
                "</code></pre>\n"
            }
            TagEnd::List(true) => "</ol>\n",
            TagEnd::List(false) => "</ul>\n",
            TagEnd::Item => "</li>\n",
            TagEnd::Emphasis => "</em>",
            TagEnd::Strong => "</strong>",
            TagEnd::Link if self.open_links.pop() == Some(true) => "</a>",
            _ => return,
        };
        self.out.push_str(close);
    }

    fn text(&mut self, t: &str) {
        if self.in_code_block || !self.open_links.is_empty() {
            escape(&mut self.out, t);
            return;
        }
        for (part, is_url) in link::autolinks(t) {
            if is_url {
                self.links += 1;
                let href = link::autolink_href(part);
                self.out.push_str("<a href=\"");
                escape(&mut self.out, &href);
                self.out.push_str("\" rel=\"");
                self.out.push_str(COMMENT_LINK_REL);
                self.out.push_str("\">");
                escape(&mut self.out, part);
                self.out.push_str("</a>");
            } else {
                escape(&mut self.out, part);
            }
        }
    }
}

/// The URL if it is an absolute `http(s)` URL with a host, else `None`. Exact prefix match:
/// any obfuscation (tabs, entities decoded to controls, leading junk) fails it.
fn comment_url(url: &str) -> Option<&str> {
    let url = url.trim_matches(|c: char| c == ' ');
    let rest = ["https://", "http://"].iter().find_map(|p| {
        url.get(..p.len())
            .filter(|h| h.eq_ignore_ascii_case(p))
            .map(|_| &url[p.len()..])
    })?;
    let host_ok = rest
        .chars()
        .next()
        .is_some_and(|c| c.is_alphanumeric() || c == '[');
    let clean = !url.chars().any(|c| c.is_control() || c.is_whitespace());
    (host_ok && clean).then_some(url)
}

#[cfg(test)]
mod tests {
    use super::{comment_url, render_comment};

    #[test]
    fn only_absolute_http_links() {
        assert_eq!(comment_url("https://a.com/x"), Some("https://a.com/x"));
        assert_eq!(comment_url("HTTP://a.com"), Some("HTTP://a.com"));
        for bad in [
            "/relative",
            "#top",
            "mailto:a@b.c",
            "javascript:alert(1)",
            "JaVaScRiPt:alert(1)",
            "  javascript:alert(1)",
            "jav\tascript:alert(1)",
            "data:text/html,x",
            "vbscript:x",
            "https:///evil",
            "https://a.com/\u{0}x",
            "//a.com",
        ] {
            assert_eq!(comment_url(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn kept_and_flattened() {
        let r = render_comment("# Title\n\n*a* **b** `c`\n\n> q\n\n- x\n\n3. y\n\n```\n<b>\n```\n");
        assert_eq!(
            r.html,
            "<p>Title</p>\n<p><em>a</em> <strong>b</strong> <code>c</code></p>\n\
             <blockquote>\n<p>q</p>\n</blockquote>\n<ul>\n<li>x</li>\n</ul>\n\
             <ol start=\"3\">\n<li>y</li>\n</ol>\n<pre><code>&lt;b&gt;\n</code></pre>\n"
        );
        assert_eq!(r.links, 0);
    }

    #[test]
    fn links_images_and_html() {
        let r = render_comment(
            "[ok](https://a.com) [no](javascript:alert(1)) ![alt *x*](https://i/x.png) \
             <b>bold</b><script>s</script> https://b.com <https://c.com>",
        );
        assert_eq!(
            r.html,
            "<p><a href=\"https://a.com\" rel=\"nofollow ugc noopener\">ok</a> no alt x \
             bolds <a href=\"https://b.com\" rel=\"nofollow ugc noopener\">https://b.com</a> \
             <a href=\"https://c.com\" rel=\"nofollow ugc noopener\">https://c.com</a></p>\n"
        );
        assert_eq!(r.links, 4);
    }

    #[test]
    fn post_features_stay_text() {
        let r = render_comment("$x$ | a |\n|---|\n\n- [ ] t\n\n> [!NOTE]\n> n\n\nx[^1]\n\n[^1]: f");
        for absent in [
            "<math",
            "<table",
            "<input",
            "callout",
            "fnref",
            "footnotes",
            " id=",
        ] {
            assert!(!r.html.contains(absent), "{absent} in {}", r.html);
        }
    }
}
