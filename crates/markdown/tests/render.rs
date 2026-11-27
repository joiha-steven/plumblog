//! What `render` returns besides HTML: TOC, excerpt, reading time, feature flags, warnings.

use plumblog_markdown::{Options, TocEntry, render};

#[test]
fn toc_lists_headings_with_their_ids_but_not_footnote_headings() {
    let r = render(
        "# A\n\n## B *b*\n\n## B b\n\nText[^n].\n\n[^n]: # Not in toc\n",
        Options::default(),
    );
    let toc: Vec<_> = r
        .toc
        .iter()
        .map(|t| (t.level, t.id.as_str(), t.text.as_str()))
        .collect();
    assert_eq!(toc, [(1, "a", "A"), (2, "b-b", "B b"), (2, "b-b-2", "B b")]);
    assert_eq!(
        r.toc[0],
        TocEntry {
            level: 1,
            id: "a".into(),
            text: "A".into()
        }
    );
}

#[test]
fn excerpt_takes_paragraph_text_only() {
    let src = "## Heading words\n\nFirst *para* with `code`.\n\n```\nnot this\n```\n\n\
               Second para[^1] here.\n\n[^1]: Footnote text.\n";
    let r = render(src, Options { excerpt_words: 5 });
    assert_eq!(r.excerpt, "First para with code. Second\u{2026}");
    let all = render(src, Options { excerpt_words: 100 });
    assert_eq!(all.excerpt, "First para with code. Second para here.");
}

#[test]
fn flags_and_reading_time() {
    let empty = render("", Options::default());
    assert_eq!(empty.reading_minutes, 0);
    assert!(!empty.has_code && !empty.has_math && !empty.has_embed);
    assert!(empty.html.is_empty() && empty.excerpt.is_empty() && empty.toc.is_empty());

    let r = render(
        "```rs\nx\n```\n\n$x$\n\nhttps://youtu.be/dQw4w9WgXcQ\n",
        Options::default(),
    );
    assert!(r.has_code && r.has_math && r.has_embed);
    assert_eq!(r.reading_minutes, 1);

    let long = render(&"word ".repeat(1000), Options::default());
    assert_eq!(long.reading_minutes, 5);
}

#[test]
fn invalid_math_is_a_warning_not_a_failure() {
    let r = render("Bad $\\notacommand$ here.", Options::default());
    assert_eq!(r.warnings.len(), 1);
    assert_eq!(r.warnings[0].code, "markdown.math_invalid");
    assert!(
        r.html
            .contains("<code class=\"math-error\">\\notacommand</code>")
    );
    assert!(
        render("Fine $x^2$.", Options::default())
            .warnings
            .is_empty()
    );
}
