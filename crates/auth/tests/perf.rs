//! Cost of the strict JSON reader. A writer may POST 160 KiB to `/api/v1/posts`, and the
//! Worker free plan allows 10 ms CPU per request; Wasm runs roughly 1.5-3x slower than native.
//! Every 160 KiB document, accepted or refused, must parse natively in under 1 ms (about
//! 0.25 ms on a laptop).
//!
//! Wall time on a shared CI runner is a poor clock, so `scripts/check` does not gate on that
//! budget. It gates on how the best time grows with the input (a 4x smaller document must
//! cost at least 1/10 as much: linear work takes about 1/4, quadratic 1/16), on a refused
//! document costing about what an accepted one does, and on a bound relative to a reference workload measured in the same run (a
//! serial hash of the same bytes), so a slow runner does not trip it. Work cannot be counted here: the workspace forbids `unsafe`, so no counting
//! allocator, and the parser has no hook. The real budget is printed as `perf ...` lines
//! (`scripts/perf` shows them) and enforced when `PLUMBLOG_PERF_STRICT` is set, on a machine
//! that is not shared.
//! The ratio is enforced in the release run only (`scripts/check` runs this test with
//! `--release`); the debug workspace run keeps the scaling checks, since a debug build is
//! 10-30x slower and its ratio would only measure the compiler.

use plumblog_auth::json::{MAX_DEPTH, MAX_ITEMS, MAX_KEY, MAX_VALUES, parse_document};
use plumblog_model::ErrorCode;
use plumblog_model::api::MAX_POST_BODY;
use std::fmt::Write;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The real budget.
const BUDGET: Duration = Duration::from_millis(1);
/// A parse may cost at most this many times the reference (FNV-1a over the same bytes, see
/// `measure`). Measured normal ratio: 1.1-1.4 (laptop, also under load); K = 8 is
/// about 6x that, so a uniform 10x slowdown of the parser fails and a slow or throttled machine, which
/// slows the reference alike, does not. The ratio test below catches growth (a scan quadratic in
/// an object's width); this catches a constant factor.
const K: u32 = 8;

/// Timings run one at a time: tests in one binary run in parallel threads, and two timed loops
/// sharing a core measured ~30% slower.
static TIMING: Mutex<()> = Mutex::new(());

/// Distinct base-26 names: `a`..`z`, `aa`...
fn name(mut n: usize) -> String {
    let mut key = String::new();
    loop {
        key.insert(0, char::from(b'a' + u8::try_from(n % 26).unwrap()));
        n /= 26;
        if n == 0 {
            return key;
        }
        n -= 1;
    }
}

/// `{"o0":{"a":0,...},"o1":{...},...}`: as many 1000-key objects as fit the post cap, 22,000
/// members in all. Per-key duplicate scans made this ~20 ms; it is now refused by the value cap.
fn wide_objects() -> String {
    let one = format!(
        "{{{}}}",
        (0..MAX_ITEMS)
            .map(|i| format!("\"{}\":0", name(i)))
            .collect::<Vec<_>>()
            .join(",")
    );
    let mut s = String::from("{");
    let mut i = 0;
    while s.len() + one.len() + 16 < MAX_POST_BODY {
        if i > 0 {
            s.push(',');
        }
        let _ = write!(s, "\"o{i}\":{one}");
        i += 1;
    }
    s.push('}');
    s
}

/// The costliest documents still accepted, each close to 160 KiB with exactly `MAX_VALUES`
/// values (or one long string), all at the deepest nesting allowed. `div` shrinks the size and
/// the value count alike, to compare a document with a smaller one of the same shape.
fn worst_accepted(div: usize) -> Vec<(&'static str, String)> {
    let body = MAX_POST_BODY / div;
    // Wraps `inner` so it sits at depth MAX_DEPTH; adds MAX_DEPTH - 2 values.
    let deep = |inner: &str| {
        let n = MAX_DEPTH - 2;
        format!("{{\"d\":{}{inner}{}}}", "[".repeat(n), "]".repeat(n))
    };
    let budget_values = MAX_VALUES / div - (MAX_DEPTH - 1);
    // Objects shrink with the document (width, not just count), so a scan that is quadratic in
    // one object's width costs 16x, not 4x, when the document is 4x.
    let width = MAX_ITEMS / div;
    // Objects of `width` members with the longest distinct keys that fit: every member
    // hashed for the duplicate check, a key allocation each.
    let key_len = (body - 1024) / budget_values - 5;
    assert!(key_len <= MAX_KEY);
    let mut members = Vec::new();
    let mut objects = Vec::new();
    for i in 0..budget_values - budget_values / width - 1 {
        let k = name(i);
        // `_` ends the name, so padded keys stay distinct.
        members.push(format!("\"{k}_{}\":0", "x".repeat(key_len - k.len() - 1)));
        if members.len() == width {
            objects.push(format!("{{{}}}", members.join(",")));
            members.clear();
        }
    }
    objects.push(format!("{{{}}}", members.join(",")));
    let keys = deep(&objects.join(","));
    // Integers of 19 digits: the slowest number path.
    let int = "-9223372036854775807";
    let all = vec![int; budget_values - budget_values / width - 1];
    let arrays: Vec<String> = all
        .chunks(width)
        .map(|c| format!("[{}]", c.join(",")))
        .collect();
    let ints = deep(&arrays.join(","));
    // One string of escapes (surrogate pairs): the byte-at-a-time path.
    let esc = r"\ud83d\ude00";
    let escapes = deep(&format!("\"{}\"", esc.repeat((body - 256) / esc.len())));
    vec![
        ("long keys", keys),
        ("integers", ints),
        ("escapes", escapes),
    ]
}

/// A serial hash of every byte: a fixed workload of the parser's size and rough cost that
/// cannot regress and shares nothing with the parser.
fn reference(src: &[u8]) -> u64 {
    src.iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3)
    })
}

/// Best of 50 runs of the parse and of the reference, interleaved so both see the same machine
/// state; the minimum is the least noisy estimate of the real cost.
fn measure(src: &str) -> (Duration, Duration) {
    let _one_at_a_time = TIMING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (mut parse, mut calibration) = (Duration::MAX, Duration::MAX);
    for _ in 0..50 {
        let t = Instant::now();
        let r = parse_document(src.as_bytes(), MAX_POST_BODY);
        parse = parse.min(t.elapsed());
        drop(r);
        let t = Instant::now();
        std::hint::black_box(reference(std::hint::black_box(src.as_bytes())));
        calibration = calibration.min(t.elapsed());
    }
    (parse, calibration)
}

fn best(src: &str) -> Duration {
    measure(src).0
}

/// Prints the real budget, enforces it only when asked, and always enforces the backstop.
fn report(what: &str, bytes: usize, (d, calibration): (Duration, Duration)) {
    eprintln!(
        "perf json {what}: {bytes} bytes in {d:?} (budget {BUDGET:?}); reference {calibration:?}, ratio {:.2} (limit {K})",
        d.as_secs_f64() / calibration.as_secs_f64()
    );
    if std::env::var_os("PLUMBLOG_PERF_STRICT").is_some() {
        assert!(d <= BUDGET, "{what}: parse took {d:?}, budget {BUDGET:?}");
    }
    // The debug workspace run (`cargo test --workspace`) also runs this file; there the
    // optimizer is off and the parser slows far more than the hash (ratio 5-7), so the ratio
    // only means something in the release run that `scripts/check` makes for this test.
    if cfg!(debug_assertions) {
        return;
    }
    assert!(
        d < K * calibration,
        "{what}: parse took {d:?}, reference {calibration:?}: over {K}x"
    );
}

/// Below this, timer noise decides a ratio.
const FLOOR: Duration = Duration::from_micros(30);

#[test]
fn many_wide_objects_are_refused_cheaply() {
    let src = wide_objects();
    assert!(src.len() > MAX_POST_BODY - 16 * 1024, "{} bytes", src.len());
    let err = match parse_document(src.as_bytes(), MAX_POST_BODY) {
        Ok(doc) => panic!(
            "accepted ({} members at the top); want the value cap",
            doc.len()
        ),
        Err(e) => e,
    };
    assert_eq!(err.code, ErrorCode::TooComplex, "{}", err.detail);
    let measured = measure(&src);
    let refused = measured.0;
    report("22 x 1000 keys, refused", src.len(), measured);
    // Refusing at the value cap must cost about what parsing a full document does, measured
    // on the same machine in the same run.
    let baseline = worst_accepted(1)
        .iter()
        .map(|(_, s)| best(s))
        .max()
        .unwrap();
    assert!(
        refused < 10 * baseline.max(FLOOR),
        "refusing took {refused:?}, a full accepted document {baseline:?}"
    );
}

#[test]
fn worst_accepted_documents_parse_in_linear_time() {
    for ((what, src), (_, quarter)) in worst_accepted(1).into_iter().zip(worst_accepted(4)) {
        assert!(src.len() <= MAX_POST_BODY, "{what}: {} bytes", src.len());
        assert!(
            src.len() > MAX_POST_BODY / 2,
            "{what}: {} bytes is not a worst case",
            src.len()
        );
        parse_document(src.as_bytes(), MAX_POST_BODY).unwrap_or_else(|e| panic!("{what}: {e:?}"));
        parse_document(quarter.as_bytes(), MAX_POST_BODY)
            .unwrap_or_else(|e| panic!("{what} (quarter): {e:?}"));
        let small = best(&quarter);
        let measured = measure(&src);
        let full = measured.0;
        report(what, src.len(), measured);
        eprintln!(
            "json {what}: {} bytes {small:?} -> {} bytes {full:?}",
            quarter.len(),
            src.len()
        );
        // 4x the document: linear takes about 4x the time, quadratic 16x.
        assert!(
            full < 10 * small.max(FLOOR),
            "{what}: 4x the document took {full:?} against {small:?}"
        );
    }
}
