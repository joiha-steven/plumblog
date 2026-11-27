//! CPU budget for the strict JSON reader. A writer may POST 160 KiB to `/api/v1/posts`, and the
//! Worker free plan allows 10 ms CPU per request; Wasm runs roughly 1.5-3x slower than native.
//! The worst shape for duplicate-key detection is many objects of ~1000 short distinct keys:
//! a per-key scan of the object made that ~20 ms natively. Parsing it with no duplicate check
//! at all takes ~0.9 ms (one allocation per key), so the budget is 2 ms, as for markdown: about
//! 6 ms on the Worker in the worst case, and still 10x under the quadratic cost.
//! The budget is only enforced in release builds (`scripts/check` runs this test with
//! `--release`); debug builds are 10-30x slower and would only measure the compiler.

use plumblog_auth::json::{MAX_ITEMS, parse_document};
use plumblog_model::api::MAX_POST_BODY;
use std::fmt::Write;
use std::time::{Duration, Instant};

const BUDGET: Duration = Duration::from_millis(2);

/// `{"o0":{"a":0,"b":0,...},"o1":{...},...}`: as many 1000-key objects as fit the post cap.
fn wide_objects() -> String {
    let mut one = String::from("{");
    for i in 0..MAX_ITEMS {
        if i > 0 {
            one.push(',');
        }
        // Base-26 keys: 1-3 letters, all distinct.
        let mut key = String::new();
        let mut n = i;
        loop {
            key.insert(0, char::from(b'a' + u8::try_from(n % 26).unwrap()));
            n /= 26;
            if n == 0 {
                break;
            }
            n -= 1;
        }
        let _ = write!(one, "\"{key}\":0");
    }
    one.push('}');
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

#[test]
fn wide_objects_parse_within_cpu_budget() {
    let src = wide_objects();
    assert!(src.len() > MAX_POST_BODY - 16 * 1024, "{} bytes", src.len());
    let first = parse_document(src.as_bytes(), MAX_POST_BODY).expect("valid document");
    assert!(first.len() >= 15, "{} objects", first.len());

    // Best of several runs: the minimum is the least noisy estimate of the real cost.
    let best = (0..50)
        .map(|_| {
            let t = Instant::now();
            let r = parse_document(src.as_bytes(), MAX_POST_BODY).unwrap();
            let d = t.elapsed();
            assert_eq!(r.len(), first.len());
            d
        })
        .min()
        .unwrap();
    eprintln!(
        "json: {} bytes, {} objects of {MAX_ITEMS} keys in {best:?} (budget {BUDGET:?})",
        src.len(),
        first.len()
    );
    if cfg!(debug_assertions) {
        return;
    }
    assert!(best <= BUDGET, "parse took {best:?}, budget {BUDGET:?}");
}
