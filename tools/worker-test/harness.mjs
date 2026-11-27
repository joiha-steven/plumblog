// Starts the real Rust Worker under `wrangler dev` with local D1/R2, for HTTP-level tests.
// Run through scripts/worker-test: it installs the pinned tools, builds the wasm once
// (node --test runs files in parallel processes) and sets WORKER_TEST_RUN_DIR.
import { spawn, spawnSync } from "node:child_process";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const CONFIG = join(ROOT, "tools/wrangler.dev.toml");
const WRANGLER = join(ROOT, "tools/node_modules/.bin/wrangler");
// scripts/worker-test removes this dir and kills the process groups recorded in it,
// as a second line of defence if this process dies without running its own cleanup.
const RUN_DIR = process.env.WORKER_TEST_RUN_DIR ?? tmpdir();
const READY_TIMEOUT_MS = 60_000;

// Keep wrangler quiet and non-interactive; never phone home from tests.
const ENV = { ...process.env, WRANGLER_SEND_METRICS: "false", CI: "1", NO_COLOR: "1" };

function run(cmd, args) {
  const r = spawnSync(cmd, args, { cwd: ROOT, env: ENV, encoding: "utf8" });
  if (r.status !== 0) {
    throw new Error(`${cmd} ${args.join(" ")} failed (exit ${r.status ?? r.signal})\n${r.stdout}\n${r.stderr}`);
  }
  return r.stdout;
}

function freePort() {
  return new Promise((ok, fail) => {
    const s = createServer();
    s.unref();
    s.on("error", fail);
    s.listen(0, "127.0.0.1", () => {
      const { port } = s.address();
      s.close(() => ok(port));
    });
  });
}

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
  live.delete(entry);
}
process.on("exit", () => [...live].forEach(cleanupSync));
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
  const entry = { state: mkdtempSync(join(RUN_DIR, "plumblog-d1-")), pgid: null, pgidFile: null };
  live.add(entry);
  return { dir: entry.state, remove: () => cleanupSync(entry) };
}

/**
 * Applies migrations to a fresh local D1 and starts the Worker. Call stop() when done.
 * `seedSql` runs after the migrations, before the Worker starts (test data only); `vars`
 * become `--var KEY:VALUE` (wrangler dev's way to set vars and secrets per run).
 */
export async function startWorker({ seedSql = null, vars = {} } = {}) {
  const entry = { state: mkdtempSync(join(RUN_DIR, "plumblog-wrangler-")), pgid: null, pgidFile: null };
  live.add(entry);
  const common = ["--config", CONFIG, "--persist-to", entry.state];
  try {
    run(WRANGLER, ["d1", "migrations", "apply", "DB", "--local", ...common]);
    if (seedSql) {
      const file = join(entry.state, "seed.sql");
      writeFileSync(file, seedSql);
      run(WRANGLER, ["d1", "execute", "DB", "--local", ...common, "--file", file]);
    }
  } catch (e) {
    cleanupSync(entry);
    throw e;
  }

  const port = await freePort();
  const inspector = await freePort();
  // detached: wrangler starts workerd children; killing the group stops them all.
  const child = spawn(
    WRANGLER,
    ["dev", ...common, "--ip", "127.0.0.1", "--port", String(port), "--inspector-port", String(inspector),
      "--show-interactive-dev-session=false", "--log-level", "warn",
      ...Object.entries(vars).flatMap(([k, v]) => ["--var", `${k}:${v}`])],
    { cwd: ROOT, env: ENV, detached: true, stdio: ["ignore", "pipe", "pipe"] },
  );
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
      if (exited !== null) {
        cleanupSync(entry);
        return ok();
      }
      child.once("exit", () => {
        cleanupSync(entry);
        ok();
      });
      try {
        process.kill(-child.pid, "SIGTERM");
      } catch {
        // Already gone between the check and the kill.
      }
      setTimeout(() => cleanupSync(entry), 5_000).unref();
    });

  const url = `http://127.0.0.1:${port}`;
  const deadline = Date.now() + READY_TIMEOUT_MS;
  while (Date.now() < deadline) {
    if (exited !== null) {
      await stop();
      throw new Error(`wrangler dev exited early (${exited}):\n${log}`);
    }
    try {
      await fetch(`${url}/healthz`);
      // Reads the running Worker's local D1 (test assertions only).
      const query = (sql) =>
        JSON.parse(run(WRANGLER, ["d1", "execute", "DB", "--local", ...common, "--json", "--command", sql]))[0].results;
      return { url, stop, log: () => log, query };
    } catch {
      await new Promise((r) => setTimeout(r, 200));
    }
  }
  await stop();
  throw new Error(`wrangler dev not ready after ${READY_TIMEOUT_MS} ms:\n${log}`);
}
