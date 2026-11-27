//! Event-to-HTML writer. Own writer instead of pulldown-cmark's so the output matches the
//! theme contract, raw HTML is dropped and URLs are filtered in one place.

use crate::link::{self, Embed};
use crate::mathml;
use crate::text::{self, Counter};
use crate::{Options, Rendered, TocEntry, Warning};
use pulldown_cmark::{
    Alignment, BlockQuoteKind, CodeBlockKind, Event, LinkType, Options as Ext, Parser, Tag, TagEnd,
    TextMergeStream,
};
use std::collections::{HashMap, HashSet};
use std::fmt::Write;

pub fn render(src: &str, options: Options) -> Rendered {
    let ext = Ext::ENABLE_TABLES
        | Ext::ENABLE_FOOTNOTES
        | Ext::ENABLE_STRIKETHROUGH
        | Ext::ENABLE_TASKLISTS
        | Ext::ENABLE_MATH
        | Ext::ENABLE_GFM
        | Ext::ENABLE_YAML_STYLE_METADATA_BLOCKS;
    let events: Vec<Event> = TextMergeStream::new(Parser::new_ext(src, ext)).collect();
    let mut w = Writer::new(&events);
    let mut i = 0;
    while i < events.len() {
        i = w.event(&events, i) + 1;
    }
    w.finish(options)
}

#[derive(Default)]
struct Writer {
    /// Output stack: the post body at the bottom, headings and footnote bodies above it.
    bufs: Vec<String>,
    defined_notes: HashSet<String>,
    note_order: Vec<String>,
    note_refs: HashMap<String, usize>,
    note_bodies: HashMap<String, String>,
    note_depth: usize,
    note_label: Option<String>,
    /// Id of the first reference to each note, where its back link points.
    note_first_ref: HashMap<String, String>,
    ids: HashSet<String>,
    toc: Vec<TocEntry>,
    heading_text: Option<String>,
    excerpt_src: String,
    para_depth: usize,
    words: Counter,
    metadata: bool,
    code: Option<CodeKind>,
    math_src: String,
    image: Vec<(String, String, bool)>,
    alt: String,
    figure: bool,
    links: Vec<bool>,
    label_open: bool,
    table: Table,
    seen: Seen,
    warnings: Vec<Warning>,
}

#[derive(Default)]
struct Table {
    align: Vec<Alignment>,
    cell: usize,
    in_head: bool,
    tbody_open: bool,
}

/// Features found, so pages load extra assets only when needed.
#[derive(Default)]
struct Seen {
    code: bool,
    math: bool,
    embed: bool,
}

#[derive(PartialEq, Eq)]
enum CodeKind {
    Code,
    Math,
}

impl Writer {
    fn new(events: &[Event]) -> Self {
        let (defined_notes, note_order) = note_numbering(events);
        Self {
            bufs: vec![String::new()],
            defined_notes,
            note_order,
            ..Self::default()
        }
    }

    fn out(&mut self) -> &mut String {
        self.bufs.last_mut().expect("output stack is never empty")
    }

    fn push(&mut self, s: &str) {
        self.out().push_str(s);
    }

    fn esc(&mut self, s: &str) {
        escape(self.out(), s);
    }

    fn close_label(&mut self) {
        if self.label_open {
            self.label_open = false;
            self.push("</label>");
        }
    }

    /// Text that is prose (not code, alt text or metadata) feeds the counters.
    fn prose(&mut self, s: &str) {
        // NFC so decomposed input (macOS) counts and cuts like composed input.
        let s = &*text::nfc(s);
        self.words.add(s);
        if let Some(h) = &mut self.heading_text {
            h.push_str(s);
        } else if self.para_depth > 0 && self.note_depth == 0 {
            self.excerpt_src.push_str(s);
        }
    }

    /// Handles `events[i]`; returns the index of the last event it consumed.
    fn event(&mut self, events: &[Event], i: usize) -> usize {
        match &events[i] {
            Event::Start(tag) => return self.start(tag, events, i),
            Event::End(tag) => self.end(*tag),
            Event::Text(t) => self.text(t),
            Event::Code(t) => {
                if self.image.is_empty() {
                    self.prose(t);
                    self.push("<code>");
                    self.esc(t);
                    self.push("</code>");
                } else {
                    self.alt.push_str(t);
                }
            }
            // Inside alt text, math and note markers are plain words.
            Event::InlineMath(t) | Event::DisplayMath(t) if !self.image.is_empty() => {
                self.alt.push_str(t);
            }
            Event::FootnoteReference(_) | Event::HardBreak if !self.image.is_empty() => {
                self.alt.push(' ');
            }
            Event::InlineMath(t) => self.math(t, false),
            Event::DisplayMath(t) => self.math(t, true),
            // Raw HTML is not allowed (docs/ARCHITECTURE.md, Markdown): tags are dropped and
            // any text between inline tags stays, so `<b>word</b>` still reads "word".
            Event::Html(_) | Event::InlineHtml(_) => {}
            Event::FootnoteReference(label) => self.note_ref(label),
            Event::SoftBreak => {
                if self.image.is_empty() {
                    self.prose(" ");
                    self.push("\n");
                } else {
                    self.alt.push(' ');
                }
            }
            Event::HardBreak => self.push("<br>\n"),
            Event::Rule => self.push("<hr>\n"),
            Event::TaskListMarker(checked) => {
                self.push(if *checked {
                    "<label><input type=\"checkbox\" disabled checked> "
                } else {
                    "<label><input type=\"checkbox\" disabled> "
                });
                self.label_open = true;
            }
        }
        i
    }

    fn text(&mut self, t: &str) {
        if self.metadata {
            return;
        }
        if !self.image.is_empty() {
            self.alt.push_str(t);
            return;
        }
        match self.code {
            Some(CodeKind::Math) => self.math_src.push_str(t),
            Some(CodeKind::Code) => {
                self.words.add(t);
                self.esc(t);
            }
            None => {
                self.prose(t);
                if self.links.is_empty() {
                    for (part, is_url) in link::autolinks(t) {
                        if is_url {
                            let href = link::autolink_href(part);
                            self.push("<a href=\"");
                            self.esc(&href);
                            self.push("\">");
                            self.esc(part);
                            self.push("</a>");
                        } else {
                            self.esc(part);
                        }
                    }
                } else {
                    self.esc(t);
                }
            }
        }
    }

    fn start(&mut self, tag: &Tag, events: &[Event], i: usize) -> usize {
        // Inside alt text only words survive; markup there would leak outside the `<img>`.
        if !self.image.is_empty() && !matches!(tag, Tag::Image { .. }) {
            return i;
        }
        match tag {
            Tag::Paragraph => {
                if let Some((embed, last)) = embed_paragraph(events, i) {
                    self.embed(&embed);
                    return last;
                }
                self.figure = image_paragraph(events, i);
                if !self.figure {
                    self.push("<p>");
                }
                self.para_depth += 1;
            }
            Tag::Heading { .. } => {
                self.close_label();
                self.bufs.push(String::new());
                self.heading_text = Some(String::new());
            }
            Tag::BlockQuote(kind) => {
                self.close_label();
                match kind {
                    None => self.push("<blockquote>\n"),
                    Some(k) => {
                        self.push("<div class=\"callout\" data-kind=\"");
                        self.push(callout_kind(*k));
                        self.push("\" role=\"note\">\n");
                    }
                }
            }
            Tag::CodeBlock(kind) => {
                self.close_label();
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => code_lang(info),
                    CodeBlockKind::Indented => String::new(),
                };
                if lang == "math" {
                    self.code = Some(CodeKind::Math);
                    self.math_src.clear();
                } else {
                    self.code = Some(CodeKind::Code);
                    self.seen.code = true;
                    self.push("<div class=\"code\"");
                    if !lang.is_empty() {
                        self.push(" data-lang=\"");
                        self.esc(&lang);
                        self.push("\"");
                    }
                    self.push("><pre><code>");
                }
            }
            Tag::List(start) => {
                self.close_label();
                // A nested list starts on its own line, after its parent item's text.
                if !self.out().is_empty() && !self.out().ends_with('\n') {
                    self.push("\n");
                }
                match start {
                    None => self.push("<ul>\n"),
                    Some(1) => self.push("<ol>\n"),
                    Some(n) => {
                        let _ = writeln!(self.out(), "<ol start=\"{n}\">");
                    }
                }
            }
            Tag::Item => self.push("<li>"),
            Tag::FootnoteDefinition(label) => {
                self.note_label = Some(label.to_string());
                self.note_depth += 1;
                self.bufs.push(String::new());
            }
            Tag::Table(_) | Tag::TableHead | Tag::TableRow | Tag::TableCell => {
                self.table_start(tag);
            }
            Tag::Emphasis => self.push("<em>"),
            Tag::Strong => self.push("<strong>"),
            Tag::Strikethrough => self.push("<del>"),
            Tag::Link { .. } | Tag::Image { .. } => self.link_start(tag),
            Tag::MetadataBlock(_) => self.metadata = true,
            // HTML blocks carry only Html events, which are dropped; the rest are not enabled.
            _ => {}
        }
        i
    }

    fn table_start(&mut self, tag: &Tag) {
        match tag {
            Tag::Table(align) => {
                self.close_label();
                self.table.align.clone_from(align);
                self.table.tbody_open = false;
                self.push("<div class=\"table\"><table>\n");
            }
            Tag::TableHead => {
                self.table.in_head = true;
                self.table.cell = 0;
                self.push("<thead>\n<tr>");
            }
            Tag::TableRow => {
                if !self.table.tbody_open {
                    self.table.tbody_open = true;
                    self.push("<tbody>\n");
                }
                self.table.cell = 0;
                self.push("<tr>");
            }
            Tag::TableCell => {
                let el = if self.table.in_head { "<th" } else { "<td" };
                self.push(el);
                // A data attribute, not `style`, so a strict CSP (admin preview) still applies.
                match self.table.align.get(self.table.cell) {
                    Some(Alignment::Left) => self.push(" data-align=\"left\""),
                    Some(Alignment::Center) => self.push(" data-align=\"center\""),
                    Some(Alignment::Right) => self.push(" data-align=\"right\""),
                    _ => {}
                }
                self.push(">");
            }
            _ => {}
        }
    }

    fn link_start(&mut self, tag: &Tag) {
        match tag {
            Tag::Link {
                link_type,
                dest_url,
                title,
                ..
            } => {
                let dest = if *link_type == LinkType::Email {
                    format!("mailto:{dest_url}")
                } else {
                    dest_url.to_string()
                };
                let safe = link::safe_url(&dest, false).map(str::to_owned);
                self.links.push(safe.is_some());
                if let Some(href) = safe {
                    self.push("<a href=\"");
                    self.esc(&href);
                    self.push("\"");
                    if !title.is_empty() {
                        self.push(" title=\"");
                        self.esc(title);
                        self.push("\"");
                    }
                    self.push(">");
                }
            }
            Tag::Image {
                dest_url, title, ..
            } => {
                let safe = link::safe_url(dest_url, true).is_some();
                if self.image.is_empty() {
                    self.alt.clear();
                }
                self.image
                    .push((dest_url.to_string(), title.to_string(), safe));
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        if !self.image.is_empty() && tag != TagEnd::Image {
            return;
        }
        match tag {
            TagEnd::Paragraph => {
                self.close_label();
                self.para_depth -= 1;
                if self.note_depth == 0 {
                    self.excerpt_src.push(' ');
                }
                if self.figure {
                    self.figure = false;
                } else {
                    self.push("</p>\n");
                }
            }
            TagEnd::Heading(level) => {
                let inner = self.bufs.pop().unwrap_or_default();
                let text = self.heading_text.take().unwrap_or_default();
                let id = self.unique_id(&text);
                let n = level as u8;
                let _ = writeln!(self.out(), "<h{n} id=\"{id}\">{inner}</h{n}>");
                if self.note_depth == 0 {
                    self.toc.push(TocEntry {
                        level: n,
                        id,
                        text: text.trim().to_owned(),
                    });
                }
            }
            TagEnd::BlockQuote(kind) => {
                self.close_label();
                self.push(if kind.is_some() {
                    "</div>\n"
                } else {
                    "</blockquote>\n"
                });
            }
            TagEnd::CodeBlock => {
                if self.code.take() == Some(CodeKind::Math) {
                    let src = std::mem::take(&mut self.math_src);
                    self.math(&src, true);
                    self.push("\n");
                } else {
                    self.push("</code></pre></div>\n");
                }
            }
            TagEnd::List(ordered) => {
                self.close_label();
                self.push(if ordered { "</ol>\n" } else { "</ul>\n" });
            }
            TagEnd::Item => {
                self.close_label();
                self.push("</li>\n");
            }
            TagEnd::FootnoteDefinition => {
                self.note_depth -= 1;
                let body = self.bufs.pop().unwrap_or_default();
                if let Some(label) = self.note_label.take() {
                    self.note_bodies.entry(label).or_insert(body);
                }
            }
            TagEnd::Table => {
                if self.table.tbody_open {
                    self.push("</tbody>\n");
                }
                self.push("</table></div>\n");
            }
            TagEnd::TableHead => {
                self.table.in_head = false;
                self.push("</tr>\n</thead>\n");
            }
            TagEnd::TableRow => self.push("</tr>\n"),
            TagEnd::TableCell => {
                self.push(if self.table.in_head { "</th>" } else { "</td>" });
                self.table.cell += 1;
            }
            TagEnd::Emphasis => self.push("</em>"),
            TagEnd::Strong => self.push("</strong>"),
            TagEnd::Strikethrough => self.push("</del>"),
            TagEnd::Link => {
                if self.links.pop() == Some(true) {
                    self.push("</a>");
                }
            }
            TagEnd::Image => self.image_end(),
            TagEnd::MetadataBlock(_) => self.metadata = false,
            _ => {}
        }
    }

    fn image_end(&mut self) {
        let Some((src, title, safe)) = self.image.pop() else {
            return;
        };
        if !self.image.is_empty() {
            // Image inside another image's alt text: only its alt survives.
            return;
        }
        let alt = std::mem::take(&mut self.alt);
        if !safe {
            // Unsafe source: keep what the author wrote about the image, as text.
            if self.figure {
                self.push("<p>");
            }
            self.esc(&alt);
            if self.figure {
                self.push("</p>\n");
            }
            return;
        }
        if self.figure {
            self.push("<figure>");
        }
        self.push("<img src=\"");
        self.esc(&src);
        self.push("\" alt=\"");
        self.esc(&alt);
        self.push("\" loading=\"lazy\" decoding=\"async\">");
        if self.figure {
            if !title.is_empty() {
                self.push("<figcaption>");
                self.esc(&title);
                self.push("</figcaption>");
            }
            self.push("</figure>\n");
        }
    }

    fn embed(&mut self, e: &Embed) {
        self.seen.embed = true;
        let _ = writeln!(
            self.out(),
            "<div class=\"embed\" data-provider=\"{}\" data-id=\"{}\"><a href=\"{}\">{}</a></div>",
            e.provider,
            e.id,
            e.url,
            e.url
        );
    }

    fn math(&mut self, tex: &str, display: bool) {
        use pulldown_latex::{Parser, RenderConfig, Storage, config::DisplayMode, push_mathml};
        self.seen.math = true;
        let storage = Storage::new();
        // Parse once to find errors: pulldown-latex would print a multi-line diagnostic into
        // the page. Its output is untrusted even without errors, see `mathml`.
        let safe_tex = mathml::neutralize(tex);
        if let Some(err) = Parser::new(&safe_tex, &storage).find_map(Result::err) {
            let first_line = err
                .to_string()
                .lines()
                .next()
                .unwrap_or_default()
                .to_owned();
            self.math_error(tex, &first_line);
            return;
        }
        let storage = Storage::new();
        let config = RenderConfig {
            display_mode: if display {
                DisplayMode::Block
            } else {
                DisplayMode::Inline
            },
            ..RenderConfig::default()
        };
        let mut raw = String::new();
        match push_mathml(&mut raw, Parser::new(&safe_tex, &storage), config) {
            Err(err) => self.math_error(tex, &err.to_string()),
            Ok(()) => match mathml::sanitize(&raw) {
                Some(safe) => self.push(&safe),
                None => self.math_error(tex, "markup outside the MathML allowlist"),
            },
        }
    }

    /// Readers see the TeX source; the author gets a warning to fix it.
    fn math_error(&mut self, tex: &str, detail: &str) {
        self.warnings.push(Warning {
            code: "markdown.math_invalid",
            detail: format!("{detail}: {tex}"),
        });
        self.push("<code class=\"math-error\">");
        self.esc(tex);
        self.push("</code>");
    }

    fn note_ref(&mut self, label: &str) {
        if !self.defined_notes.contains(label) {
            // Not a footnote after all: keep what the author typed.
            self.push("[^");
            self.esc(label);
            self.push("]");
            return;
        }
        let in_unused_note = self
            .note_label
            .as_ref()
            .is_some_and(|l| !self.note_order.contains(l));
        let Some(pos) = self.note_order.iter().position(|l| l == label) else {
            return;
        };
        if in_unused_note {
            // This body is discarded; counting its reference would shift the ids of real ones.
            return;
        }
        let n = pos + 1;
        let count = self.note_refs.entry(label.to_owned()).or_insert(0);
        *count += 1;
        let id = if *count == 1 {
            format!("fnref-{n}")
        } else {
            format!("fnref-{n}-{count}")
        };
        self.note_first_ref
            .entry(label.to_owned())
            .or_insert_with(|| id.clone());
        let _ = write!(
            self.out(),
            "<sup class=\"fnref\" id=\"{id}\"><a href=\"#fn-{n}\" role=\"doc-noteref\">{n}</a></sup>"
        );
    }

    /// Heading ids are unique and never collide with footnote ids.
    fn unique_id(&mut self, text: &str) -> String {
        let mut base = text::heading_id(text);
        if is_note_id(&base) {
            base.push_str("-h");
        }
        let mut id = base.clone();
        let mut n = 1;
        while !self.ids.insert(id.clone()) {
            n += 1;
            id = format!("{base}-{n}");
        }
        id
    }

    fn finish(mut self, options: Options) -> Rendered {
        let mut html = self.bufs.swap_remove(0);
        if !self.note_order.is_empty() {
            html.push_str("<section class=\"footnotes\" role=\"doc-endnotes\">\n<ol>\n");
            for (i, label) in self.note_order.iter().enumerate() {
                let n = i + 1;
                let body = self.note_bodies.remove(label).unwrap_or_default();
                let target = self
                    .note_first_ref
                    .get(label)
                    .cloned()
                    .unwrap_or_else(|| format!("fnref-{n}"));
                let back = format!(
                    " <a class=\"fnback\" href=\"#{target}\" role=\"doc-backlink\">&#8617;&#xFE0E;</a>"
                );
                let _ = write!(html, "<li id=\"fn-{n}\">");
                if let Some(head) = body.strip_suffix("</p>\n") {
                    let _ = writeln!(html, "{head}{back}</p>");
                } else {
                    let _ = writeln!(html, "{body}<p>{}</p>", back.trim_start());
                }
                html.push_str("</li>\n");
            }
            html.push_str("</ol>\n</section>\n");
        }
        Rendered {
            html,
            toc: self.toc,
            excerpt: text::excerpt(&self.excerpt_src, options.excerpt_words),
            reading_minutes: self.words.reading_minutes(),
            has_code: self.seen.code,
            has_math: self.seen.math,
            has_embed: self.seen.embed,
            warnings: self.warnings,
        }
    }
}

/// Defined note labels, and the notes to show in display order: notes referenced from the
/// post in order of first reference, then notes referenced only from those notes. A note
/// reachable from nowhere is not shown, and references inside it do not count.
fn note_numbering(events: &[Event]) -> (HashSet<String>, Vec<String>) {
    let mut defined = HashSet::new();
    let mut main_refs: Vec<String> = Vec::new();
    let mut nested: HashMap<String, Vec<String>> = HashMap::new();
    let mut current: Option<String> = None;
    for e in events {
        match e {
            Event::Start(Tag::FootnoteDefinition(l)) => {
                defined.insert(l.to_string());
                current = Some(l.to_string());
            }
            Event::End(TagEnd::FootnoteDefinition) => current = None,
            Event::FootnoteReference(l) => match &current {
                None => main_refs.push(l.to_string()),
                Some(c) => nested.entry(c.clone()).or_default().push(l.to_string()),
            },
            _ => {}
        }
    }
    let mut order: Vec<String> = Vec::new();
    let mut queue = main_refs;
    let mut i = 0;
    while i < queue.len() {
        let label = queue[i].clone();
        i += 1;
        if defined.contains(&label) && !order.contains(&label) {
            if let Some(children) = nested.get(&label) {
                queue.extend(children.iter().cloned());
            }
            order.push(label);
        }
    }
    (defined, order)
}

pub(crate) fn escape(out: &mut String, s: &str) {
    let mut last = 0;
    for (i, b) in s.bytes().enumerate() {
        let rep = match b {
            b'&' => "&amp;",
            b'<' => "&lt;",
            b'>' => "&gt;",
            b'"' => "&quot;",
            _ => continue,
        };
        out.push_str(&s[last..i]);
        out.push_str(rep);
        last = i + 1;
    }
    out.push_str(&s[last..]);
}

/// GitHub's alert syntax (`> [!NOTE]`), which AI agents already write.
fn callout_kind(k: BlockQuoteKind) -> &'static str {
    match k {
        BlockQuoteKind::Note => "note",
        BlockQuoteKind::Tip => "tip",
        BlockQuoteKind::Important => "important",
        BlockQuoteKind::Warning => "warning",
        BlockQuoteKind::Caution => "caution",
    }
}

/// First word of the fence info, limited to characters a language name uses, so it is safe
/// in an attribute and a CSS `attr()`.
fn code_lang(info: &str) -> String {
    info.split_whitespace()
        .next()
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '#' | '-' | '_' | '.'))
        .take(32)
        .collect::<String>()
        .to_ascii_lowercase()
}

fn is_note_id(id: &str) -> bool {
    ["fn-", "fnref-", "sn-"].iter().any(|p| {
        id.strip_prefix(p)
            .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
    })
}

/// A paragraph holding only one image becomes a figure; its title is the caption.
fn image_paragraph(events: &[Event], i: usize) -> bool {
    if !matches!(events.get(i + 1), Some(Event::Start(Tag::Image { .. }))) {
        return false;
    }
    let mut depth = 0;
    for (j, e) in events.iter().enumerate().skip(i + 1) {
        match e {
            Event::Start(Tag::Image { .. }) => depth += 1,
            Event::End(TagEnd::Image) => {
                depth -= 1;
                if depth == 0 {
                    return matches!(events.get(j + 1), Some(Event::End(TagEnd::Paragraph)));
                }
            }
            _ => {}
        }
    }
    false
}

/// A paragraph holding only a video URL (bare or `<url>`) becomes an embed facade.
/// Returns the embed and the index of the paragraph's end event.
fn embed_paragraph(events: &[Event], i: usize) -> Option<(Embed, usize)> {
    match events.get(i + 1..i + 5) {
        Some(
            [
                Event::Start(Tag::Link { dest_url, .. }),
                Event::Text(t),
                Event::End(TagEnd::Link),
                Event::End(TagEnd::Paragraph),
            ],
        ) if dest_url.as_ref() == t.as_ref() => return link::embed(t).map(|e| (e, i + 4)),
        _ => {}
    }
    match events.get(i + 1..i + 3) {
        Some([Event::Text(t), Event::End(TagEnd::Paragraph)]) => {
            link::embed(t.trim()).map(|e| (e, i + 2))
        }
        _ => None,
    }
}
