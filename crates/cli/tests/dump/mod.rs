//! The database dump `blog backup` and `blog deploy` write, checked in real SQLite: a round
//! trip into an empty database (FTS included), value escaping, table order, paging, statement
//! size, and restore's row-count check.

use super::{GOOD, TempDir, client};
use crate::deploy::{FakeClock, make_bundle};
use crate::fake_cf::{Fake, import_sql};
use crate::setup::{ALL, ok as setup_ok, run_with as setup_with, site};
use plumblog_cli::backup::{self, Options};
use plumblog_cli::dump::{MAX_STATEMENT, PAGE_BYTES};
use plumblog_cli::migrations;
use plumblog_cli::restore;
use plumblog_cli::setup::{Context, Failure as RunFailure, Options as SetupOptions};
use plumblog_cli::state;
use plumblog_cli::text::{Code, Text};
use rusqlite::Connection;
use rusqlite::types::{Value as Sql, ValueRef};
use std::collections::{BTreeMap, HashSet};

const NOW: i64 = 1_790_000_000_000;
const DB: &str = "db-1";
const FILE: &str = "backups/20260921T141320Z-plumblog.sql";

fn en() -> Text {
    Text::new("en")
}

fn set_up() -> (Fake, TempDir) {
    let fake = Fake::start(site(ALL));
    let dir = TempDir::new();
    setup_ok(&setup_with(
        &fake,
        &dir,
        &SetupOptions::default(),
        &migrations::bundled(),
        &en(),
    ));
    (fake, dir)
}

/// `blog backup --no-media`; the dump's text, or the failure and the output.
fn backup_db(fake: &Fake, dir: &TempDir) -> Result<String, (RunFailure, String)> {
    let mut c = client(fake, GOOD);
    let migs = migrations::bundled();
    let text = en();
    let cx = Context {
        dir: &dir.0,
        migrations: &migs,
        now_ms: NOW,
        text: &text,
    };
    let mut buf = Vec::new();
    let opts = Options {
        no_media: true,
        out: None,
    };
    match backup::run(&mut c, None, &opts, &cx, &mut buf) {
        Ok(()) => Ok(std::fs::read_to_string(dir.0.join(FILE)).unwrap()),
        Err(e) => Err((e, String::from_utf8(buf).unwrap())),
    }
}

fn dumped(fake: &Fake, dir: &TempDir) -> String {
    backup_db(fake, dir).unwrap_or_else(|(e, out)| panic!("{:?}: {}\n{out}", e.code, e.message))
}

/// Realistic rows in every kind of table: parents and children, WITHOUT ROWID tables, the FTS
/// table with Vietnamese text, a self-referencing comment thread.
fn seed(fake: &Fake) {
    fake.exec(
        DB,
        "INSERT INTO users (id, login, slug, display_name, role, created_at, updated_at) VALUES \
           (1, 'owner', 'owner', 'Tr\u{1ea7}n M\u{1ea1}nh H\u{f9}ng', 'owner', 1, 2), \
           (2, 'writer', 'writer', 'Ng\u{1b0}\u{1edd}i vi\u{1ebf}t \"O''Brien\"', 'writer', 3, 4);\
         INSERT INTO media (id, r2_key, mime, bytes, width, height, alt, variants_json, \
           uploaded_by, created_at) VALUES \
           (1, '2026/11/pho.png', 'image/png', 1234, 640, 480, 'B\u{e1}t ph\u{1edf} b\u{f2}', \
            '[{\"key\":\"2026/11/pho-320.webp\",\"width\":320,\"height\":240,\"bytes\":99,\
              \"mime\":\"image/webp\"}]', 1, 5), \
           (2, '2026/11/hue.jpg', 'image/jpeg', 99, 10, 10, '', '[]', NULL, 6);\
         INSERT INTO tags (id, slug, name) VALUES (1, 'am-thuc', '\u{1ea8}m th\u{1ef1}c'), (2, 'du-lich', 'Du l\u{1ecb}ch');\
         INSERT INTO series (id, slug, title) VALUES (1, 'mien-trung', 'Mi\u{1ec1}n Trung');\
         INSERT INTO posts (id, kind, translation_group, lang, slug, status, published_at, \
           author_id, title, markdown, html, cover_media_id, series_id, series_position, \
           created_at, updated_at) VALUES \
           (1, 'post', 1, 'vi', 'pho-bo', 'published', 10, 1, 'Ph\u{1edf} b\u{f2} H\u{e0} N\u{1ed9}i', \
            'N\u{1b0}\u{1edb}c d\u{f9}ng *trong*,\nb\u{e1}nh ph\u{1edf} m\u{1ec1}m; ''qu\u{1ea9}y'' gi\u{f2}n.', '<p>N\u{1b0}\u{1edb}c d\u{f9}ng</p>', 1, 1, 1, 10, 11), \
           (2, 'post', 1, 'en', 'beef-pho', 'published', 12, 2, 'Hanoi beef pho', \
            'Clear broth.\r\nSoft noodles.', '<p>Clear broth.</p>', 1, NULL, NULL, 12, 13), \
           (3, 'page', 3, 'vi', 'gioi-thieu', 'draft', NULL, 1, 'Gi\u{1edb}i thi\u{1ec7}u', 'Xin ch\u{e0}o', \
            '<p>Xin ch\u{e0}o</p>', NULL, NULL, NULL, 14, 15);\
         INSERT INTO posts_fts (rowid, title, body) VALUES \
           (1, 'Ph\u{1edf} b\u{f2} H\u{e0} N\u{1ed9}i', 'N\u{1b0}\u{1edb}c d\u{f9}ng trong, b\u{e1}nh ph\u{1edf} m\u{1ec1}m; qu\u{1ea9}y gi\u{f2}n.'), \
           (2, 'Hanoi beef pho', 'Clear broth. Soft noodles.'), \
           (3, 'Gi\u{1edb}i thi\u{1ec7}u', 'Xin ch\u{e0}o');\
         INSERT INTO post_tags (post_id, tag_id) VALUES (1, 1), (1, 2), (2, 1);\
         INSERT INTO post_media (post_id, media_id) VALUES (1, 1), (1, 2), (2, 1);\
         INSERT INTO comments (id, post_id, parent_id, author_name, body, status, created_at) \
           VALUES (1, 1, NULL, 'Lan', 'Ngon qu\u{e1}!', 'approved', 20), \
                  (2, 1, 1, 'Minh', '\u{110}\u{1ed3}ng \u{fd} \u{1f44d}', 'approved', 21), \
                  (3, 2, NULL, 'Spam', 'buy now', 'spam', 22);\
         INSERT INTO redirects (from_path, to_path, created_at) VALUES ('/old', '/vi/pho-bo', 30);\
         INSERT INTO settings (key, value_json, updated_at) VALUES \
           ('title', '\"B\u{1ebf}p nh\u{e0} H\u{f9}ng\"', 40), ('nav', '[{\"label\":\"Trang ch\u{1ee7}\",\"url\":\"/\"}]', 41);",
    );
}

/// Table name (or `(schema)`) -> its rows as [`exact`] values, sorted.
type Snapshot = BTreeMap<String, Vec<String>>;

/// A value exactly, type included: `''`, `X''` and NULL differ, a REAL keeps its bits.
fn exact(v: ValueRef) -> String {
    match v {
        ValueRef::Null => "N".into(),
        ValueRef::Integer(i) => format!("I{i}"),
        ValueRef::Real(f) => format!("R{:016x}", f.to_bits()),
        ValueRef::Text(t) => format!("T{t:?}"),
        ValueRef::Blob(b) => format!("B{b:?}"),
    }
}

/// Every object of the schema, and every row of every table the owner's data lives in (the
/// FTS table through its own rows, not its shadow tables), sorted.
fn snapshot(conn: &Connection) -> Snapshot {
    let mut out = BTreeMap::new();
    let mut schema: Vec<String> = conn
        .prepare(
            "SELECT type, name, tbl_name, sql FROM sqlite_master \
             WHERE name NOT LIKE 'sqlite\\_%' ESCAPE '\\'",
        )
        .unwrap()
        .query_map([], |r| {
            Ok(format!(
                "{}|{}|{}|{}",
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?.unwrap_or_default()
            ))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect();
    schema.sort();
    out.insert("(schema)".into(), schema);
    let tables: Vec<(String, String)> = conn
        .prepare(
            "SELECT name, sql FROM sqlite_master WHERE type = 'table' \
             AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' AND name NOT LIKE 'posts\\_fts\\_%' ESCAPE '\\'",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for (name, sql) in tables {
        let select = if sql.starts_with("CREATE VIRTUAL TABLE") {
            format!("SELECT rowid, * FROM \"{name}\"")
        } else {
            format!("SELECT * FROM \"{name}\"")
        };
        let mut q = conn.prepare(&select).unwrap();
        let n = q.column_count();
        let mut rows = q.query([]).unwrap();
        let mut all = Vec::new();
        while let Some(row) = rows.next().unwrap() {
            let vals: Vec<String> = (0..n).map(|i| exact(row.get_ref(i).unwrap())).collect();
            all.push(vals.join(","));
        }
        all.sort();
        out.insert(name, all);
    }
    out
}

fn live(fake: &Fake) -> Snapshot {
    snapshot(&fake.state().sqlite[DB])
}

/// A new, empty database, foreign keys enforced as D1 does, with `sql` imported the way the
/// fake's D1 import runs a file.
fn import_into_empty(sql: &str) -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
    import_sql(&conn, sql).unwrap_or_else(|e| panic!("import failed: {e}"));
    conn
}

fn fts_hits(conn: &Connection, q: &str) -> Vec<i64> {
    conn.prepare("SELECT rowid FROM posts_fts WHERE posts_fts MATCH ? ORDER BY rowid")
        .unwrap()
        .query_map([q], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn a_dump_restores_into_an_empty_database_row_for_row() {
    let (fake, dir) = set_up();
    seed(&fake);
    let before = live(&fake);
    let hits_before: Vec<Vec<i64>> = ["pho", "ph\u{1edf}", "nuoc dung", "broth"]
        .iter()
        .map(|q| fts_hits(&fake.state().sqlite[DB], q))
        .collect();
    assert_eq!(hits_before[0], [1, 2], "the seed is searchable");
    let text = dumped(&fake, &dir);
    // The backup run itself writes `last_backup` after the dump: compare without it.
    let restored = import_into_empty(&text);
    let mut after = snapshot(&restored);
    let mut before = before;
    for snap in [&mut before, &mut after] {
        snap.get_mut("settings")
            .unwrap()
            .retain(|r| !r.contains("last_backup"));
    }
    assert_eq!(before, after);
    assert!(!after["posts_fts"].is_empty() && !after["d1_migrations"].is_empty());
    let hits_after: Vec<Vec<i64>> = ["pho", "ph\u{1edf}", "nuoc dung", "broth"]
        .iter()
        .map(|q| fts_hits(&restored, q))
        .collect();
    assert_eq!(hits_before, hits_after);
}

/// The live database and the dump imported into an empty one, `last_backup` (written by the
/// backup run after the dump) left out of both.
fn round_trip(fake: &Fake, dir: &TempDir) -> (Snapshot, Snapshot, String) {
    let mut before = live(fake);
    let text = dumped(fake, dir);
    let mut after = snapshot(&import_into_empty(&text));
    for snap in [&mut before, &mut after] {
        snap.get_mut("settings")
            .unwrap()
            .retain(|r| !r.contains("last_backup"));
    }
    (before, after, text)
}

/// Fixed-seed xorshift: the same values on every run, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n as u64).unwrap()
    }
}

const PIECES: &[&str] = &[
    "'",
    "\"",
    "\\",
    "\n",
    "\r",
    "\0",
    "\u{1f642}",
    "\u{103}",
    "Ph\u{1edf}",
    "x",
    ";",
    "--",
    "/*",
    "*/",
    "X'00'",
    "NULL",
    "\u{feff}",
    "\t",
    "''",
    "\u{7f}",
    "char(0)",
];

const REALS: &[f64] = &[
    0.1,
    -0.0,
    1e308,
    -1e-308,
    5e-324,
    f64::MAX,
    f64::MIN_POSITIVE,
    1.0 / 3.0,
    f64::INFINITY,
    f64::NEG_INFINITY,
    123_456_789.0,
    -2.5,
];

fn any_text(r: &mut Rng) -> String {
    (0..r.below(24))
        .map(|_| PIECES[r.below(PIECES.len())])
        .collect()
}

fn any_value(r: &mut Rng) -> Sql {
    match r.below(10) {
        0 => Sql::Null,
        1 => Sql::Text(String::new()),
        2 | 3 => Sql::Text(any_text(r)),
        4 => Sql::Blob(
            (0..r.below(48))
                .map(|_| r.next().to_le_bytes()[0])
                .collect(),
        ),
        5 => Sql::Blob(Vec::new()),
        6 => Sql::Integer([i64::MIN, i64::MAX, 0, -1, 1][r.below(5)]),
        7 => Sql::Integer(r.next().cast_signed()),
        8 => Sql::Real(REALS[r.below(REALS.len())]),
        _ => {
            let f = f64::from_bits(r.next());
            Sql::Real(if f.is_nan() { 0.5 } else { f })
        }
    }
}

#[test]
fn every_kind_of_value_survives_the_round_trip() {
    let (fake, dir) = set_up();
    {
        let st = fake.state();
        let conn = &st.sqlite[DB];
        // No declared types: SQLite keeps each value's own type.
        conn.execute_batch(
            "CREATE TABLE prop (id INTEGER PRIMARY KEY, a, b, c);\
             CREATE TABLE propk (k PRIMARY KEY, v) WITHOUT ROWID;",
        )
        .unwrap();
        let mut r = Rng(0x5eed_1234_abcd_0042);
        for id in 0..400 {
            let vals = [any_value(&mut r), any_value(&mut r), any_value(&mut r)];
            conn.execute(
                "INSERT INTO prop (id, a, b, c) VALUES (?, ?, ?, ?)",
                rusqlite::params![id, vals[0], vals[1], vals[2]],
            )
            .unwrap();
        }
        // Long values: quotes doubled, a NUL forcing the hex form.
        let long = [
            "'".repeat(30_000) + "\u{e9}",
            "ab'\0\n".repeat(8_000),
            "Ph\u{1edf} \u{1f642} ".repeat(3_000),
        ];
        for (i, t) in long.iter().enumerate() {
            conn.execute(
                "INSERT INTO prop (id, a) VALUES (?, ?)",
                rusqlite::params![1000 + i64::try_from(i).unwrap(), t],
            )
            .unwrap();
        }
        // Keys of every type, NUL inside text keys too: the page cursor is a literal.
        let mut seen = HashSet::new();
        while seen.len() < 300 {
            let k = any_value(&mut r);
            if matches!(k, Sql::Null) {
                continue;
            }
            let n = conn
                .execute(
                    "INSERT OR IGNORE INTO propk (k, v) VALUES (?, ?)",
                    rusqlite::params![k, any_value(&mut r)],
                )
                .unwrap();
            seen.insert(format!("{k:?}{n}"));
        }
    }
    let (mut before, after, text) = round_trip(&fake, &dir);
    // The one documented loss: SQLite's text and D1's JSON drop the sign of a zero REAL.
    for rows in before.values_mut() {
        for r in rows.iter_mut() {
            *r = r.replace("R8000000000000000", "R0000000000000000");
        }
        rows.sort();
    }
    assert_eq!(before["prop"].len(), 403);
    assert!(before["propk"].len() > 250);
    let diff = |x: &[String], y: &[String]| -> Vec<String> {
        x.iter()
            .filter(|r| !y.contains(r))
            .map(|r| r.chars().take(200).collect())
            .collect()
    };
    assert!(
        before["prop"] == after["prop"],
        "lost: {:?}\ngained: {:?}",
        diff(&before["prop"], &after["prop"]),
        diff(&after["prop"], &before["prop"])
    );
    assert_eq!(before["propk"], after["propk"]);
    assert_eq!(before, after);
    // quote() cuts TEXT at a NUL, so such values are written as their bytes.
    assert!(text.contains("CAST(X'"), "hex form used");
}

fn pos(text: &str, needle: &str) -> usize {
    text.find(needle)
        .unwrap_or_else(|| panic!("{needle} not in the dump"))
}

#[test]
fn parents_come_before_children_and_triggers_after_the_rows() {
    let (fake, dir) = set_up();
    seed(&fake);
    // The child is created first and sorts first: only the foreign key puts it second.
    fake.exec(
        DB,
        "CREATE TABLE a_child (id INTEGER PRIMARY KEY, \
           parent_id INTEGER NOT NULL REFERENCES z_parent (id));\
         CREATE TABLE z_parent (id INTEGER PRIMARY KEY, name TEXT NOT NULL);\
         INSERT INTO z_parent VALUES (1, 'm\u{1ed9}t'), (2, 'hai');\
         INSERT INTO a_child VALUES (1, 2), (2, 1), (3, 2);\
         CREATE VIEW parent_names AS SELECT name FROM z_parent;\
         CREATE TRIGGER child_added AFTER INSERT ON a_child BEGIN \
           UPDATE z_parent SET name = name || '!' WHERE id = new.parent_id; END;",
    );
    let (before, after, text) = round_trip(&fake, &dir);
    assert!(pos(&text, "CREATE TABLE z_parent") < pos(&text, "CREATE TABLE a_child"));
    assert!(pos(&text, "CREATE TABLE users") < pos(&text, "CREATE TABLE posts"));
    assert!(pos(&text, "CREATE TABLE posts") < pos(&text, "CREATE TABLE post_media"));
    assert!(pos(&text, "CREATE TABLE media") < pos(&text, "CREATE TABLE post_media"));
    assert!(pos(&text, "INSERT INTO \"posts_fts\"") < pos(&text, "\nCREATE INDEX"));
    assert!(pos(&text, "\nCREATE INDEX") < pos(&text, "\nCREATE VIEW"));
    assert!(pos(&text, "\nCREATE VIEW") < pos(&text, "\nCREATE TRIGGER"));
    assert!(text.starts_with("-- plumblog database dump"));
    assert_eq!(
        text.lines().find(|l| !l.starts_with("--")),
        Some("PRAGMA defer_foreign_keys = true;")
    );
    assert!(!text.contains("BEGIN TRANSACTION") && !text.contains("\nCOMMIT"));
    // The trigger ran on no restored row.
    assert_eq!(before, after);
    // Without deferred checks, foreign keys still hold statement by statement.
    let strict = text.replacen("PRAGMA defer_foreign_keys = true;\n", "", 1);
    let conn = import_into_empty(&strict);
    let broken: i64 = conn
        .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(broken, 0);
}

fn page_queries(fake: &Fake, table: &str) -> Vec<(String, usize)> {
    let st = fake.state();
    let needle = format!("FROM \\\"{table}\\\"");
    st.requests
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            r.path.ends_with("/query") && r.body.contains(&needle) && r.body.contains("_pb_s")
        })
        .map(|(i, r)| {
            let bytes = st
                .reply_bytes
                .iter()
                .find(|(n, _)| *n == i + 1)
                .map_or(0, |(_, b)| *b);
            (r.body.clone(), bytes)
        })
        .collect()
}

#[test]
fn without_rowid_tables_page_by_their_primary_key() {
    let (fake, dir) = set_up();
    {
        let st = fake.state();
        let conn = &st.sqlite[DB];
        conn.execute_batch(
            "CREATE TABLE wr (a TEXT COLLATE NOCASE, b INTEGER, v TEXT, PRIMARY KEY (a, b)) \
             WITHOUT ROWID",
        )
        .unwrap();
        for i in 0..2500 {
            let a = format!("{}{}", ["x", "Y", "\u{103}", "Z"][i % 4], i % 37);
            conn.execute(
                "INSERT INTO wr VALUES (?, ?, ?)",
                rusqlite::params![a, i64::try_from(i).unwrap(), format!("v{i}")],
            )
            .unwrap();
        }
    }
    let (before, after, _) = round_trip(&fake, &dir);
    assert_eq!(before["wr"].len(), 2500);
    assert_eq!(before["wr"], after["wr"]);
    let pages = page_queries(&fake, "wr");
    assert!(pages.len() >= 3, "{} pages", pages.len());
    assert!(
        pages[1].0.contains("WHERE (\\\"a\\\", \\\"b\\\") > ("),
        "{}",
        pages[1].0
    );
}

#[test]
fn a_page_stays_within_its_byte_bound() {
    let (fake, dir) = set_up();
    let row = "z".repeat(3_000);
    {
        let st = fake.state();
        let conn = &st.sqlite[DB];
        conn.execute_batch("CREATE TABLE big (id INTEGER PRIMARY KEY, t TEXT)")
            .unwrap();
        for i in 0..1500 {
            conn.execute("INSERT INTO big VALUES (?, ?)", rusqlite::params![i, row])
                .unwrap();
        }
    }
    let (before, after, _) = round_trip(&fake, &dir);
    assert_eq!(before["big"], after["big"]);
    let pages = page_queries(&fake, "big");
    let bound = usize::try_from(PAGE_BYTES).unwrap() + 2 * row.len() + 64 * 1024;
    let largest = pages.iter().map(|p| p.1).max().unwrap();
    assert!(pages.len() >= 9, "{} pages for 4.5 MB", pages.len());
    assert!(
        largest <= bound,
        "a reply of {largest} bytes, bound {bound}"
    );
    assert!(
        largest > bound / 2,
        "pages are cut by bytes, not rows: {largest}"
    );
}

#[test]
fn inserts_are_split_under_the_statement_limit() {
    let (fake, dir) = set_up();
    {
        let st = fake.state();
        let conn = &st.sqlite[DB];
        conn.execute_batch("CREATE TABLE wide (id INTEGER PRIMARY KEY, t TEXT)")
            .unwrap();
        for i in 0..10 {
            conn.execute(
                "INSERT INTO wide VALUES (?, ?)",
                rusqlite::params![i, "y".repeat(30_000)],
            )
            .unwrap();
        }
    }
    let (before, after, text) = round_trip(&fake, &dir);
    assert_eq!(before["wide"], after["wide"]);
    assert!(text.matches("INSERT INTO \"wide\"").count() >= 4);
    // No row of this database holds ";\n", so it splits the file into statements.
    for st in text.split(";\n") {
        assert!(st.len() + 1 < MAX_STATEMENT, "{} bytes", st.len() + 1);
    }
}

#[test]
fn a_row_too_large_for_d1_import_stops_with_no_file() {
    let (fake, dir) = set_up();
    fake.exec(
        DB,
        "CREATE TABLE wide (id INTEGER PRIMARY KEY, t TEXT);\
         INSERT INTO wide VALUES (1, 'ok'), (7, printf('%.120000c', 'w')), (8, 'never read');",
    );
    let (e, out) = backup_db(&fake, &dir).unwrap_err();
    assert_eq!(e.code, Code::DumpRowTooLarge, "{out}");
    assert_eq!(e.code.as_str(), "backup.row_too_large");
    assert!(
        e.message.contains("wide") && e.message.contains("Row 7 "),
        "{}",
        e.message
    );
    let files: Vec<String> = std::fs::read_dir(dir.0.join("backups"))
        .map(|d| {
            d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    assert!(files.is_empty(), "{files:?}");
}

fn restore_file(fake: &Fake, dir: &TempDir) -> Result<String, (RunFailure, String)> {
    let bundle = make_bundle(dir);
    let mut c = client(fake, GOOD);
    let migs = migrations::bundled();
    let text = en();
    let cx = Context {
        dir: &dir.0,
        migrations: &migs,
        now_ms: NOW + 60_000,
        text: &text,
    };
    let opts = restore::Options {
        file: FILE.into(),
        yes: true,
        bundle: Some(bundle),
        ..restore::Options::default()
    };
    let mut buf = Vec::new();
    let r = restore::run(&mut c, None, &opts, &cx, &FakeClock::default(), &mut buf);
    let out = String::from_utf8(buf).unwrap();
    r.map(|()| out.clone()).map_err(|e| (e, out))
}

#[test]
fn blog_restore_puts_a_dump_back_and_checks_every_table() {
    let (fake, dir) = set_up();
    seed(&fake);
    dumped(&fake, &dir);
    fake.exec(
        DB,
        "DELETE FROM post_tags WHERE post_id = 2; DELETE FROM post_media WHERE post_id = 2; \
         DELETE FROM comments WHERE post_id = 2; DELETE FROM posts WHERE id = 2; \
         DELETE FROM posts_fts WHERE rowid = 2;",
    );
    let out = restore_file(&fake, &dir)
        .unwrap_or_else(|(e, out)| panic!("{:?}: {}\n{out}", e.code, e.message));
    assert!(out.contains("Checked: all "), "{out}");
    assert_eq!(fts_hits(&fake.state().sqlite[DB], "broth"), [2]);
}

#[test]
fn a_row_lost_in_the_import_stops_with_the_undo_command() {
    let (fake, dir) = set_up();
    seed(&fake);
    dumped(&fake, &dir);
    fake.state().after_import = Some("DELETE FROM post_tags WHERE post_id = 2".into());
    let (e, out) = restore_file(&fake, &dir).unwrap_err();
    assert_eq!(e.code, Code::RestoreCountMismatch, "{out}");
    assert_eq!(e.code.as_str(), "restore.count_mismatch");
    let st = state::load(&dir.0).unwrap().unwrap();
    let rec = st.rollbacks.last().unwrap();
    let undo = format!(
        "blog rollback --db {} --yes",
        rec.previous.as_deref().unwrap()
    );
    assert!(e.message.contains(&undo), "{}", e.message);
    assert!(e.message.contains("post_tags 3 -> 2"), "{}", e.message);
    // Printed before the import too.
    assert!(out.contains(&undo), "{out}");
    assert_eq!(rec.outcome.as_deref(), Some("count_mismatch"));
}
