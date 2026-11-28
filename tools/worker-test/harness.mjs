// Starts the real Rust Worker under `wrangler dev` with local D1/R2, for HTTP-level tests.
// Run through scripts/worker-test: it installs the pinned tools, builds the wasm once
// (node --test runs files in parallel processes) and sets WORKER_TEST_RUN_DIR.
import { spawn, spawnSync } from "node:child_process";
import { randomUUID } from "node:crypto";
import { linkSync, mkdirSync, mkdtempSync, readFileSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { basename, dirname, join, resolve } from "node:path";
import { afterEach, beforeEach } from "node:test";
import { fileURLToPath } from "node:url";

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
// One `wrangler d1 execute` / `r2 object get`: about 1 s; a hang here must fail, not stall the file.
const HELPER_TIMEOUT_MS = 60_000;

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
      const file = join(entry.state, "seed.sql");
      writeFileSync(file, sql);
      run(WRANGLER, ["d1", "execute", "DB", "--local", ...common, "--file", file]);
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
    ["dev", join(OUT, "worker/shim.mjs"), "--assets", assets, ...common, "--ip", "127.0.0.1", "--port", String(port), "--inspector-port", "0",
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
        // Reads the running Worker's local D1 (test assertions only).
        const query = (sql) =>
          JSON.parse(run(WRANGLER, ["d1", "execute", "DB", "--local", ...common, "--json", "--command", sql]))[0].results;
        // Size of an object in the running Worker's local R2 bucket, or null when it is absent.
        const r2Size = (key) => {
          const r = spawnSync(WRANGLER, ["r2", "object", "get", `plumblog-dev/${key}`, "--local", ...common, "--pipe"], { cwd: ROOT, env: ENV, maxBuffer: 64 << 20, timeout: HELPER_TIMEOUT_MS });
          return r.status === 0 ? r.stdout.length : null;
        };
        // state: the --persist-to dir, for reading the local D1 with `wrangler d1 execute`.
        return { url, instance: token, stop, state: entry.state, log: () => log, query, r2Size };
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
