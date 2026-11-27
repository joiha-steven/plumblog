// Migrations on real local D1 (wrangler, no Worker needed):
// (a) a fresh database gets exactly the tables and indexes the migration files create;
// (b) for every sample in migration-samples/NNNN.sql (data valid right after migration NNNN),
//     applying the later migrations loses no rows and, where the row count is unchanged,
//     changes no value in any column present both before and after.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { makeStateDir, wrangler } from "./harness.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const MIGRATIONS = join(ROOT, "migrations");
const SAMPLES = join(ROOT, "tools/worker-test/migration-samples");

const migrations = readdirSync(MIGRATIONS)
  .filter((f) => /^\d{4}_.*\.sql$/.test(f))
  .sort()
  .map((f) => ({ n: Number(f.slice(0, 4)), path: join(MIGRATIONS, f) }));

// FTS5 keeps its index in shadow tables named after the virtual table.
const FTS_SHADOWS = ["_data", "_idx", "_content", "_docsize", "_config"];

/** Names of tables and indexes the migration files leave behind, replayed in order. */
function expectedSchema() {
  const tables = new Set();
  const indexes = new Set();
  const virtual = new Set();
  const stmt =
    /\b(CREATE\s+(UNIQUE\s+)?INDEX|CREATE\s+(VIRTUAL\s+)?TABLE|DROP\s+(TABLE|INDEX))\s+(IF\s+(NOT\s+)?EXISTS\s+)?"?(\w+)"?|\bALTER\s+TABLE\s+"?(\w+)"?\s+RENAME\s+TO\s+"?(\w+)"?/gi;
  for (const { path } of migrations) {
    const sql = readFileSync(path, "utf8").replace(/--[^\n]*/g, "");
    for (const m of sql.matchAll(stmt)) {
      if (m[8]) {
        tables.delete(m[8]);
        tables.add(m[9]);
        continue;
      }
      const head = m[1].toUpperCase().replace(/\s+/g, " ");
      const name = m[7];
      if (head.endsWith("INDEX") && head.startsWith("CREATE")) indexes.add(name);
      else if (head === "DROP INDEX") indexes.delete(name);
      else if (head === "DROP TABLE") tables.delete(name);
      else {
        tables.add(name);
        if (m[3]) virtual.add(name);
      }
    }
  }
  return { tables, indexes, virtual };
}

/** Rows of every statement in `sql`, run on the local D1 in `dir`. */
function query(dir, sql) {
  const out = wrangler(["d1", "execute", "DB", "--local", "--persist-to", dir, "--json", "--command", sql]);
  const start = out.indexOf("[");
  assert.ok(start >= 0, `no JSON in wrangler output:\n${out}`);
  return JSON.parse(out.slice(start)).flatMap((r) => r.results ?? []);
}

function executeFile(dir, path) {
  wrangler(["d1", "execute", "DB", "--local", "--persist-to", dir, "--file", path]);
}

/** Tables and indexes in the database, without SQLite, D1 and FTS5 internals. */
function actualSchema(dir, virtual) {
  const internal = (name) =>
    name.startsWith("sqlite_") ||
    name.startsWith("_cf_") ||
    name === "d1_migrations" ||
    [...virtual].some((v) => FTS_SHADOWS.some((s) => name === v + s));
  const rows = query(dir, "SELECT type, name FROM sqlite_master WHERE type IN ('table', 'index')");
  const pick = (type) => new Set(rows.filter((r) => r.type === type && !internal(r.name)).map((r) => r.name));
  return { tables: pick("table"), indexes: pick("index") };
}

/** Row count per table, as one row of scalar subqueries (D1 caps compound SELECTs). */
function counts(dir, tables) {
  const sql = `SELECT ${[...tables].map((t) => `(SELECT COUNT(*) FROM "${t}") AS "${t}"`).join(", ")}`;
  const [row] = query(dir, sql);
  return row;
}

const q = (name) => `"${name}"`;

const hash = (rowJson) => {
  const sha = createHash("sha256");
  for (const j of rowJson) sha.update(j).update("\n");
  return sha.digest("hex");
};

/** Columns and primary key of a table; virtual (FTS5) tables compare rowid, title, body. */
function tableShape(dir, table, isVirtual) {
  if (isVirtual) return { cols: ["rowid", "title", "body"], pk: ["rowid"] };
  const info = query(dir, `SELECT name, pk FROM pragma_table_info('${table}')`);
  return {
    cols: info.map((c) => c.name),
    pk: info.filter((c) => c.pk > 0).sort((a, b) => a.pk - b.pk).map((c) => c.name),
  };
}

/** Rows of `cols` in a deterministic order: primary key, else rowid. */
function orderedRows(dir, table, cols, pk) {
  const orderBy = (pk.length ? pk : ["rowid"]).map(q).join(", ");
  return query(dir, `SELECT ${cols.map(q).join(", ")} FROM ${q(table)} ORDER BY ${orderBy}`);
}

/** Every table's shape and ordered rows, to be compared after the later migrations. */
function contentBefore(dir, tables, virtual) {
  const out = {};
  for (const t of tables) {
    const shape = tableShape(dir, t, virtual.has(t));
    out[t] = { ...shape, rows: orderedRows(dir, t, shape.cols, shape.pk) };
  }
  return out;
}

/** Problems found comparing each table, on the columns present both before and after. */
function contentProblems(dir, before, virtual) {
  const problems = [];
  for (const [t, was] of Object.entries(before)) {
    const now = tableShape(dir, t, virtual.has(t));
    const cols = was.cols.filter((c) => now.cols.includes(c));
    const pk = was.pk.filter((c) => cols.includes(c));
    const key = (r) => JSON.stringify(cols.map((c) => r[c]));
    const a = was.rows.map(key);
    const b = orderedRows(dir, t, cols, pk).map(key);
    if (b.length < a.length) {
      problems.push(`${t}: rows ${a.length} -> ${b.length}`);
    } else if (b.length === a.length && hash(a) !== hash(b)) {
      const i = a.findIndex((j, k) => j !== b[k]);
      problems.push(
        `${t}: content changed (${a.length} rows, columns [${cols.join(", ")}], checksum ${hash(a).slice(0, 12)} -> ${hash(b).slice(0, 12)})\n` +
          `  first differing row #${i}\n  before: ${a[i]}\n  after:  ${b[i]}`,
      );
    }
  }
  return problems;
}

const sorted = (set) => [...set].sort();

test("fresh database has every table and index the migrations create", () => {
  const want = expectedSchema();
  assert.ok(want.tables.size > 0 && want.indexes.size > 0, "parsed no tables or indexes from migrations/");
  const db = makeStateDir();
  try {
    wrangler(["d1", "migrations", "apply", "DB", "--local", "--persist-to", db.dir]);
    const got = actualSchema(db.dir, want.virtual);
    assert.deepEqual(sorted(got.tables), sorted(want.tables), "tables");
    assert.deepEqual(sorted(got.indexes), sorted(want.indexes), "indexes");
  } finally {
    db.remove();
  }
});

const samples = readdirSync(SAMPLES)
  .filter((f) => /^\d{4}\.sql$/.test(f))
  .sort();

test("there is a sample for the first migration", () => {
  assert.ok(samples.includes("0001.sql"), "tools/worker-test/migration-samples/0001.sql is missing");
});

for (const sample of samples) {
  const at = Number(sample.slice(0, 4));
  test(`sample ${sample}: later migrations keep every row`, () => {
    assert.ok(migrations.some((m) => m.n === at), `no migration numbered ${sample.slice(0, 4)}`);
    const { virtual } = expectedSchema();
    const db = makeStateDir();
    try {
      for (const m of migrations.filter((m) => m.n <= at)) executeFile(db.dir, m.path);
      executeFile(db.dir, join(SAMPLES, sample));
      const { tables } = actualSchema(db.dir, virtual);
      const before = counts(db.dir, tables);
      const content = contentBefore(db.dir, tables, virtual);
      const empty = Object.entries(before).filter(([, n]) => n === 0).map(([t]) => t);
      assert.deepEqual(empty, [], `the sample must put rows in every table`);

      for (const m of migrations.filter((m) => m.n > at)) executeFile(db.dir, m.path);
      const now = actualSchema(db.dir, virtual).tables;
      const gone = sorted(tables).filter((t) => !now.has(t));
      assert.deepEqual(gone, [], "tables dropped by a later migration");
      const after = counts(db.dir, tables);
      const lost = Object.keys(before).filter((t) => after[t] < before[t]).map((t) => `${t}: ${before[t]} -> ${after[t]}`);
      assert.deepEqual(lost, [], "tables that lost rows");
      assert.deepEqual(contentProblems(db.dir, content, virtual), [], "tables whose content changed");
    } finally {
      db.remove();
    }
  });
}
