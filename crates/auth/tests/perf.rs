//! Cost of the strict JSON reader. A writer may POST 160 KiB to `/api/v1/posts`, and the
//! Worker free plan allows 10 ms CPU per request; Wasm runs roughly 1.5-3x slower than native.
//! Every 160 KiB document, accepted or refused, must parse natively in under 1 ms (about
//! 0.25 ms on a laptop).
//!
//! Wall time on a shared CI runner is a poor clock, so `scripts/check` does not gate on that
//! budget. It gates on how the best time grows with the input (a 4x smaller document must
//! cost at least 1/10 as much: linear work takes about 1/4, quadratic 1/16), on a refused
//! document costing about what an accepted one does, and on an absolute bound far above
//! normal. Work cannot be counted here: the workspace forbids `unsafe`, so no counting
//! allocator, and the parser has no hook. The real budget is printed as `perf ...` lines
//! (`scripts/perf` shows them) and enforced when `PLUMBLOG_PERF_STRICT` is set, on a machine
//! that is not shared.
//! Release builds only (`scripts/check` runs this test with `--release`); debug builds are
//! 10-30x slower and would only measure the compiler.

use plumblog_auth::json::{MAX_DEPTH, MAX_ITEMS, MAX_KEY, MAX_VALUES, parse_document};
use plumblog_model::ErrorCode;
use plumblog_model::api::MAX_POST_BODY;
use std::fmt::Write;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The real budget.
const BUDGET: Duration = Duration::from_millis(1);
/// Far above normal (0.25 ms): only a blowup gets here, on any runner. Per-key duplicate scans
/// once made the 22,000-key document take ~20 ms.
const BACKSTOP: Duration = Duration::from_millis(10);

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
    // Objects of MAX_ITEMS members with the longest distinct keys that fit: every member
    // hashed for the duplicate check, a key allocation each.
    let key_len = (body - 1024) / budget_values - 5;
    assert!(key_len <= MAX_KEY);
    let mut members = Vec::new();
    let mut objects = Vec::new();
    for i in 0..budget_values - budget_values / MAX_ITEMS - 1 {
        let k = name(i);
        // `_` ends the name, so padded keys stay distinct.
        members.push(format!("\"{k}_{}\":0", "x".repeat(key_len - k.len() - 1)));
        if members.len() == MAX_ITEMS {
            objects.push(format!("{{{}}}", members.join(",")));
            members.clear();
        }
    }
    objects.push(format!("{{{}}}", members.join(",")));
    let keys = deep(&objects.join(","));
    // Integers of 19 digits: the slowest number path.
    let int = "-9223372036854775807";
    let all = vec![int; budget_values - budget_values / MAX_ITEMS - 1];
    let arrays: Vec<String> = all
        .chunks(MAX_ITEMS)
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

/// Best of 50 runs: the minimum is the least noisy estimate of the real cost.
fn best(src: &str) -> Duration {
    let _one_at_a_time = TIMING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    (0..50)
        .map(|_| {
            let t = Instant::now();
            let r = parse_document(src.as_bytes(), MAX_POST_BODY);
            let d = t.elapsed();
            drop(r);
            d
        })
        .min()
        .unwrap()
}

/// Prints the real budget, enforces it only when asked, and always enforces the backstop.
fn report(what: &str, bytes: usize, d: Duration) {
    eprintln!("perf json {what}: {bytes} bytes in {d:?} (budget {BUDGET:?})");
    if std::env::var_os("PLUMBLOG_PERF_STRICT").is_some() {
        assert!(d <= BUDGET, "{what}: parse took {d:?}, budget {BUDGET:?}");
    }
    assert!(d <= BACKSTOP, "{what}: parse took {d:?}, far above normal");
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
    let refused = best(&src);
    report("22 x 1000 keys, refused", src.len(), refused);
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
        let (small, full) = (best(&quarter), best(&src));
        report(what, src.len(), full);
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
