//! The admin's generated types must equal what the Rust types emit today.

use std::path::Path;

#[test]
fn admin_types_match_the_rust_types() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/admin/src/api/model.gen.ts");
    let want = plumblog_model::ts::admin_types();
    if std::env::var_os("BLESS").is_some() {
        std::fs::write(&path, &want).unwrap();
    }
    let have = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        have == want,
        "{} is stale: run `BLESS=1 cargo test -p plumblog-model --test ts`",
        path.display()
    );
}
