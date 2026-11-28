// Migrations on real local D1 (wrangler, no Worker needed):
// (a) a fresh database gets exactly the tables and indexes the migration files create;
// (b) for every sample in migration-samples/NNNN.sql (data valid right after migration NNNN),
//     applying the later migrations keeps every row: each row is found again by its key from
//     before (rowid; primary key for WITHOUT ROWID tables) with the same value in every column
//     present both before and after. New rows are allowed; row order is never relied on.
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";
import { d1Exec, d1QueryEach, makeStateDir, wrangler } from "./harness.mjs";

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

/** Rows of each statement, in order, on one connection to the state dir's D1 file (reads go to SQLite directly, not through wrangler). */
const queryEach = (dir, sqls) => d1QueryEach(dir, sqls);

/** Rows of a single statement. */
const query = (dir, sql) => queryEach(dir, [sql])[0];

function executeFile(dir, path) {
  // Stays on wrangler: this is how each migration file reaches a fresh D1, and it creates the file.
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

/** Per table: columns and the SQL that names a row: rowid, or the primary key for a
 *  WITHOUT ROWID table (no rowid there). Virtual (FTS5) tables compare title and body. */
function tableShapes(dir, tables, virtual) {
  const real = [...tables].filter((t) => !virtual.has(t));
  const res = queryEach(
    dir,
    real.flatMap((t) => [
      `SELECT sql FROM sqlite_master WHERE type = 'table' AND name = '${t}'`,
      `SELECT name, pk FROM pragma_table_info('${t}')`,
    ]),
  );
  const shapes = {};
  for (const t of tables) if (virtual.has(t)) shapes[t] = { cols: ["title", "body"], key: ["rowid"] };
  real.forEach((t, i) => {
    const [{ sql }] = res[2 * i];
    const info = res[2 * i + 1];
    const pk = info.filter((c) => c.pk > 0).sort((a, b) => a.pk - b.pk).map((c) => q(c.name));
    shapes[t] = { cols: info.map((c) => c.name), key: /WITHOUT\s+ROWID/i.test(sql) ? pk : ["rowid"] };
  });
  return shapes;
}

/** Per table: rows of `cols` by key (JSON of the `key` expressions). `want` maps table -> {cols, key}. */
function keyedRowsAll(dir, want) {
  const tables = Object.keys(want);
  const res = queryEach(
    dir,
    tables.map((t) => {
      const { cols, key } = want[t];
      return `SELECT ${[...key.map((k, i) => `${k} AS "__k${i}"`), ...cols.map(q)].join(", ")} FROM ${q(t)}`;
    }),
  );
  const out = {};
  tables.forEach((t, i) => {
    const rows = new Map();
    for (const r of res[i]) rows.set(JSON.stringify(want[t].key.map((_, j) => r[`__k${j}`])), r);
    out[t] = rows;
  });
  return out;
}

/** Every table's shape and rows by key, to be compared after the later migrations. */
function contentBefore(dir, tables, virtual) {
  const shapes = tableShapes(dir, tables, virtual);
  const rows = keyedRowsAll(dir, shapes);
  return Object.fromEntries(Object.keys(shapes).map((t) => [t, { ...shapes[t], rows: rows[t] }]));
}

/** Old rows that are gone or whose surviving columns changed, found by their old key. */
function contentProblems(dir, before, virtual) {
  const problems = [];
  const nowShapes = tableShapes(dir, new Set(Object.keys(before)), virtual);
  const want = {};
  for (const [t, was] of Object.entries(before)) {
    const now = nowShapes[t];
    const cols = was.cols.filter((c) => now.cols.includes(c));
    const keyGone = was.key.filter((k) => k !== "rowid" && !now.cols.includes(k.slice(1, -1)));
    if (keyGone.length) {
      problems.push(`${t}: key column(s) ${keyGone.join(", ")} no longer exist; old rows cannot be matched`);
      continue;
    }
    want[t] = { cols, key: was.key };
  }
  const afterAll = keyedRowsAll(dir, want);
  for (const [t, was] of Object.entries(before)) {
    if (!want[t]) continue;
    const { cols } = want[t];
    const after = afterAll[t];
    const bad = [];
    for (const [k, old] of was.rows) {
      const row = after.get(k);
      const values = (r) => JSON.stringify(cols.map((c) => r[c]));
      if (!row) bad.push(`  key ${k}: row is gone\n    before: ${values(old)}`);
      else if (values(row) !== values(old)) {
        bad.push(`  key ${k}: values changed\n    before: ${values(old)}\n    after:  ${values(row)}`);
      }
    }
    if (bad.length) {
      problems.push(
        `${t}: ${bad.length} of ${was.rows.size} old rows lost or changed (key [${was.key.join(", ")}], columns [${cols.join(", ")}])\n` +
          bad.slice(0, 3).join("\n"),
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

// api_tokens was rebuilt by 0005 (scope CHECK): the rebuilt table must keep every constraint
// of the original, not only its rows.
test("api_tokens after every migration: scopes, unique hash, user cascade, index", () => {
  const db = makeStateDir();
  try {
    wrangler(["d1", "migrations", "apply", "DB", "--local", "--persist-to", db.dir]);
    const run = (sql) => d1Exec(db.dir, sql);
    run(`INSERT INTO users (id, login, slug, display_name, role, created_at, updated_at) VALUES
      (1, 'a', 'a', 'A', 'owner', 1, 1), (2, 'b', 'b', 'B', 'writer', 1, 1)`);
    run(`INSERT INTO api_tokens (user_id, name, token_hash, scope, created_at) VALUES
      (1, 'r', 'h-read', 'read', 1), (1, 'w', 'h-writer', 'writer', 1), (2, 'o', 'h-owner', 'owner', 1)`);
    const refused = (sql, why) => assert.throws(() => run(sql), /constraint|CHECK|UNIQUE|FOREIGN/i, why);
    refused(`INSERT INTO api_tokens (user_id, name, token_hash, scope, created_at) VALUES (1, 'x', 'h-x', 'admin', 1)`, "unknown scope");
    refused(`INSERT INTO api_tokens (user_id, name, token_hash, scope, created_at) VALUES (1, 'x', 'h-read', 'read', 1)`, "duplicate hash");
    refused(`INSERT INTO api_tokens (user_id, name, token_hash, scope, created_at) VALUES (99, 'x', 'h-y', 'read', 1)`, "missing user");
    run("DELETE FROM users WHERE id = 1");
    assert.deepEqual(query(db.dir, "SELECT user_id, scope FROM api_tokens ORDER BY id"), [{ user_id: 2, scope: "owner" }]);
    assert.deepEqual(query(db.dir, "SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'api_tokens' AND name NOT LIKE 'sqlite_autoindex%'"), [{ name: "api_tokens_user" }]);
    assert.equal(query(db.dir, "SELECT COUNT(*) AS n FROM sqlite_master WHERE name LIKE 'api_tokens_new%'")[0].n, 0, "scratch table left behind");
  } finally {
    db.remove();
  }
});
