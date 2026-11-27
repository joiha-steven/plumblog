//! CPU budget for rendering on save. The Worker free plan allows 10 ms CPU per request, and
//! a save also parses JSON, checks the session and writes to D1. Wasm runs roughly 1.5-3x
//! slower than native, and Cloudflare cores may be slower than a developer laptop, so a
//! 5,000-word post with every feature must render natively in under 2 ms: about 6 ms on the
//! Worker in the worst case, leaving room for the rest of the request.
//! The budget is only enforced in release builds (`scripts/check` runs this test with
//! `--release`); debug builds are 10-30x slower and would only measure the compiler.

use plumblog_markdown::{Options, render, render_comment};
use std::fmt::Write;
use std::time::{Duration, Instant};

const BUDGET: Duration = Duration::from_millis(2);

/// About 5,000 words using every feature the renderer knows.
fn long_post() -> String {
    let mut s = String::from("---\ntitle: Long\n---\n\n");
    for i in 0..26 {
        let _ = write!(
            s,
            "## Section {i}\n\n\
             A paragraph with *emphasis*, **strong**, `code`, ~~struck~~, a [link](https://example.com/{i}) \
             and https://example.org/bare-{i} plus a note[^n{i}]. Ti\u{1ebf}ng Vi\u{1ec7}t c\u{f3} d\u{1ea5}u, \
             \u{440}\u{443}\u{441}\u{441}\u{43a}\u{438}\u{439} \u{442}\u{435}\u{43a}\u{441}\u{442}, \u{4e2d}\u{6587}. \
             Inline math $a_{i} + b^2 = \\frac{{c}}{{d}}$ and more words to read through here.\n\n\
             {}\n\n\
             - [x] done\n- [ ] open\n  - nested item\n\n\
             | a | b |\n|:--|--:|\n| 1 | 2 |\n\n\
             > [!NOTE]\n> A callout with **bold**.\n\n\
             ![Image {i}](/media/{i}.avif \"Caption {i}\")\n\n\
             ```rust\nfn f{i}() -> u32 {{ {i} }}\n```\n\n\
             $$\n\\sum_{{k=1}}^{{n}} k^2\n$$\n\n\
             https://youtu.be/dQw4w9WgXcQ\n\n\
             <b>raw</b> html <script>x</script>\n\n\
             [^n{i}]: Footnote {i} with a [link](/x).\n\n",
            "Plain prose to fill the section with realistic reading material. ".repeat(10),
        );
    }
    s
}

#[test]
fn long_post_renders_within_cpu_budget() {
    let src = long_post();
    let words = src.split_whitespace().count();
    assert!(
        (4_500..6_500).contains(&words),
        "test post has {words} words"
    );
    let first = render(&src, Options::default());
    assert!(first.has_code && first.has_math && first.has_embed && first.warnings.is_empty());

    // Best of several runs: the minimum is the least noisy estimate of the real cost.
    let best = (0..20)
        .map(|_| {
            let t = Instant::now();
            let r = render(&src, Options::default());
            let d = t.elapsed();
            assert_eq!(r.html.len(), first.html.len());
            d
        })
        .min()
        .unwrap();
    eprintln!("render: {words} words in {best:?} (budget {BUDGET:?})");
    if cfg!(debug_assertions) {
        return;
    }
    assert!(best <= BUDGET, "render took {best:?}, budget {BUDGET:?}");
}

/// A comment is rendered once, at submit (stored as `body_html`), inside a request that has
/// 10 ms of CPU. A 5,000-character hostile body (the emphasis bomb is the worst case, about
/// 0.3 ms natively, 0.66 ms on CI runners) gets the long-post budget: far below the request
/// limit, and still fails on quadratic growth.
const COMMENT_BUDGET: Duration = Duration::from_millis(2);

#[test]
fn hostile_comment_renders_within_cpu_budget() {
    let unit = "*_[x](javascript:a) <b>t</b> https://a.example/ `c` > ";
    let body: String = unit
        .repeat(5000 / unit.len() + 1)
        .chars()
        .take(5000)
        .collect();
    let bomb: String = format!("{}x{}", "*_".repeat(1250), "_*".repeat(1250));
    for src in [body, bomb, "[".repeat(5000)] {
        let best = (0..20)
            .map(|_| {
                let t = Instant::now();
                let r = render_comment(&src);
                let d = t.elapsed();
                assert!(!r.html.is_empty());
                d
            })
            .min()
            .unwrap();
        eprintln!(
            "comment: {} chars in {best:?} (budget {COMMENT_BUDGET:?})",
            src.chars().count()
        );
        if !cfg!(debug_assertions) {
            assert!(best <= COMMENT_BUDGET, "comment took {best:?}");
        }
    }
}
