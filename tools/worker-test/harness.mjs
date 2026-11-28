// Starts the real Rust Worker under `wrangler dev` with local D1/R2, for HTTP-level tests.
// Run through scripts/worker-test: it installs the pinned tools, builds the wasm once
// (node --test runs files in parallel processes) and sets WORKER_TEST_RUN_DIR.
import { spawn, spawnSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { linkSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { afterEach, beforeEach } from "node:test";
import { fileURLToPath } from "node:url";

// node:sqlite is unflagged from Node 22.13 (tools/package.json engines). Say so, not "ERR_UNKNOWN_BUILTIN_MODULE".
let DatabaseSync;
try {
  ({ DatabaseSync } = await import("node:sqlite"));
} catch (e) {
  throw new Error(`worker-test needs node:sqlite (Node >= 22.13); this is Node ${process.versions.node}: ${e.message}`, { cause: e });
}

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const CONFIG = join(ROOT, "tools/wrangler.dev.toml");
const WRANGLER = join(ROOT, "tools/node_modules/.bin/wrangler");
// scripts/worker-test removes this dir and kills the process groups recorded in it,
// as a second line of defence if this process dies without running its own cleanup.
const RUN_DIR = process.env.WORKER_TEST_RUN_DIR ?? tmpdir();
// The test build (scripts/worker-test), apart from target/worker-dev that the config names
// for scripts/admin-dev, so tests never replace files under a running dev server.
const OUT = process.env.WORKER_TEST_OUT ?? join(ROOT, "target/worker-test");
/** The static assets scripts/assemble-assets laid out for this build. */
export const ASSETS_DIR = join(OUT, "assets");
// Readiness and helper-process bounds. A start is wrangler migrations + seed + build of the
// worker: about 5 s idle, 17 s under a loaded 16-core run (measured); 60 s leaves 3x on a slow CI box.
const READY_TIMEOUT_MS = 60_000;
// One wrangler helper run (`d1 migrations apply`): about 1 s; a hang here must fail, not stall the file.
const HELPER_TIMEOUT_MS = 60_000;
// How long SQLite itself waits for a lock held by workerd or another connection before it gives
// up (SQLITE_BUSY). The Worker's own writes are single statements or short batches, so waits are
// expected to be milliseconds (harness.test.mjs checks both directions with a lock held for
// 1.5 s); 10 s is far below the per-test timeout. It is the database's own lock wait, not a
// test-level retry: a lock held longer still fails the test.
const SQLITE_BUSY_MS = 10_000;

// Keep wrangler quiet and non-interactive; never phone home from tests.
const ENV = { ...process.env, WRANGLER_SEND_METRICS: "false", CI: "1", NO_COLOR: "1" };

function run(cmd, args) {
  const r = spawnSync(cmd, args, { cwd: ROOT, env: ENV, encoding: "utf8", timeout: HELPER_TIMEOUT_MS });
  if (r.status !== 0) {
    const why = r.error?.code === "ETIMEDOUT" ? `timed out after ${HELPER_TIMEOUT_MS} ms` : `exit ${r.status ?? r.signal}`;
    throw new Error(`${cmd} ${args.join(" ")} failed (${why})\n${r.stdout}\n${r.stderr}`);
  }
  return r.stdout;
}

// Ports come from a range below every OS's ephemeral range (Linux 32768+, macOS 49152+), so
// nothing that binds port 0 (workerd's own sockets, other tools) can take one. Within the range
// a lock file per port, created with O_EXCL, hands each port to one starter at a time across
// all parallel test files and concurrent runs. The old way (bind port 0, close, pass the number
// to wrangler seconds later, after migrations and seeding) left a window in which two files
// could get the same port, and one then reached the other's Worker or died with "Address
// already in use".
const PORT_LO = 20_000;
const PORT_COUNT = 10_000;
// Per user: on a shared Linux /tmp another user's directory would refuse us (EACCES).
export const PORT_DIR = join(tmpdir(), `plumblog-worker-test-ports-${process.getuid?.() ?? "u"}`);

function alive(pid) {
  try {
    process.kill(pid, 0);
    return true;
  } catch (e) {
    return e.code === "EPERM";
  }
}

/**
 * Takes the lock file for a port. The pid is in the file from the moment the name exists
 * (written to a temp file, then link()ed: link fails if the name is taken), so nobody reads an
 * empty lock. A lock whose owner is gone is taken over by renaming it away first, never by
 * unlinking: a second process that also saw it stale cannot delete a lock another process has
 * just created, it finds a live pid in what it renamed and puts it back. Anything unreadable
 * counts as held; the caller moves on to the next port and verifies with ownsLock().
 */
export function claim(lock) {
  const tmp = `${lock}.${process.pid}.${randomUUID()}`;
  writeFileSync(tmp, String(process.pid));
  try {
    for (let attempt = 0; attempt < 2; attempt++) {
      try {
        linkSync(tmp, lock);
        return true;
      } catch (e) {
        if (e.code !== "EEXIST") throw e;
      }
      let owner;
      try {
        owner = Number(readFileSync(lock, "utf8"));
      } catch {
        return false; // released or replaced between the two calls: held by someone
      }
      if (!owner || alive(owner)) return false; // no pid at all: not ours to judge
      const aside = `${lock}.stale.${process.pid}.${randomUUID()}`;
      try {
        renameSync(lock, aside);
      } catch {
        return false; // someone else took it over first
      }
      const moved = Number(readFileSync(aside, "utf8"));
      if (!moved || alive(moved)) {
        // It was not the stale lock we read but a live one created meanwhile: give it back.
        try {
          linkSync(aside, lock);
        } catch {
          // Taken again already; the owner's ownsLock() check will notice.
        }
        rmSync(aside, { force: true });
        return false;
      }
      rmSync(aside, { force: true });
    }
    return false;
  } finally {
    rmSync(tmp, { force: true });
  }
}

/** Whether the lock file still names this process (a takeover race may have moved it). */
export function ownsLock(lock) {
  try {
    return Number(readFileSync(lock, "utf8")) === process.pid;
  } catch {
    return false;
  }
}

function bindable(port) {
  return new Promise((ok) => {
    const s = createServer();
    s.unref();
    s.on("error", () => ok(false));
    s.listen(port, "127.0.0.1", () => s.close(() => ok(true)));
  });
}

/** A port nobody else holds, with the lock file that keeps it ours until released. */
async function reservePort() {
  mkdirSync(PORT_DIR, { recursive: true });
  const first = Math.floor(Math.random() * PORT_COUNT);
  for (let i = 0; i < PORT_COUNT; i++) {
    const port = PORT_LO + ((first + i) % PORT_COUNT);
    const lock = join(PORT_DIR, String(port));
    if (!claim(lock)) continue;
    if ((await bindable(port)) && ownsLock(lock)) return { port, lock };
    if (ownsLock(lock)) rmSync(lock, { force: true }); // held by something that is not a test worker
  }
  throw new Error(`no free port in ${PORT_LO}..${PORT_LO + PORT_COUNT - 1}`);
}

// Per-test timeout that names the test. Longest test measured: about 12 s idle on 16 cores (the
// webhook timeout test waits 10 s by design); a 2-core CI runner is about 3x slower. 120 s is
// 10x the slowest test and well under the 20 min a CI cancel takes. Not node:test's `timeout`:
// that cannot be set once for every test, and `--test-timeout` bounds a whole file.
// Exiting runs the exit handler below, so the Workers die with the process.
// WORKER_TEST_TEST_TIMEOUT_MS exists for harness.test.mjs only.
const TEST_TIMEOUT_MS = Number(process.env.WORKER_TEST_TEST_TIMEOUT_MS ?? 120_000);
let testTimer = null;
beforeEach((t) => {
  testTimer = setTimeout(() => {
    console.error(`worker-test: FAIL: test "${t.fullName ?? t.name}" in ${basename(process.argv[1] ?? "?")} ran over ${TEST_TIMEOUT_MS / 1000} s; stopping this file`);
    process.exit(1);
  }, TEST_TIMEOUT_MS);
  testTimer.unref();
});
afterEach(() => {
  clearTimeout(testTimer);
  testTimer = null;
});

// Every live wrangler group and temp dir, so Ctrl-C or a crash cannot leak them.
const live = new Set();
function cleanupSync(entry) {
  if (!live.has(entry)) return; // already cleaned; never signal a pid that may be reused
  if (entry.pgid) {
    try {
      process.kill(-entry.pgid, "SIGKILL");
    } catch {
      // Group already gone.
    }
  }
  rmSync(entry.state, { recursive: true, force: true });
  if (entry.pgidFile) rmSync(entry.pgidFile, { force: true });
  if (entry.portLock) rmSync(entry.portLock, { force: true });
  live.delete(entry);
}
process.on("exit", () => [...live].forEach(cleanupSync));
// node:test fails the running test on a stray error and carries on, but rethrows one it cannot
// attribute to a test; that ends the process (exit code 7) without an "exit" event, and
// the Workers would outlive it. So kill them only when no test is running (testTimer is set
// between beforeEach and afterEach). Prepend: this runs before node:test's own listener.
process.prependListener("uncaughtException", () => {
  if (testTimer === null) [...live].forEach(cleanupSync);
});
for (const sig of ["SIGINT", "SIGTERM", "SIGHUP"]) {
  process.on(sig, () => {
    [...live].forEach(cleanupSync);
    process.exit(128 + ({ SIGHUP: 1, SIGINT: 2, SIGTERM: 15 })[sig]);
  });
}

/**
 * The one database file of a local D1 / R2 object under a `--persist-to` dir. miniflare names it by a hash of
 * the binding, so it is found by listing, never guessed. Returns null when the directory does not exist yet
 * (R2 has none until the first object is written); several files is an error.
 */
function sqliteFileIn(state, sub) {
  const dir = join(state, "v3", sub);
  let files;
  try {
    files = readdirSync(dir).filter((f) => f.endsWith(".sqlite") && f !== "metadata.sqlite"); // metadata.sqlite is miniflare's own bookkeeping
  } catch (e) {
    if (e.code === "ENOENT") return null;
    throw e;
  }
  if (files.length > 1) throw new Error(`expected one .sqlite file in ${dir}, found ${files.length}: ${files.join(", ")}`);
  return files.length ? join(dir, files[0]) : null;
}
export function d1File(state) {
  const f = sqliteFileIn(state, "d1/miniflare-D1DatabaseObject");
  if (!f) throw new Error(`no local D1 database under ${state}; \`wrangler d1 migrations apply\` creates it`);
  return f;
}
export const r2File = (state) => sqliteFileIn(state, "r2/miniflare-R2BucketObject");

/**
 * Runs `fn(db)` on a fresh connection to a local SQLite file, then closes it. Fresh each call: a
 * long-lived connection returned stale rows while workerd wrote through its own.
 */
function withDb(file, fn) {
  const db = new DatabaseSync(file);
  try {
    db.exec(`PRAGMA busy_timeout = ${SQLITE_BUSY_MS}`);
    return fn(db);
  } finally {
    db.close();
  }
}

const plain = (rows) => rows.map((r) => ({ ...r }));
// What may follow a statement without being another one: whitespace, `;`, comments.
const nothingLeft = (text) => text.replace(/--[^\n]*|\/\*[\s\S]*?\*\//g, "").replace(/[\s;]/g, "") === "";

/**
 * Runs `sql` one statement at a time (SQLite's own parser finds the end of each: `prepare` alone
 * silently drops everything after the first statement). Returns the rows of each statement that
 * returns columns, null for the others. Statements are prepared one by one, so a later one may use
 * what an earlier one created.
 */
function eachStatement(db, sql) {
  const out = [];
  for (let rest = sql; !nothingLeft(rest); ) {
    const st = db.prepare(rest);
    out.push(st.columns().length > 0 ? plain(st.all()) : (st.run(), null));
    rest = rest.slice(st.sourceSQL.length);
  }
  return out;
}

/** The same, in one transaction that takes the write lock first: the Worker never sees half a batch. */
function inTransaction(db, fn) {
  db.exec("BEGIN IMMEDIATE");
  try {
    const r = fn();
    db.exec("COMMIT");
    return r;
  } catch (e) {
    try {
      db.exec("ROLLBACK");
    } catch {
      // The failed statement already ended the transaction; the original error matters.
    }
    throw e;
  }
}

/**
 * Rows of exactly ONE statement that returns rows (SELECT, WITH, RETURNING, PRAGMA) on the local
 * D1 in `state`, which a running Worker has open. A statement that returns no rows (use d1Exec)
 * or a second statement is an error, never silently dropped. Not read-only on purpose: SQLite
 * refuses a read-only open of a WAL database whose -shm is gone.
 */
export function d1Query(state, sql) {
  return withDb(d1File(state), (db) => {
    const res = eachStatement(db, sql);
    if (res.length !== 1 || res[0] === null) throw new Error(`d1Query needs exactly one statement that returns rows (use d1Exec for writes); got ${res.length}: ${sql}`);
    return res[0];
  });
}

/** Rows of each single-statement query, in order, on one connection (migrations.test.mjs). */
export function d1QueryEach(state, sqls) {
  return withDb(d1File(state), (db) =>
    sqls.map((sql) => {
      const res = eachStatement(db, sql);
      if (res.length !== 1 || res[0] === null) throw new Error(`d1QueryEach needs one statement that returns rows per entry; got ${res.length}: ${sql}`);
      return res[0];
    }),
  );
}

/**
 * Runs write SQL (one or many statements, in one transaction) on the local D1 in `state`; SQLite
 * waits for workerd's lock. A statement that returns rows is an error: use d1Query for those.
 */
export function d1Exec(state, sql) {
  withDb(d1File(state), (db) =>
    inTransaction(db, () => {
      const res = eachStatement(db, sql);
      if (res.some((r) => r !== null)) throw new Error(`d1Exec statements must not return rows (use d1Query): ${sql}`);
    }),
  );
}

/** Runs `[{sql, params}]` (one statement each, params bound in order) in one transaction. */
export function d1Batch(state, stmts) {
  withDb(d1File(state), (db) =>
    inTransaction(db, () => {
      for (const { sql, params } of stmts) db.prepare(sql).run(...params);
    }),
  );
}

/**
 * For test helpers that take either: SQL whose first statement returns rows is a query (exactly
 * one statement, as d1Query); anything else is a write batch (d1Exec) and returns [].
 */
export function d1Sql(state, sql) {
  const first = withDb(d1File(state), (db) => db.prepare(sql).columns().length > 0);
  if (first) return d1Query(state, sql);
  d1Exec(state, sql);
  return [];
}

/** Size of an object in the Worker's local R2 bucket (from its metadata table), or null when absent. */
export function r2SizeIn(state, key) {
  const file = r2File(state);
  if (file === null) return null; // no object was ever written
  const rows = withDb(file, (db) => db.prepare("SELECT size FROM _mf_objects WHERE key = ?").all(key));
  return rows.length ? Number(rows[0].size) : null;
}

/** Runs wrangler with the dev config; throws with its output on failure. */
export function wrangler(args) {
  return run(WRANGLER, [...args, "--config", CONFIG]);
}

/**
 * A fresh directory for a local D1 (`--persist-to`), removed by remove() and, like the
 * Worker's state, on exit, Ctrl-C or a crash.
 */
export function makeStateDir() {
  const entry = { state: mkdtempSync(join(RUN_DIR, "plumblog-d1-")), pgid: null, pgidFile: null, portLock: null };
  live.add(entry);
  return { dir: entry.state, remove: () => cleanupSync(entry) };
}

/**
 * Applies migrations to a fresh local D1 and starts the Worker. Call stop() when done.
 * `seedSql` runs after the migrations, before the Worker starts (test data only); it may be a
 * function of the Worker's URL (e.g. to configure the site origin); `vars`
 * become `--var KEY:VALUE` (wrangler dev's way to set vars and secrets per run).
 * Ready means this start's own Worker answered with its own token (`/__instance`), not just
 * that something answers on the port. `port` pins the port; only the harness's own test uses it.
 * `assets` replaces the static assets directory (default: the build's, see `ASSETS_DIR`).
 */
/** Empties the database and saves the sample site (settings, posts, tags, one translated pair). */
export async function seedSample(worker) {
  const res = await fetch(`${worker.url}/__seed`, { method: "POST" });
  if (res.status !== 200) throw new Error(`/__seed: ${res.status} ${await res.text()}`);
}

export async function startWorker({ seedSql = null, vars = {}, port: pinned = null, assets = ASSETS_DIR } = {}) {
  const entry = { state: mkdtempSync(join(RUN_DIR, "plumblog-wrangler-")), pgid: null, pgidFile: null, portLock: null };
  live.add(entry);
  const common = ["--config", CONFIG, "--persist-to", entry.state];
  const who = basename(process.argv[1] ?? "worker-test");
  const token = randomUUID();
  let port;
  try {
    if (pinned === null) {
      const r = await reservePort();
      port = r.port;
      entry.portLock = r.lock;
    } else {
      port = pinned;
    }
    const url = `http://127.0.0.1:${port}`;
    entry.url = url;
    run(WRANGLER, ["d1", "migrations", "apply", "DB", "--local", ...common]);
    const sql = typeof seedSql === "function" ? seedSql(url) : seedSql;
    if (sql) {
      // No Worker runs yet and the file was just created by migrations apply: nothing to lock against.
      d1Exec(entry.state, sql);
    }
  } catch (e) {
    cleanupSync(entry);
    throw e;
  }
  const url = entry.url;

  // detached: wrangler starts workerd children; killing the group stops them all.
  // The inspector port 0 lets the OS pick: nothing here connects to it.
  const child = spawn(
    WRANGLER,
    ["dev", join(OUT, "entry.mjs"), "--assets", assets, ...common, "--ip", "127.0.0.1", "--port", String(port), "--inspector-port", "0",
      "--show-interactive-dev-session=false", "--log-level", "log",
      // Serves /__scheduled so tests can fire the cron trigger.
      "--test-scheduled",
      ...Object.entries({ ...vars, PLUMBLOG_TEST_INSTANCE: token }).flatMap(([k, v]) => ["--var", `${k}:${v}`])],
    { cwd: ROOT, env: ENV, detached: true, stdio: ["ignore", "pipe", "pipe"] },
  );
  // A suite that fails (or a hook that throws before stop() is reachable) must not keep node
  // alive on these handles; the exit handler above then kills the group.
  child.unref();
  child.stdout.unref();
  child.stderr.unref();
  entry.pgid = child.pid;
  entry.pgidFile = join(RUN_DIR, `${child.pid}.pgid`);
  writeFileSync(entry.pgidFile, "");
  let log = "";
  child.stdout.on("data", (d) => (log += d));
  child.stderr.on("data", (d) => (log += d));
  let exited = null;
  child.on("exit", (code, signal) => (exited = code ?? signal));

  const stop = () =>
    new Promise((ok) => {
      // Waiting for the exit needs a live handle, or node sees an empty loop and cancels the test.
      child.ref();
      if (exited !== null) {
        cleanupSync(entry);
        return ok();
      }
      const fallback = setTimeout(() => {
        cleanupSync(entry);
        ok();
      }, 5_000);
      child.once("exit", () => {
        clearTimeout(fallback);
        cleanupSync(entry);
        ok();
      });
      try {
        process.kill(-child.pid, "SIGTERM");
      } catch {
        // Already gone between the check and the kill.
      }
    });

  const deadline = Date.now() + READY_TIMEOUT_MS;
  let last = "no response yet";
  let foreign = null;
  while (Date.now() < deadline) {
    if (exited !== null) {
      await stop();
      throw new Error(`${who}: wrangler dev on ${url} exited early (${exited}):\n${log}`);
    }
    try {
      const res = await fetch(`${url}/__instance`, { signal: AbortSignal.timeout(2_000) });
      const body = await res.text();
      last = `HTTP ${res.status} ${JSON.stringify(body.slice(0, 80))}`;
      if (res.status === 200 && body === token && exited === null) {
        // Read the running Worker's local D1 / R2 (test assertions only), straight from the files.
        const query = (sql) => d1Query(entry.state, sql);
        const exec = (sql) => d1Exec(entry.state, sql);
        const r2Size = (key) => r2SizeIn(entry.state, key);
        return { url, instance: token, stop, state: entry.state, log: () => log, query, exec, r2Size };
      }
      if (res.status === 200 && body !== "") foreign = body;
    } catch (e) {
      last = `${e.name}: ${e.message}`;
    }
    if (foreign !== null) {
      // Another start's Worker owns this port: no amount of waiting makes it ours.
      await stop();
      throw new Error(`${who}: ${url} is answered by another Worker (instance ${JSON.stringify(foreign)}, expected ${token}); wrangler log:\n${log}`);
    }
    await new Promise((r) => setTimeout(r, 200));
  }
  await stop();
  throw new Error(`${who}: wrangler dev on ${url} not ready after ${READY_TIMEOUT_MS} ms; last response: ${last}\n${log}`);
}
