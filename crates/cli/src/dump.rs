//! plumblog's own SQLite text dump of the site's D1 database, read through the query API.
//!
//! D1's export endpoint refuses any database with a virtual table ("Export is not supported
//! for virtual tables, including databases with virtual tables",
//! <https://developers.cloudflare.com/d1/best-practices/import-export-data/>), and every
//! plumblog database has `posts_fts`. Why and how: docs/ARCHITECTURE.md (Site-specific state).
//!
//! The file is what D1 import runs: no `BEGIN`/`COMMIT`, `PRAGMA defer_foreign_keys` first,
//! every table, then the rows (parents before children) as multi-row `INSERT`s under D1's
//! 100,000-byte statement limit, a longer row as an `INSERT` and `UPDATE`s appending its long
//! values piece by piece, then indexes, views and triggers; `sqlite_sequence` only as `-- sequence`
//! lines, applied by `blog restore` outside the import. Values are SQLite's own
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
/// page is cut after the row that crosses it, so a reply holds at most this plus one row. Small enough for any reply, large
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
    /// A row whose key alone is too long for a statement D1 import accepts.
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
    /// Primary key columns, unquoted, in key order.
    pk: Vec<String>,
    fks: Vec<Fk>,
}

/// One FOREIGN KEY: the child's columns (positions in `cols`) and the parent's (unquoted;
/// the parent's primary key when the schema names none).
struct Fk {
    parent: String,
    from: Vec<usize>,
    to: Vec<String>,
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
            pk: Vec::new(),
            fks: Vec::new(),
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
        t.pk = pk.into_iter().map(|(_, c)| c).collect();
        let mut by_id: Vec<(i64, i64, &Map<String, Value>)> = r[1]
            .iter()
            .map(|f| {
                let n = |k: &str| f.get(k).and_then(Value::as_i64).unwrap_or(0);
                (n("id"), n("seq"), f)
            })
            .collect();
        by_id.sort_by_key(|(id, seq, _)| (*id, *seq));
        for group in by_id.chunk_by(|a, b| a.0 == b.0) {
            let from: Option<Vec<usize>> = group
                .iter()
                .map(|(_, _, f)| t.cols.iter().position(|c| *c == ident(&text(f, "from"))))
                .collect();
            // A key the columns cannot show is left to the import to check.
            if let Some(from) = from {
                t.fks.push(Fk {
                    parent: text(group[0].2, "table"),
                    from,
                    to: group.iter().map(|(_, _, f)| text(f, "to")).collect(),
                });
            }
        }
    }
    let pks: HashMap<String, Vec<String>> = tables
        .iter()
        .map(|t| (t.name.clone(), t.pk.clone()))
        .collect();
    for t in &mut tables {
        for fk in &mut t.fks {
            if fk.to.iter().any(String::is_empty) {
                fk.to = pks.get(&fk.parent).cloned().unwrap_or_default();
            }
        }
    }
    let (virt, plain): (Vec<Table>, Vec<Table>) = tables.into_iter().partition(|t| t.virt);
    let mut out = ordered(plain);
    out.extend(virt);
    Ok(out)
}

/// One row as literals: `(keys, values)`.
type Row = (Vec<String>, Vec<String>);

/// One page of `t` after `cursor` (key literals): its rows, and whether the table ends with
/// this page.
type Page = (Vec<Row>, bool);

/// The key and value literal columns of a select over `t`.
fn literal_columns(t: &Table) -> (Vec<String>, Vec<String>) {
    let keys = t
        .keys
        .iter()
        .enumerate()
        .map(|(i, k)| format!("{} AS _pb_k{i}", literal(k)))
        .collect();
    let vals = t
        .cols
        .iter()
        .enumerate()
        .map(|(i, c)| format!("{} AS _pb_v{i}", literal(c)))
        .collect();
    (keys, vals)
}

fn row_of(t: &Table, r: &Map<String, Value>) -> Result<Row, DumpError> {
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
    Ok((keys, vals))
}

fn page(db: &Database, t: &Table, cursor: Option<&[String]>, n: u64) -> Result<Page, DumpError> {
    let (key_lits, vals) = literal_columns(t);
    let raw_keys: Vec<String> = t
        .keys
        .iter()
        .enumerate()
        .map(|(i, k)| format!("{k} AS _pb_o{i}"))
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
    let out = rows
        .iter()
        .map(|r| row_of(t, r))
        .collect::<Result<Vec<_>, _>>()?;
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

/// A literal that can be written in pieces: `'text'`, `X'blob'`, or text holding a NUL.
#[derive(Clone, Copy)]
enum Piecewise {
    Text,
    Blob,
    NulText,
}

const NUL_TEXT_OPEN: &str = "CAST(X'";
const NUL_TEXT_CLOSE: &str = "' AS TEXT)";

impl Piecewise {
    fn of(lit: &str) -> Option<Self> {
        if lit.starts_with(NUL_TEXT_OPEN) && lit.ends_with(NUL_TEXT_CLOSE) {
            Some(Self::NulText)
        } else if lit.starts_with("X'") && lit.ends_with('\'') {
            Some(Self::Blob)
        } else if lit.len() >= 2 && lit.starts_with('\'') && lit.ends_with('\'') {
            Some(Self::Text)
        } else {
            None
        }
    }

    /// What the `INSERT` holds until the pieces are appended.
    fn empty(self) -> &'static str {
        match self {
            Self::Blob => "X''",
            Self::Text | Self::NulText => "''",
        }
    }

    /// `c` with `piece` appended. `||` makes TEXT, so a BLOB is cast back; both keep the bytes.
    fn append(self, c: &str, piece: &str) -> String {
        match self {
            Self::Blob => format!("CAST({c} || {piece} AS BLOB)"),
            Self::Text | Self::NulText => format!("{c} || {piece}"),
        }
    }

    /// `lit` as literals of at most `max` bytes each (at least 32), cut between characters:
    /// never inside a doubled quote or a UTF-8 sequence, so each piece is valid text.
    fn pieces(self, lit: &str, max: usize) -> Vec<String> {
        let mut out = Vec::new();
        match self {
            Self::Text => {
                let cap = max - 2;
                let mut cur = String::new();
                let mut chars = lit[1..lit.len() - 1].chars();
                while let Some(c) = chars.next() {
                    let mut unit = String::from(c);
                    if c == '\'' {
                        unit.extend(chars.next());
                    }
                    if !cur.is_empty() && cur.len() + unit.len() > cap {
                        out.push(format!("'{cur}'"));
                        cur.clear();
                    }
                    cur.push_str(&unit);
                }
                if !cur.is_empty() {
                    out.push(format!("'{cur}'"));
                }
            }
            Self::Blob => {
                let hex = &lit[2..lit.len() - 1];
                let cap = (max - 3) & !1;
                let mut at = 0;
                while at < hex.len() {
                    let end = (at + cap).min(hex.len());
                    out.push(format!("X'{}'", &hex[at..end]));
                    at = end;
                }
            }
            Self::NulText => {
                let hex = &lit[NUL_TEXT_OPEN.len()..lit.len() - NUL_TEXT_CLOSE.len()];
                let cap = (max - NUL_TEXT_OPEN.len() - NUL_TEXT_CLOSE.len()) & !1;
                let continues = |i: usize| {
                    u8::from_str_radix(&hex[i..i + 2], 16).is_ok_and(|b| b & 0xc0 == 0x80)
                };
                let mut at = 0;
                while at < hex.len() {
                    let mut end = (at + cap).min(hex.len());
                    while end < hex.len() && end > at + 2 && continues(end) {
                        end -= 2;
                    }
                    if end < hex.len() && continues(end) {
                        // Not UTF-8 there: SQLite joins the bytes as they are anyway.
                        end = (at + cap).min(hex.len());
                    }
                    out.push(format!("{NUL_TEXT_OPEN}{}{NUL_TEXT_CLOSE}", &hex[at..end]));
                    at = end;
                }
            }
        }
        out
    }
}

/// Writes rows as SQL and keeps the dump consistent by FOREIGN KEY: a row whose parent the
/// dump lacks (added after the parent's table was read) brings that parent along, read now,
/// or is left out when the parent is gone.
struct Dumper<'a> {
    db: &'a Database<'a>,
    tables: &'a [Table],
    index: HashMap<String, usize>,
    out: &'a mut dyn Write,
    /// Row keys written, for each table some FOREIGN KEY points at.
    written: HashMap<usize, BTreeSet<Vec<String>>>,
    /// Parent column values written, per (parent table, columns).
    referenced: HashMap<(usize, Vec<String>), BTreeSet<Vec<String>>>,
    counts: Vec<u64>,
    left_out: Vec<u64>,
}

impl<'a> Dumper<'a> {
    fn new(db: &'a Database<'a>, tables: &'a [Table], out: &'a mut dyn Write) -> Self {
        let index: HashMap<String, usize> = tables
            .iter()
            .enumerate()
            .map(|(i, t)| (t.name.clone(), i))
            .collect();
        let mut written = HashMap::new();
        let mut referenced = HashMap::new();
        for fk in tables.iter().flat_map(|t| &t.fks) {
            if let Some(&p) = index.get(&fk.parent) {
                written.insert(p, BTreeSet::new());
                referenced.insert((p, fk.to.clone()), BTreeSet::new());
            }
        }
        Self {
            db,
            tables,
            index,
            out,
            written,
            referenced,
            counts: vec![0; tables.len()],
            left_out: vec![0; tables.len()],
        }
    }

    /// The rows of table `ti`, page by page; returns nothing, counts are kept.
    fn table(&mut self, ti: usize) -> Result<(), DumpError> {
        let tables = self.tables;
        let t = &tables[ti];
        let mut stmt = String::new();
        let mut cursor: Option<Vec<String>> = None;
        let mut n = FIRST_PAGE_ROWS;
        loop {
            let (rows, end) = page(self.db, t, cursor.as_deref(), n)?;
            let got = rows.len() as u64;
            for (keys, vals) in rows {
                if cursor.as_ref() == Some(&keys) {
                    return Err(malformed(format!("table {} repeats a key", t.name)));
                }
                cursor = Some(keys.clone());
                // Brought along earlier as a parent.
                if self.written.get(&ti).is_some_and(|w| w.contains(&keys)) {
                    continue;
                }
                if !self.parents(ti, &vals, 0)? {
                    self.left_out[ti] += 1;
                    continue;
                }
                self.emit(ti, &keys, vals, Some(&mut stmt))?;
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
            self.out.write_all(stmt.as_bytes())?;
        }
        Ok(())
    }

    /// Whether every parent the row `vals` of table `ti` names is in the dump, adding the
    /// missing ones; `false` when one is gone.
    fn parents(&mut self, ti: usize, vals: &[String], depth: usize) -> Result<bool, DumpError> {
        let tables = self.tables;
        for fk in &tables[ti].fks {
            let Some(&p) = self.index.get(&fk.parent) else {
                continue;
            };
            let tuple: Vec<String> = fk.from.iter().map(|&i| vals[i].clone()).collect();
            // A NULL in the key: SQLite checks nothing.
            if tuple.iter().any(|v| v.eq_ignore_ascii_case("NULL"))
                || fk.to.len() != tuple.len()
                || self
                    .referenced
                    .get(&(p, fk.to.clone()))
                    .is_some_and(|s| s.contains(&tuple))
            {
                continue;
            }
            if !self.bring(p, &fk.to, &tuple, depth)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Reads now the row of table `p` whose `to` columns hold `tuple` and adds it, with its
    /// own missing parents; `false` when there is none.
    fn bring(
        &mut self,
        p: usize,
        to: &[String],
        tuple: &[String],
        depth: usize,
    ) -> Result<bool, DumpError> {
        // A chain of rows each added after its parent was read: deeper means a loop.
        if depth > 64 {
            return Ok(false);
        }
        let tables = self.tables;
        let t = &tables[p];
        let (keys, vals) = literal_columns(t);
        let cols: Vec<String> = to.iter().map(|c| ident(c)).collect();
        let sql = format!(
            "SELECT {}, {} FROM {} WHERE ({}) = ({}) LIMIT 1",
            keys.join(", "),
            vals.join(", "),
            ident(&t.name),
            cols.join(", "),
            tuple.join(", ")
        );
        let Some(r) = self.db.rows(&sql, &[])?.into_iter().next() else {
            return Ok(false);
        };
        let (keys, vals) = row_of(t, &r)?;
        let seen = self.written.entry(p).or_default();
        if !seen.insert(keys.clone()) {
            // Written already, its value spelled differently (a collation, an affinity).
            if let Some(s) = self.referenced.get_mut(&(p, to.to_vec())) {
                s.insert(tuple.to_vec());
            }
            return Ok(true);
        }
        if !self.parents(p, &vals, depth + 1)? {
            if let Some(w) = self.written.get_mut(&p) {
                w.remove(&keys);
            }
            return Ok(false);
        }
        self.emit(p, &keys, vals, None)?;
        Ok(true)
    }

    /// Records the row as written and writes it: into `stmt` (a multi-row `INSERT` being
    /// built for table `ti`) or as statements of its own.
    fn emit(
        &mut self,
        ti: usize,
        keys: &[String],
        vals: Vec<String>,
        stmt: Option<&mut String>,
    ) -> Result<(), DumpError> {
        let tables = self.tables;
        let t = &tables[ti];
        for ((p, to), set) in &mut self.referenced {
            if *p == ti {
                let tuple: Option<Vec<String>> = to
                    .iter()
                    .map(|c| {
                        t.cols
                            .iter()
                            .position(|x| *x == ident(c))
                            .map(|i| vals[i].clone())
                    })
                    .collect();
                set.extend(tuple);
            }
        }
        self.counts[ti] += 1;
        let prefix = format!(
            "INSERT INTO {} ({}) VALUES\n",
            ident(&t.name),
            t.cols.join(", ")
        );
        let row = format!("({})", vals.join(","));
        let fits = prefix.len() + row.len() + 1 < MAX_STATEMENT;
        if let Some(w) = self.written.get_mut(&ti) {
            w.insert(keys.to_vec());
        }
        match stmt {
            Some(stmt) if fits => {
                // `,\n` before the row and `;` after it.
                if !stmt.is_empty() && stmt.len() + 2 + row.len() + 1 >= MAX_STATEMENT {
                    stmt.push_str(";\n");
                    self.out.write_all(stmt.as_bytes())?;
                    stmt.clear();
                }
                if stmt.is_empty() {
                    stmt.push_str(&prefix);
                } else {
                    stmt.push_str(",\n");
                }
                stmt.push_str(&row);
                Ok(())
            }
            None if fits => {
                self.out.write_all(format!("{prefix}{row};\n").as_bytes())?;
                Ok(())
            }
            _ => self.split(t, &prefix, keys, vals),
        }
    }

    /// A row over [`MAX_STATEMENT`]: its largest TEXT and BLOB values are inserted empty and
    /// appended piece by piece with `UPDATE`s on its key, each statement under the limit.
    fn split(
        &mut self,
        t: &Table,
        prefix: &str,
        keys: &[String],
        mut vals: Vec<String>,
    ) -> Result<(), DumpError> {
        let total = |v: &[String]| prefix.len() + 2 + v.iter().map(|s| s.len() + 1).sum::<usize>();
        let too_large = |bytes| DumpError::RowTooLarge {
            table: t.name.clone(),
            key: shown_key(keys),
            bytes,
        };
        let full = total(&vals);
        let mut order: Vec<usize> = (0..vals.len())
            .filter(|&i| Piecewise::of(&vals[i]).is_some() && !t.keys.contains(&t.cols[i]))
            .collect();
        order.sort_by_key(|&i| std::cmp::Reverse(vals[i].len()));
        let mut later = Vec::new();
        for i in order {
            if total(&vals) < MAX_STATEMENT {
                break;
            }
            let kind = Piecewise::of(&vals[i]).unwrap_or(Piecewise::Text);
            let lit = std::mem::replace(&mut vals[i], kind.empty().to_string());
            later.push((i, kind, lit));
        }
        // Only the key and short values are left: a key this long cannot be split.
        if total(&vals) >= MAX_STATEMENT {
            return Err(too_large(full));
        }
        let target = if t.keys.len() == 1 {
            format!("{} = {}", t.keys[0], keys[0])
        } else {
            format!("({}) = ({})", t.keys.join(", "), keys.join(", "))
        };
        let mut sql = format!("{prefix}({});\n", vals.join(","));
        for (i, kind, lit) in later {
            let c = &t.cols[i];
            let shell = format!(
                "UPDATE {} SET {c} = {} WHERE {target};",
                ident(&t.name),
                kind.append(c, "")
            );
            let max = MAX_STATEMENT.saturating_sub(shell.len() + 1);
            if max < 1024 {
                return Err(too_large(full));
            }
            for piece in kind.pieces(&lit, max) {
                let _ = writeln!(
                    sql,
                    "UPDATE {} SET {c} = {} WHERE {target};",
                    ident(&t.name),
                    kind.append(c, &piece)
                );
            }
        }
        self.out.write_all(sql.as_bytes())?;
        Ok(())
    }
}

/// `-- sequence "<table>" <seq>`: an AUTOINCREMENT high-water mark at dump time. Kept as data,
/// not SQL, so the import never writes `sqlite_sequence` (D1 may refuse that); `blog restore`
/// raises the marks in a query of its own after the import.
pub const SEQUENCE: &str = "-- sequence ";

/// `seq` as the AUTOINCREMENT high-water mark of `table`, unless it is higher already, so a
/// deleted id is never handed out again: two statements, the row made only where the table
/// exists.
#[must_use]
pub fn raise_sequence(table: &str, seq: i64) -> [String; 2] {
    let name = format!("'{}'", table.replace('\'', "''"));
    [
        format!(
            "INSERT INTO sqlite_sequence (name, seq) SELECT {name}, 0 WHERE NOT EXISTS \
             (SELECT 1 FROM sqlite_sequence WHERE name = {name}) AND EXISTS \
             (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = {name})"
        ),
        format!("UPDATE sqlite_sequence SET seq = max(seq, {seq}) WHERE name = {name}"),
    ]
}

/// The `-- sequence` lines of a dump.
#[must_use]
pub fn sequence_marks(text: &str) -> Vec<(String, i64)> {
    text.lines()
        .filter_map(|l| {
            let rest = l.strip_prefix(SEQUENCE)?.strip_prefix('"')?;
            let (name, n) = rest.rsplit_once("\" ")?;
            Some((name.replace("\"\"", "\""), n.parse().ok()?))
        })
        .collect()
}

/// `sqlite_sequence` as `(table, seq)`; empty when no table ever used AUTOINCREMENT.
///
/// # Errors
/// As [`Database::rows`].
pub fn sequences(db: &Database) -> Result<Vec<(String, i64)>, ApiError> {
    let exists = db.rows(
        "SELECT 1 AS x FROM sqlite_master WHERE type = 'table' AND name = 'sqlite_sequence'",
        &[],
    )?;
    if exists.is_empty() {
        return Ok(Vec::new());
    }
    Ok(db
        .rows("SELECT name, seq FROM sqlite_sequence ORDER BY name", &[])?
        .iter()
        .filter_map(|r| Some((text(r, "name"), r.get("seq")?.as_i64()?)))
        .collect())
}

/// What a dump wrote: rows per table in file order, and rows left out per table because
/// their parent row was deleted while the dump ran.
#[derive(Debug, Default)]
pub struct Written {
    pub counts: Vec<(String, u64)>,
    pub left_out: Vec<(String, u64)>,
}

/// Writes the dump of `db` to `out`; `bookmark` is the Time Travel bookmark taken just
/// before, recorded in the header.
///
/// # Errors
/// [`DumpError`]: the API, a row too large for D1 import, or `out`.
pub fn write(db: &Database, bookmark: &str, out: &mut dyn Write) -> Result<Written, DumpError> {
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

    let tables = tables(db, &objects)?;
    // Every CREATE TABLE first: a parent row brought along may belong to a later table.
    for t in &tables {
        out.write_all(format!("{};\n", t.sql).as_bytes())?;
    }
    let mut d = Dumper::new(db, &tables, out);
    for ti in 0..tables.len() {
        d.table(ti)?;
    }
    let (counts, left_out) = (d.counts, d.left_out);
    // Read after the rows, so every id written is at or below it.
    let marks = sequences(db)?;
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
    let named = |v: Vec<u64>| -> Vec<(String, u64)> {
        tables.iter().map(|t| t.name.clone()).zip(v).collect()
    };
    let written = Written {
        counts: named(counts),
        left_out: named(left_out).into_iter().filter(|c| c.1 > 0).collect(),
    };
    let mut tail = String::new();
    for (name, seq) in &marks {
        let _ = writeln!(tail, "{SEQUENCE}{} {seq}", ident(name));
    }
    for (name, n) in &written.counts {
        let _ = writeln!(tail, "{ROWS}{} {n}", ident(name));
    }
    tail.push_str(END);
    tail.push('\n');
    out.write_all(tail.as_bytes())?;
    Ok(written)
}

/// Names the rows a dump left out because their parent was deleted while it ran.
fn warn_left_out(run: &mut Run, left_out: &[(String, u64)]) {
    if left_out.is_empty() {
        return;
    }
    let list: Vec<String> = left_out.iter().map(|(t, n)| format!("{t} {n}")).collect();
    let rows: u64 = left_out.iter().map(|c| c.1).sum();
    run.line(
        key!("cli.backup.left_out"),
        &[("rows", rows.to_string()), ("tables", list.join(", "))],
    );
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
        return Err(unwritable(
            &path,
            text.t(key!("cli.backup.reason_file_exists"), &[]),
        ));
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
    let written = match written {
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
        return Err(unwritable(
            &path,
            text.t(key!("cli.backup.reason_file_exists"), &[]),
        ));
    }
    fs::rename(&part, &path).map_err(|e| {
        let _ = fs::remove_file(&part);
        unwritable(&path, e.to_string())
    })?;
    let counts = written.counts;
    warn_left_out(run, &written.left_out);
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
