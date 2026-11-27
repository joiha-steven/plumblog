//! Byte-exact fixture corpus: every `fixtures/<case>.md` must render to `<case>.html`.
//! After an intended output change, run with `BLESS=1` and review the diff of the `.html` files.

use plumblog_markdown::{Options, render};
use std::fs;
use std::path::Path;

#[test]
fn fixtures_render_byte_exact() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let bless = std::env::var_os("BLESS").is_some();
    let mut cases: Vec<_> = fs::read_dir(&dir)
        .expect("tests/fixtures exists")
        .map(|e| e.expect("readable dir entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "md"))
        .collect();
    cases.sort();
    assert!(
        cases.len() >= 15,
        "fixture corpus shrank: {} cases",
        cases.len()
    );
    let mut failed = Vec::new();
    for md in &cases {
        let src = fs::read_to_string(md).expect("fixture is UTF-8");
        let got = render(&src, Options::default()).html;
        let want_path = md.with_extension("html");
        if bless {
            fs::write(&want_path, &got).expect("write fixture");
            continue;
        }
        let want = fs::read_to_string(&want_path).unwrap_or_default();
        if got != want {
            failed.push(md.file_stem().unwrap().to_string_lossy().into_owned());
            eprintln!(
                "--- {} expected\n{want}\n--- got\n{got}",
                want_path.display()
            );
        }
    }
    assert!(
        failed.is_empty(),
        "fixtures differ: {failed:?} (BLESS=1 to accept)"
    );
}
