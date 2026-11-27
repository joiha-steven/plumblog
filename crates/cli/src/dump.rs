//! plumblog's own SQLite text dump of the site's D1 database, read through the query API.
//!
//! D1's export endpoint refuses any database with a virtual table ("Export is not supported
//! for virtual tables, including databases with virtual tables",
//! <https://developers.cloudflare.com/d1/best-practices/import-export-data/>), and every
//! plumblog database has `posts_fts`. Why and how: docs/ARCHITECTURE.md (Site-specific state).
//!
//! The file is what D1 import runs: no `BEGIN`/`COMMIT`, `PRAGMA defer_foreign_keys` first,
//! tables parents before children, each table's rows as multi-row `INSERT`s under D1's
//! 100,000-byte statement limit, then indexes, views and triggers. Values are SQLite's own
//! literals (`quote()`), except TEXT holding a NUL byte, which `quote()` cuts at the NUL: that
//! is written as `CAST(X'..' AS TEXT)`. The last lines list the rows written per table, which
//! `blog restore` checks after the import.

use crate::cloudflare::{ApiError, ApiErrorKind, Endpoint};
use crate::d1::Database;
use crate::deploy::{Purpose, Site};
use crate::setup::{Failure, Run};
use crate::text::Code;
use plumblog_i18n::key;
use serde_json::{Map, Value};
use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::io::Write;
use std::path::Path;

/// D1's limit on one SQL statement (<https://developers.cloudflare.com/d1/platform/limits/>);
/// every statement in the file stays under it.
pub const MAX_STATEMENT: usize = 100_000;
/// SQL literal bytes one page of rows may carry before the rest waits for the next page; a
/// page is cut after the row that crosses it, so a reply holds at most this plus one row
/// (and one row over [`MAX_STATEMENT`] stops the dump). Small enough for any reply, large
/// enough that a blog's tables take few requests.
pub const PAGE_BYTES: u64 = 512 * 1024;
/// Rows a page reads at most; the next page asks for fewer when rows are large, so rows read
/// past the byte cut (and read again next page) stay few.
pub const MAX_PAGE_ROWS: u64 = 1000;
const FIRST_PAGE_ROWS: u64 = 100;
/// The last line of a complete dump.
pub const END: &str = "-- end of plumblog dump";
/// `-- rows "<table>" <count>`: rows written for one table.
pub const ROWS: &str = "-- rows ";
/// FTS5's shadow tables, `<virtual table>_<suffix>`: rebuilt from the virtual table's rows.
const SHADOW: &[&str] = &["data", "idx", "content", "docsize", "config"];
/// PRAGMA statements per batch request.
const BATCH: usize = 50;

/// Why a dump stopped.
#[derive(Debug)]
pub enum DumpError {
    Api(ApiError),
    /// One row is too large for a statement D1 import accepts.
    RowTooLarge {
        table: String,
        key: String,
        bytes: usize,
    },
    Write(std::io::Error),
}

impl From<ApiError> for DumpError {
    fn from(e: ApiError) -> Self {
        Self::Api(e)
    }
}

impl From<std::io::Error> for DumpError {
    fn from(e: std::io::Error) -> Self {
        Self::Write(e)
    }
}

fn malformed(detail: String) -> DumpError {
    DumpError::Api(ApiError {
        kind: ApiErrorKind::Malformed,
        endpoint: Some(Endpoint::D1Query),
        status: None,
        errors: Vec::new(),
        detail,
        tries: 1,
    })
}

/// A quoted SQL name.
#[must_use]
pub fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// `c` as an exact SQL literal, computed by SQLite, or a finite REAL as itself. `quote()`
/// stops at a NUL inside TEXT, so such a value goes out as its bytes in hex, cast back to
/// TEXT. `quote()` writes a REAL in at most 20 digits, which SQLite parses back exactly only
/// where it has a long double, so a finite REAL comes back as a JSON number (exact: D1 and
/// `float_roundtrip` both round-trip doubles) and [`real`] writes it. Infinities keep
/// `quote()`'s `9.0e+999`, which parses back to them.
fn literal(c: &str) -> String {
    format!(
        "CASE WHEN typeof({c}) = 'real' AND {c} - {c} = 0.0 THEN {c} \
         WHEN typeof({c}) = 'text' AND instr({c}, char(0)) > 0 \
         THEN 'CAST(X''' || hex({c}) || ''' AS TEXT)' ELSE quote({c}) END"
    )
}

/// A finite REAL as SQL that SQLite evaluates to exactly these bits on any platform: an
/// integer mantissa (exact below 2^53) scaled by powers of two (exact, as every step's result
/// is representable). Negative zero becomes zero: SQLite's text and JSON lose its sign too.
#[must_use]
pub fn real(value: f64) -> String {
    if value == 0.0 {
        return "0.0".into();
    }
    let bits = value.to_bits();
    let biased = i32::try_from((bits >> 52) & 0x7ff).unwrap_or(0);
    let frac = bits & ((1 << 52) - 1);
    let (mut mantissa, mut exp) = if biased == 0 {
        (frac, -1074)
    } else {
        (frac | (1 << 52), biased - 1075)
    };
    let zeros = mantissa.trailing_zeros();
    mantissa >>= zeros;
    exp += i32::try_from(zeros).unwrap_or(0);
    let sign = if value < 0.0 { "-" } else { "" };
    let mut sql = format!("CAST({sign}{mantissa} AS REAL)");
    if exp == 0 {
        return sql;
    }
    let op = if exp > 0 { " * " } else { " / " };
    let mut left = exp.unsigned_abs();
    while left > 0 {
        let step = left.min(62);
        let _ = write!(sql, "{op}{}", 1u64 << step);
        left -= step;
    }
    format!("({sql})")
}

/// A literal from a page: SQL text, or a finite REAL as a number.
fn sql_value(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => n.as_f64().map(real),
        _ => None,
    }
}

fn text(r: &Map<String, Value>, k: &str) -> String {
    r.get(k)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn is_virtual(sql: &str) -> bool {
    sql.trim_start()
        .get(..20)
        .is_some_and(|p| p.eq_ignore_ascii_case("CREATE VIRTUAL TABLE"))
}

/// `WITHOUT ROWID` comes after the column list's closing parenthesis.
fn without_rowid(sql: &str) -> bool {
    let tail = sql
        .rsplit(')')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let words: Vec<&str> = tail
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .filter(|w| !w.is_empty())
        .collect();
    words.windows(2).any(|w| w == ["WITHOUT", "ROWID"])
}

struct Table {
    name: String,
    sql: String,
    virt: bool,
    /// Columns inserted, in order (`rowid` first for a virtual table).
    cols: Vec<String>,
    /// Expressions the rows are ordered and paged by.
    keys: Vec<String>,
    parents: BTreeSet<String>,
}

/// Parents before children (by FOREIGN KEY), otherwise in the database's own order; a cycle
/// keeps that order (`defer_foreign_keys` checks only at the end of the import).
fn ordered(tables: Vec<Table>) -> Vec<Table> {
    let names: BTreeSet<String> = tables.iter().map(|t| t.name.clone()).collect();
    let mut left = tables;
    let mut done: BTreeSet<String> = BTreeSet::new();
    let mut out = Vec::new();
    while !left.is_empty() {
        let ready = left.iter().position(|t| {
            t.parents
                .iter()
                .all(|p| p == &t.name || done.contains(p) || !names.contains(p))
        });
        let t = left.remove(ready.unwrap_or(0));
        done.insert(t.name.clone());
        out.push(t);
    }
    out
}

/// One batch request per [`BATCH`] statements, results in order.
fn batched(db: &Database, sqls: &[String]) -> Result<Vec<Vec<Map<String, Value>>>, ApiError> {
    let mut out = Vec::new();
    for chunk in sqls.chunks(BATCH) {
        let stmts: Vec<(&str, &[&str])> = chunk.iter().map(|s| (s.as_str(), &[][..])).collect();
        out.extend(db.batch(&stmts)?.into_iter().map(|r| r.results));
    }
    Ok(out)
}

fn tables(db: &Database, objects: &[Map<String, Value>]) -> Result<Vec<Table>, DumpError> {
    let mut tables: Vec<Table> = objects
        .iter()
        .filter(|o| text(o, "type") == "table")
        .map(|o| Table {
            name: text(o, "name"),
            sql: text(o, "sql"),
            virt: is_virtual(&text(o, "sql")),
            cols: Vec::new(),
            keys: Vec::new(),
            parents: BTreeSet::new(),
        })
        .collect();
    let mut sqls = Vec::new();
    for t in &tables {
        sqls.push(format!("PRAGMA table_info({})", ident(&t.name)));
        sqls.push(format!("PRAGMA foreign_key_list({})", ident(&t.name)));
    }
    let replies = batched(db, &sqls)?;
    for (t, r) in tables.iter_mut().zip(replies.chunks(2)) {
        let mut info = r[0].clone();
        info.sort_by_key(|c| c.get("cid").and_then(Value::as_i64).unwrap_or(0));
        let cols: Vec<String> = info.iter().map(|c| text(c, "name")).collect();
        if cols.is_empty() || cols.iter().any(String::is_empty) {
            return Err(malformed(format!("no columns for table {}", t.name)));
        }
        let mut pk: Vec<(i64, String)> = info
            .iter()
            .filter_map(|c| {
                let n = c.get("pk").and_then(Value::as_i64).unwrap_or(0);
                (n > 0).then(|| (n, text(c, "name")))
            })
            .collect();
        pk.sort();
        let quoted: Vec<String> = cols.iter().map(|c| ident(c)).collect();
        if !t.virt && without_rowid(&t.sql) {
            if pk.is_empty() {
                return Err(malformed(format!("no primary key for table {}", t.name)));
            }
            t.keys = pk.iter().map(|(_, c)| ident(c)).collect();
            t.cols = quoted;
        } else {
            // The first rowid alias no column hides.
            let rowid = ["rowid", "_rowid_", "oid"]
                .into_iter()
                .find(|a| !cols.iter().any(|c| c.eq_ignore_ascii_case(a)))
                .ok_or_else(|| malformed(format!("no rowid name free in table {}", t.name)))?;
            t.keys = vec![rowid.to_string()];
            t.cols = quoted;
            if t.virt {
                // FTS rows keep their rowid: it is the post's id.
                t.cols.insert(0, rowid.to_string());
            }
        }
        t.parents = r[1].iter().map(|f| text(f, "table")).collect();
    }
    let (virt, plain): (Vec<Table>, Vec<Table>) = tables.into_iter().partition(|t| t.virt);
    let mut out = ordered(plain);
    out.extend(virt);
    Ok(out)
}

/// One page of `t` after `cursor` (key literals): `(keys, values)` literals per row, and
/// whether the table ends with this page.
type Page = (Vec<(Vec<String>, Vec<String>)>, bool);

fn page(db: &Database, t: &Table, cursor: Option<&[String]>, n: u64) -> Result<Page, DumpError> {
    let key_lits: Vec<String> = t
        .keys
        .iter()
        .enumerate()
        .map(|(i, k)| format!("{} AS _pb_k{i}", literal(k)))
        .collect();
    let raw_keys: Vec<String> = t
        .keys
        .iter()
        .enumerate()
        .map(|(i, k)| format!("{k} AS _pb_o{i}"))
        .collect();
    let vals: Vec<String> = t
        .cols
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{} AS _pb_v{i}", literal(c)))
        .collect();
    let size = (0..t.cols.len())
        .map(|i| format!("length(CAST(_pb_v{i} AS BLOB))"))
        .collect::<Vec<_>>()
        .join(" + ");
    let order_raw = t.keys.join(", ");
    let order_out = (0..t.keys.len())
        .map(|i| format!("_pb_o{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let pick: Vec<String> = (0..t.keys.len())
        .map(|i| format!("_pb_k{i}"))
        .chain((0..t.cols.len()).map(|i| format!("_pb_v{i}")))
        .collect();
    let filter = match cursor {
        None => String::new(),
        Some(c) if c.len() == 1 => format!("WHERE {} > {}", t.keys[0], c[0]),
        Some(c) => format!("WHERE ({}) > ({})", order_raw, c.join(", ")),
    };
    // The innermost query reads at most `n` rows in key order; the running sum of their
    // literal sizes cuts the page after the row that crosses PAGE_BYTES (the first row
    // always goes). Window functions run only over those `n` rows.
    let sql = format!(
        "SELECT {pick}, _pb_s, _pb_n FROM (\
           SELECT *, sum(_pb_s) OVER (ORDER BY {order_out} ROWS UNBOUNDED PRECEDING) AS _pb_run, \
             count(*) OVER () AS _pb_n FROM (\
             SELECT *, {size} AS _pb_s FROM (\
               SELECT {keys}, {raw}, {vals} FROM {table} {filter} ORDER BY {order_raw} LIMIT {n}))) \
         WHERE _pb_run - _pb_s < {budget} ORDER BY {order_out}",
        pick = pick.join(", "),
        keys = key_lits.join(", "),
        raw = raw_keys.join(", "),
        vals = vals.join(", "),
        table = ident(&t.name),
        budget = PAGE_BYTES,
    );
    let rows = db.rows(&sql, &[])?;
    let read = rows
        .first()
        .and_then(|r| r.get("_pb_n"))
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let get = |k: String| {
            r.get(&k)
                .and_then(sql_value)
                .ok_or_else(|| malformed(format!("{k} of table {} is not a literal", t.name)))
        };
        let keys = (0..t.keys.len())
            .map(|i| get(format!("_pb_k{i}")))
            .collect::<Result<Vec<_>, _>>()?;
        let vals = (0..t.cols.len())
            .map(|i| get(format!("_pb_v{i}")))
            .collect::<Result<Vec<_>, _>>()?;
        out.push((keys, vals));
    }
    let end = out.is_empty() || (out.len() as u64 == read && read < n);
    Ok((out, end))
}

/// A key as shown in an error: short and printable.
fn shown_key(keys: &[String]) -> String {
    let s = keys.join(", ");
    let mut short: String = s.chars().take(80).collect();
    if short.len() < s.len() {
        short.push('\u{2026}');
    }
    short
        .chars()
        .map(|c| if c.is_control() { '?' } else { c })
        .collect()
}

/// Rows of `t` as `INSERT`s, each under [`MAX_STATEMENT`] bytes; returns the rows written.
fn rows(db: &Database, t: &Table, out: &mut dyn Write) -> Result<u64, DumpError> {
    let prefix = format!(
        "INSERT INTO {} ({}) VALUES\n",
        ident(&t.name),
        t.cols.join(", ")
    );
    let mut stmt = String::new();
    let mut count = 0u64;
    let mut cursor: Option<Vec<String>> = None;
    let mut n = FIRST_PAGE_ROWS;
    loop {
        let (rows, end) = page(db, t, cursor.as_deref(), n)?;
        let got = rows.len() as u64;
        for (keys, vals) in rows {
            let row = format!("({})", vals.join(","));
            if prefix.len() + row.len() + 1 >= MAX_STATEMENT {
                return Err(DumpError::RowTooLarge {
                    table: t.name.clone(),
                    key: shown_key(&keys),
                    bytes: prefix.len() + row.len() + 1,
                });
            }
            // `,\n` before the row and `;` after it.
            if !stmt.is_empty() && stmt.len() + 2 + row.len() + 1 >= MAX_STATEMENT {
                stmt.push_str(";\n");
                out.write_all(stmt.as_bytes())?;
                stmt.clear();
            }
            if stmt.is_empty() {
                stmt.push_str(&prefix);
            } else {
                stmt.push_str(",\n");
            }
            stmt.push_str(&row);
            count += 1;
            if cursor.as_ref() == Some(&keys) {
                return Err(malformed(format!("table {} repeats a key", t.name)));
            }
            cursor = Some(keys);
        }
        if end {
            break;
        }
        // Cut by bytes: ask for about as many rows as fitted; else grow towards the cap.
        n = if got < n {
            got.max(1)
        } else {
            (n * 2).min(MAX_PAGE_ROWS)
        };
    }
    if !stmt.is_empty() {
        stmt.push_str(";\n");
        out.write_all(stmt.as_bytes())?;
    }
    Ok(count)
}

/// Writes the dump of `db` to `out`; `bookmark` is the Time Travel bookmark taken just
/// before, recorded in the header. Returns `(table, rows)` in the order written.
///
/// # Errors
/// [`DumpError`]: the API, a row too large for D1 import, or `out`.
pub fn write(
    db: &Database,
    bookmark: &str,
    out: &mut dyn Write,
) -> Result<Vec<(String, u64)>, DumpError> {
    let objects = db.rows(
        "SELECT type, name, tbl_name, sql FROM sqlite_master WHERE sql IS NOT NULL \
         AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' AND name NOT LIKE '\\_cf\\_%' ESCAPE '\\' \
         ORDER BY rowid",
        &[],
    )?;
    let virtuals: Vec<String> = objects
        .iter()
        .filter(|o| text(o, "type") == "table" && is_virtual(&text(o, "sql")))
        .map(|o| text(o, "name"))
        .collect();
    let shadows: BTreeSet<String> = virtuals
        .iter()
        .flat_map(|v| SHADOW.iter().map(move |s| format!("{v}_{s}")))
        .collect();
    let objects: Vec<Map<String, Value>> = objects
        .into_iter()
        .filter(|o| !shadows.contains(&text(o, "name")) && !shadows.contains(&text(o, "tbl_name")))
        .collect();

    let mut head = String::new();
    let _ = writeln!(head, "-- plumblog database dump, format 1");
    let _ = writeln!(
        head,
        "-- Time Travel bookmark taken just before: {bookmark}"
    );
    let _ = writeln!(
        head,
        "-- Read page by page while the site ran, so not one instant: for the exact state at \
         the bookmark, restore it with Time Travel."
    );
    head.push_str("PRAGMA defer_foreign_keys = true;\n");
    out.write_all(head.as_bytes())?;

    let mut counts = Vec::new();
    for t in tables(db, &objects)? {
        out.write_all(format!("{};\n", t.sql).as_bytes())?;
        counts.push((t.name.clone(), rows(db, &t, out)?));
    }
    let by_type: HashMap<String, Vec<&Map<String, Value>>> =
        objects.iter().fold(HashMap::new(), |mut m, o| {
            m.entry(text(o, "type")).or_default().push(o);
            m
        });
    for ty in ["index", "view", "trigger"] {
        for o in by_type.get(ty).into_iter().flatten() {
            out.write_all(format!("{};\n", text(o, "sql")).as_bytes())?;
        }
    }
    let mut tail = String::new();
    for (name, n) in &counts {
        let _ = writeln!(tail, "{ROWS}{} {n}", ident(name));
    }
    tail.push_str(END);
    tail.push('\n');
    out.write_all(tail.as_bytes())?;
    Ok(counts)
}

/// Creates `path` (0600, its directory 0700), failing if it exists.
fn create(path: &Path) -> std::io::Result<fs::File> {
    if let Some(dir) = path.parent() {
        let mut b = fs::DirBuilder::new();
        b.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            b.mode(0o700);
        }
        b.create(dir)?;
    }
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

/// The dump into `<dir>/<stamp>-<db>.sql` (0600), `dir` relative to the site folder or
/// absolute, through `<file>.blog-part` and a rename, so a file under the final name is
/// always complete. Returns the file as shown to the owner.
pub(crate) fn export(
    run: &mut Run,
    site: &Site,
    db: &Database,
    bookmark: &str,
    stamp: &str,
    dir: &str,
    purpose: Purpose,
) -> Result<String, Failure> {
    let deploy = purpose == Purpose::Deploy;
    if deploy {
        run.line(key!("cli.deploy.exporting"), &[]);
    } else {
        run.line(key!("cli.backup.exporting"), &[("dir", dir.to_string())]);
    }
    let rel = format!("{dir}/{stamp}-{}.sql", site.db_name);
    let path = run.cx.dir.join(&rel);
    let part = run.cx.dir.join(format!("{rel}.blog-part"));
    let text = run.cx.text;
    let unwritable = |p: &Path, reason: String| {
        Failure::new(
            Code::BackupUnwritable,
            text,
            if deploy {
                key!("cli.error.backup_unwritable")
            } else {
                key!("cli.error.backup_file_unwritable")
            },
            &[("path", p.display().to_string()), ("reason", reason)],
        )
    };
    if path.exists() {
        return Err(unwritable(&path, "the file exists".into()));
    }
    // A part left by a run that was killed is ours to replace.
    let _ = fs::remove_file(&part);
    let file = create(&part).map_err(|e| unwritable(&part, e.to_string()))?;
    let mut w = std::io::BufWriter::new(file);
    let written = write(db, bookmark, &mut w).and_then(|counts| {
        let f = w
            .into_inner()
            .map_err(std::io::IntoInnerError::into_error)?;
        f.sync_all()?;
        Ok(counts)
    });
    let counts = match written {
        Ok(c) => c,
        Err(e) => {
            let _ = fs::remove_file(&part);
            return Err(match e {
                DumpError::Api(e) => run.api(&e),
                DumpError::Write(e) => unwritable(&part, e.to_string()),
                DumpError::RowTooLarge { table, key, bytes } => Failure::new(
                    Code::DumpRowTooLarge,
                    text,
                    if deploy {
                        key!("cli.error.dump_row_too_large")
                    } else {
                        key!("cli.error.backup_row_too_large")
                    },
                    &[
                        ("table", table),
                        ("key", key),
                        ("bytes", bytes.to_string()),
                        ("limit", MAX_STATEMENT.to_string()),
                    ],
                ),
            });
        }
    };
    if path.exists() {
        let _ = fs::remove_file(&part);
        return Err(unwritable(&path, "the file exists".into()));
    }
    fs::rename(&part, &path).map_err(|e| {
        let _ = fs::remove_file(&part);
        unwritable(&path, e.to_string())
    })?;
    run.line(
        if deploy {
            key!("cli.deploy.exported")
        } else {
            key!("cli.backup.exported")
        },
        &[
            ("file", rel.clone()),
            ("tables", counts.len().to_string()),
            ("rows", counts.iter().map(|c| c.1).sum::<u64>().to_string()),
        ],
    );
    Ok(rel)
}

/// The row counts a complete dump ends with, `None` when the end marker or a count line is
/// missing or unreadable (a cut file, or not ours).
#[must_use]
pub fn counts(text: &str) -> Option<Vec<(String, u64)>> {
    let mut lines = text.lines().rev().skip_while(|l| l.trim().is_empty());
    if lines.next()? != END {
        return None;
    }
    let mut out: Vec<(String, u64)> = Vec::new();
    for l in lines {
        let Some(rest) = l.strip_prefix(ROWS) else {
            break;
        };
        let rest = rest.strip_prefix('"')?;
        let (name, n) = rest.rsplit_once("\" ")?;
        out.push((name.replace("\"\"", "\""), n.parse().ok()?));
    }
    out.reverse();
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn without_rowid_is_read_after_the_columns() {
        assert!(without_rowid(
            "CREATE TABLE a (x PRIMARY KEY) WITHOUT ROWID"
        ));
        assert!(without_rowid(
            "CREATE TABLE a (x PRIMARY KEY)\n) without  rowid, STRICT"
        ));
        assert!(!without_rowid(
            "CREATE TABLE a (x TEXT DEFAULT 'WITHOUT ROWID')"
        ));
    }

    #[test]
    fn reals_are_a_mantissa_scaled_by_powers_of_two() {
        assert_eq!(real(0.5), "(CAST(1 AS REAL) / 2)");
        assert_eq!(real(-3.0), "CAST(-3 AS REAL)");
        assert_eq!(real(1024.0), "(CAST(1 AS REAL) * 1024)");
        assert_eq!(
            real(0.1),
            "(CAST(3602879701896397 AS REAL) / 36028797018963968)"
        );
        assert_eq!(real(-0.0), "0.0");
        assert_eq!(real(5e-324).matches(" / ").count(), 18);
    }

    #[test]
    fn counts_need_the_end_marker() {
        let t = format!("x;\n{ROWS}\"a\"\"b\" 3\n{ROWS}\"c\" 0\n{END}\n");
        assert_eq!(counts(&t), Some(vec![("a\"b".into(), 3), ("c".into(), 0)]));
        assert_eq!(counts(&t.replace(END, "")), None);
        assert_eq!(counts(&t.replace(" 3\n", " x\n")), None);
    }
}
