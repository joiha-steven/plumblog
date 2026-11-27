// Migrations on real local D1 (wrangler, no Worker needed):
// (a) a fresh database gets exactly the tables and indexes the migration files create;
// (b) for every sample in migration-samples/NNNN.sql (data valid right after migration NNNN),
//     applying the later migrations loses no rows in any table.
import assert from "node:assert/strict";
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
      const empty = Object.entries(before).filter(([, n]) => n === 0).map(([t]) => t);
      assert.deepEqual(empty, [], `the sample must put rows in every table`);

      for (const m of migrations.filter((m) => m.n > at)) executeFile(db.dir, m.path);
      const now = actualSchema(db.dir, virtual).tables;
      const gone = sorted(tables).filter((t) => !now.has(t));
      assert.deepEqual(gone, [], "tables dropped by a later migration");
      const after = counts(db.dir, tables);
      const lost = Object.keys(before).filter((t) => after[t] < before[t]).map((t) => `${t}: ${before[t]} -> ${after[t]}`);
      assert.deepEqual(lost, [], "tables that lost rows");
    } finally {
      db.remove();
    }
  });
}
