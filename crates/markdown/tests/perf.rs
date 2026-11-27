//! Cost of rendering on save. The Worker free plan allows 10 ms CPU per request, and a save
//! also parses JSON, checks the session and writes to D1. Wasm runs roughly 1.5-3x slower
//! than native, so a 5,000-word post with every feature must render natively in under 2 ms
//! (about 0.6 ms on a laptop): about 6 ms on the Worker in the worst case.
//!
//! Wall time on a shared CI runner is a poor clock, so `scripts/check` does not gate on that
//! budget. It gates on how the best time grows with the input (a 4x larger input must take
//! under 10x: linear work takes about 4x, quadratic 16x), plus a bound relative to a reference
//! workload measured in the same run (a serial hash of the same bytes), which catches a uniform slowdown. Work cannot be counted here: the workspace forbids `unsafe`, so no
//! counting allocator, and the renderer has no hook. The real budget is printed as `perf ...` lines
//! (`scripts/perf` shows them) and enforced when `PLUMBLOG_PERF_STRICT` is set, on a machine
//! that is not shared.
//! The ratio is enforced in the release run only (`scripts/check` runs this test with
//! `--release`); the debug workspace run keeps the scaling checks; debug builds are
//! 10-30x slower and would only measure the compiler.

use plumblog_markdown::{Options, render, render_comment};
use std::fmt::Write;
use std::time::{Duration, Instant};

/// The real budget: 2 ms natively for a 5,000-word post or a 5,000-character comment.
const BUDGET: Duration = Duration::from_millis(2);
/// A render may cost at most this many times the reference (FNV-1a over the same source, see
/// `measure`). Measured normal ratio: 0.9-1.15 on a laptop (also under load); K = 8 is
/// about 7x that, so a uniform 10x slowdown of the renderer fails, and a
/// slow or throttled machine, which slows the reference alike, does not. The scaling test
/// catches growth; this catches a constant factor.
const K: u32 = 8;

/// A serial hash of every byte, `passes` times over: a fixed workload that cannot regress and
/// shares nothing with the renderer. `passes` is chosen per input so the reference takes about
/// as long as the render does (a few microseconds would be timer noise).
fn reference(src: &[u8], passes: usize) -> u64 {
    (0..passes).fold(0xcbf2_9ce4_8422_2325_u64, |h, _| {
        src.iter()
            .fold(h, |h, &b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3))
    })
}

/// Best of 30 runs of `f` and of the reference over `src`, interleaved so both see the same
/// machine state; the minimum is the least noisy estimate of the real cost.
fn measure(src: &str, passes: usize, mut f: impl FnMut()) -> (Duration, Duration) {
    let (mut work, mut calibration) = (Duration::MAX, Duration::MAX);
    for _ in 0..30 {
        let t = Instant::now();
        f();
        work = work.min(t.elapsed());
        let t = Instant::now();
        std::hint::black_box(reference(std::hint::black_box(src.as_bytes()), passes));
        calibration = calibration.min(t.elapsed());
    }
    (work, calibration)
}

/// `big` is 4x the input of `small`: linear work takes about 4x the time, quadratic 16x; the
/// bound of 10 clears both with room. The floor keeps timer noise on tiny runs from deciding.
fn scales_linearly(what: &str, small: Duration, big: Duration) {
    eprintln!("{what}: 4x input, {small:?} -> {big:?}");
    let floor = Duration::from_micros(200);
    assert!(
        big < 10 * small.max(floor),
        "{what}: 4x input took {big:?} against {small:?}"
    );
}

/// The real CPU budget: printed for `scripts/perf`, enforced only when asked.
fn report(what: &str, (best, calibration): (Duration, Duration), budget: Duration) {
    eprintln!(
        "perf {what}: {best:?} (budget {budget:?}); reference {calibration:?}, ratio {:.2} (limit {K})",
        best.as_secs_f64() / calibration.as_secs_f64()
    );
    if std::env::var_os("PLUMBLOG_PERF_STRICT").is_some() {
        assert!(best <= budget, "{what}: {best:?} over budget {budget:?}");
    }
    // The debug workspace run (`cargo test --workspace`) also runs this file; there the
    // optimizer is off and the renderer slows more than the hash (ratio 3-5), so the ratio
    // only means something in the release run that `scripts/check` makes for this test.
    if cfg!(debug_assertions) {
        return;
    }
    assert!(
        best < K * calibration,
        "{what}: {best:?}, reference {calibration:?}: over {K}x"
    );
}

/// About 5,000 words using every feature the renderer knows.
fn long_post(sections: usize) -> String {
    let mut s = String::from("---\ntitle: Long\n---\n\n");
    for i in 0..sections {
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
fn long_post_renders_in_linear_work() {
    let src = long_post(26);
    let words = src.split_whitespace().count();
    assert!(
        (4_500..6_500).contains(&words),
        "test post has {words} words"
    );
    let first = render(&src, Options::default());
    assert!(first.has_code && first.has_math && first.has_embed && first.warnings.is_empty());

    let small = measure(&src, 16, || {
        assert_eq!(render(&src, Options::default()).html, first.html);
    });
    let big_src = long_post(104);
    let big = measure(&big_src, 16, || drop(render(&big_src, Options::default())));
    scales_linearly("post", small.0, big.0);
    report("markdown render, 5,000 words", small, BUDGET);
}

/// A comment is rendered once, at submit (stored as `body_html`), inside a request that has
/// 10 ms of CPU. A 5,000-character hostile body (the emphasis bomb is the worst case, about
/// 0.3 ms natively) gets the same budget; 4x that body must cost under 10x.
#[test]
fn hostile_comment_renders_in_linear_work() {
    let sized = |n: usize| {
        let unit = "*_[x](javascript:a) <b>t</b> https://a.example/ `c` > ";
        let body: String = unit.repeat(n / unit.len() + 1).chars().take(n).collect();
        let bomb = format!("{}x{}", "*_".repeat(n / 4), "_*".repeat(n / 4));
        [
            ("mixed", body),
            ("emphasis bomb", bomb),
            ("brackets", "[".repeat(n)),
        ]
    };
    for ((what, small_src), (_, big_src)) in sized(5_000).into_iter().zip(sized(20_000)) {
        let run = |src: &str| assert!(!render_comment(src).html.is_empty());
        let passes = if what == "emphasis bomb" { 48 } else { 16 };
        let small = measure(&small_src, passes, || run(&small_src));
        let big = measure(&big_src, passes, || run(&big_src));
        scales_linearly(&format!("comment, {what}"), small.0, big.0);
        report(&format!("comment {what}, 5,000 chars"), small, BUDGET);
    }
}
