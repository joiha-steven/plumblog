// The harness itself: parallel starts never share a port or reach each other's Worker, a start
// on a taken port fails loudly instead of returning the other Worker, and a process that ends
// without stop() leaves nothing running and is not kept alive by it.
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";
import { PORT_DIR, claim, d1Exec, d1File, d1Query, d1Sql, startWorker } from "./harness.mjs";

const HERE = dirname(fileURLToPath(import.meta.url));
const started = [];
const start = async (opts) => {
  const w = await startWorker(opts);
  started.push(w);
  return w;
};
after(async () => {
  await Promise.all(started.map((w) => w.stop()));
});

const instance = async (w) => (await fetch(`${w.url}/__instance`)).text();

test("parallel starts get distinct ports and each answers with its own token", async () => {
  const workers = await Promise.all([start(), start(), start()]);
  assert.equal(new Set(workers.map((w) => w.url)).size, 3, workers.map((w) => w.url).join());
  assert.equal(new Set(workers.map((w) => w.instance)).size, 3);
  for (const w of workers) assert.equal(await instance(w), w.instance, w.url);
});

test("a start on a port another Worker holds fails; it never returns the other Worker", async () => {
  const a = await start();
  const port = Number(new URL(a.url).port);
  await assert.rejects(
    start({ port }),
    (e) => {
      assert.match(e.message, /Address already in use|another Worker|exited early/, e.message);
      assert.ok(e.message.includes(String(port)), "names the port");
      return true;
    },
  );
  assert.equal(await instance(a), a.instance, "the first Worker is untouched");
});

// A child that starts a Worker and then ends without stop(): normally (nothing left to do)
// or by an uncaught error. Either way node must exit by itself and the Worker must be gone.
for (const how of ["returns", "throws"]) {
  test(`a process that ${how} without stop() exits on its own and leaves no Worker`, async () => {
    const code = `
      import { startWorker } from ${JSON.stringify(join(HERE, "harness.mjs"))};
      const w = await startWorker();
      console.log("URL " + w.url);
      ${how === "throws" ? 'throw new Error("boom");' : ""}
    `;
    const { NODE_TEST_CONTEXT, ...env } = process.env; // else node treats the child as a test worker
    const r = spawnSync(process.execPath, ["--input-type=module", "-e", code], { encoding: "utf8", timeout: 90_000, env });
    assert.notEqual(r.error?.code, "ETIMEDOUT", "node stayed alive after the script ended");
    // The error exit code is node:test's (harness.mjs imports it); only zero vs not matters.
    assert.equal(r.status === 0, how === "returns", `${r.status} ${r.stderr}`);
    const url = /URL (\S+)/.exec(r.stdout)?.[1];
    assert.ok(url, r.stdout + r.stderr);
    await assert.rejects(fetch(`${url}/healthz`, { signal: AbortSignal.timeout(3_000) }), "the Worker is gone");
    assert.ok(!existsSync(join(tmpdir(), "plumblog-worker-test-ports", new URL(url).port)), "port lock released");
  });
}

const { NODE_TEST_CONTEXT, ...CHILD_ENV } = process.env; // else node treats a child as a test worker
const nodeTest = (fixture, env = {}) =>
  spawnSync(process.execPath, ["--test", join(HERE, "fixtures", fixture)], { encoding: "utf8", timeout: 120_000, env: { ...CHILD_ENV, ...env } });

test("the port lock directory is per user", () => {
  assert.match(PORT_DIR, new RegExp(`-${process.getuid?.() ?? "u"}$`));
});

test("a stray error inside a test fails only that test; the shared Worker stays up", () => {
  const r = nodeTest("stray-error.mjs");
  assert.match(r.stdout, /not ok \d+ - stray error/, r.stdout);
  assert.match(r.stdout, /ok \d+ - the shared Worker is still up/, r.stdout);
  assert.doesNotMatch(r.stdout, /not ok \d+ - the shared Worker/, r.stdout);
});

test("a test over the per-test bound stops its file and is named", () => {
  const r = nodeTest("hung-test.mjs", { WORKER_TEST_TEST_TIMEOUT_MS: "1500" });
  assert.notEqual(r.error?.code, "ETIMEDOUT", "the file never ended");
  assert.notEqual(r.status, 0);
  assert.match(r.stdout + r.stderr, /FAIL: test "suite Z > this one hangs" in hung-test\.mjs ran over 1\.5 s/);
});

test("stop() leaves nothing that keeps the process up", () => {
  const code = `
    import { startWorker } from ${JSON.stringify(join(HERE, "harness.mjs"))};
    const w = await startWorker();
    await w.stop();
    console.log("STOPPED " + Date.now());
  `;
  const r = spawnSync(process.execPath, ["--input-type=module", "-e", code], { encoding: "utf8", timeout: 90_000, env: CHILD_ENV });
  const at = Number(/STOPPED (\d+)/.exec(r.stdout)?.[1]);
  assert.ok(at, r.stdout + r.stderr);
  // The 5 s fallback timer used to hold the process after a clean stop.
  assert.ok(Date.now() - at < 3_000, `process ended ${Date.now() - at} ms after stop()`);
});

test("two processes racing for stale locks: each lock goes to exactly one", async () => {
  const dir = mkdtempSync(join(tmpdir(), "plumblog-lock-race-"));
  try {
    const dead = spawnSync(process.execPath, ["-e", "process.stdout.write(String(process.pid))"], { encoding: "utf8" }).stdout;
    const COUNT = 300;
    for (let i = 0; i < COUNT; i++) writeFileSync(join(dir, String(i)), dead);
    writeFileSync(join(dir, "empty"), ""); // a lock whose pid is not written yet counts as held
    const start = Date.now() + 1_500;
    // A racer that exits makes its own locks stale, and the other then rightly takes them over
    // (it did, on the last few locks, whenever one racer finished first): both stay alive, and
    // keep their locks, until both have reported; closing stdin lets them go.
    const racer = () => {
      const c = spawn(process.execPath, [join(HERE, "fixtures", "race-claim.mjs"), String(start), dir, String(COUNT)], { env: CHILD_ENV });
      const exited = new Promise((ok, fail) => {
        c.on("error", fail);
        c.on("exit", (code) => (code === 0 ? ok() : fail(new Error(`racer exited ${code}`))));
      });
      const won = new Promise((ok, fail) => {
        let out = "";
        c.stdout.on("data", (d) => {
          out += d;
          const line = out.split("\n").find((l) => l.startsWith("[") && l.endsWith("]"));
          if (line) ok(JSON.parse(line));
        });
        exited.then(() => fail(new Error(`racer ended without a result: ${out}`)), fail);
      });
      return { c, exited, won };
    };
    const racers = [racer(), racer()];
    let a, b;
    try {
      [a, b] = await Promise.all(racers.map((r) => r.won));
    } finally {
      for (const r of racers) r.c.stdin.end();
    }
    await Promise.all(racers.map((r) => r.exited));
    assert.equal(a.length + b.length, COUNT, `claimed ${a.length} + ${b.length} of ${COUNT}`);
    assert.equal(new Set([...a, ...b]).size, COUNT, "a lock went to both");
    assert.equal(claim(join(dir, "empty")), false);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

// The database is shared with workerd (WAL mode). Another process holds the write lock for 1.5 s:
// a read must not fail and must not see the uncommitted row; a write must wait for the lock
// (SQLite's busy_timeout) and then succeed. The old `wrangler d1 execute` path failed here.
// The write proves the wait: its INSERT..SELECT only inserts when it sees the holder's committed row.
test("D1 helpers wait for a write lock held by another process instead of failing", async () => {
  const w = await start();
  const holder = `
    import { DatabaseSync } from "node:sqlite";
    const db = new DatabaseSync(process.argv[1]);
    db.exec("PRAGMA busy_timeout = 10000");
    db.exec("BEGIN IMMEDIATE");
    db.exec("INSERT INTO settings (key, value_json, updated_at) VALUES ('held', '1', 0)");
    console.log("locked");
    setTimeout(() => { db.exec("COMMIT"); db.close(); }, 1500);
  `;
  const child = spawn(process.execPath, ["--input-type=module", "-e", holder, d1File(w.state)], { env: CHILD_ENV, stdio: ["ignore", "pipe", "inherit"] });
  const done = new Promise((ok) => child.on("exit", ok));
  await new Promise((ok, no) => {
    child.stdout.on("data", (d) => d.toString().includes("locked") && ok());
    child.on("exit", () => no(new Error("lock holder exited before locking")));
  });
  assert.deepEqual(w.query("SELECT COUNT(*) AS n FROM settings WHERE key = 'held'"), [{ n: 0 }], "uncommitted row is not visible");
  d1Exec(w.state, "INSERT INTO settings (key, value_json, updated_at) SELECT 'after', '1', 0 FROM settings WHERE key = 'held'");
  assert.deepEqual(w.query("SELECT key FROM settings WHERE key IN ('held', 'after') ORDER BY key"), [{ key: "after" }, { key: "held" }], "the write waited for the commit, then ran");
  assert.equal(await done, 0, "lock holder exited cleanly");
});

// The same lock, held while the Worker itself writes (an anonymous passkey challenge is stored
// by POST login/options). The Worker either waits for the lock and succeeds, or fails with a
// clear error; either way no row is lost or doubled.
test("a Worker write during a held lock waits or fails clearly, never loses or doubles rows", async () => {
  const w = await start({ seedSql: (url) => `INSERT INTO settings (key, value_json, updated_at) VALUES ('origin', '"${url}"', 0)` });
  const holder = `
    import { DatabaseSync } from "node:sqlite";
    const db = new DatabaseSync(process.argv[1]);
    db.exec("PRAGMA busy_timeout = 10000");
    db.exec("BEGIN IMMEDIATE");
    db.exec("INSERT INTO settings (key, value_json, updated_at) VALUES ('held', '1', 0)");
    console.log("locked");
    setTimeout(() => { db.exec("COMMIT"); db.close(); }, 1500);
  `;
  const child = spawn(process.execPath, ["--input-type=module", "-e", holder, d1File(w.state)], { env: CHILD_ENV, stdio: ["ignore", "pipe", "inherit"] });
  const done = new Promise((ok) => child.on("exit", ok));
  await new Promise((ok, no) => {
    child.stdout.on("data", (d) => d.toString().includes("locked") && ok());
    child.on("exit", () => no(new Error("lock holder exited before locking")));
  });
  const t0 = Date.now();
  const res = await fetch(`${w.url}/api/auth/passkey/login/options`, { method: "POST", headers: { "content-type": "application/json", origin: w.url }, body: "{}" });
  const text = await res.text();
  const waited = Date.now() - t0;
  console.log(`Worker write under a held lock: HTTP ${res.status} after ${waited} ms ${text.slice(0, 120)}`);
  assert.equal(await done, 0);
  const stored = w.query("SELECT COUNT(*) AS n FROM auth_challenges")[0].n;
  assert.equal(stored, res.status === 200 ? 1 : 0, `HTTP ${res.status}: ${text}`);
  assert.ok(res.status === 200 || res.status >= 500, `an unclear status ${res.status}: ${text}`);
  assert.equal(w.query("SELECT COUNT(*) AS n FROM settings WHERE key = 'held'")[0].n, 1, "the holder's row survived");
});

test("R2 helper on a Worker that never wrote an object: absent, not an error", async () => {
  const w = await start();
  assert.equal(w.r2Size("never/written"), null);
});

test("d1 helpers refuse ambiguous SQL instead of dropping part of it", async () => {
  const w = await start();
  const q = (sql) => d1Query(w.state, sql);
  assert.deepEqual(q("WITH x AS (SELECT 1 AS a) SELECT a FROM x"), [{ a: 1 }]);
  assert.deepEqual(q("-- note\nSELECT 2 AS a;"), [{ a: 2 }]);
  assert.throws(() => q("SELECT 1 AS a; SELECT 2 AS b"), /exactly one statement/);
  assert.throws(() => q("UPDATE settings SET value_json = '1' WHERE 0"), /exactly one statement that returns rows/);
  assert.throws(() => d1Exec(w.state, "SELECT 1"), /must not return rows/);
  assert.throws(() => d1Exec(w.state, "DELETE FROM settings WHERE 0; SELECT 1"), /must not return rows/);
  assert.throws(() => d1Sql(w.state, "SELECT 1 AS a; DELETE FROM settings WHERE 0"), /exactly one statement/);
  assert.deepEqual(d1Sql(w.state, "DELETE FROM settings WHERE 0; DELETE FROM settings WHERE 0"), []);
});

test("a failing batch leaves nothing half applied", async () => {
  const w = await start();
  assert.throws(() => d1Exec(w.state, "INSERT INTO settings (key, value_json, updated_at) VALUES ('half', '1', 0); INSERT INTO no_such_table VALUES (1)"), /no_such_table/);
  assert.deepEqual(w.query("SELECT COUNT(*) AS n FROM settings WHERE key = 'half'"), [{ n: 0 }]);
});
