// Starts the real Rust Worker under `wrangler dev` with local D1/R2, for HTTP-level tests.
// Run through scripts/worker-test, which installs the pinned tools this file expects.
import { spawn, spawnSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const CONFIG = join(ROOT, "tools/wrangler.dev.toml");
const WRANGLER = join(ROOT, "tools/node_modules/.bin/wrangler");
const WORKER_BUILD = process.env.WORKER_BUILD ?? join(ROOT, "target/tools/bin/worker-build");
const READY_TIMEOUT_MS = 60_000;

// Keep wrangler quiet and non-interactive; never phone home from tests.
const ENV = { ...process.env, WRANGLER_SEND_METRICS: "false", CI: "1", NO_COLOR: "1" };

function run(cmd, args, opts = {}) {
  const r = spawnSync(cmd, args, { cwd: ROOT, env: ENV, encoding: "utf8", ...opts });
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

// Built once per test process; every test file shares the same wasm.
let built = false;
function buildWorker() {
  if (built) return;
  // --no-opt: wasm-opt is for deploy size; tests want the fastest correct build.
  run(WORKER_BUILD, ["--release", "--no-opt", "--out-dir", "../../target/worker-dev"], {
    cwd: join(ROOT, "crates/worker"),
  });
  built = true;
}

/** Builds the Worker, applies migrations to a fresh local D1, starts it. Call stop() when done. */
export async function startWorker() {
  buildWorker();
  const state = mkdtempSync(join(tmpdir(), "plumblog-wrangler-"));
  const common = ["--config", CONFIG, "--persist-to", state];
  run(WRANGLER, ["d1", "migrations", "apply", "DB", "--local", ...common]);

  const port = await freePort();
  const inspector = await freePort();
  // detached: wrangler starts workerd children; killing the group stops them all.
  const child = spawn(
    WRANGLER,
    ["dev", ...common, "--ip", "127.0.0.1", "--port", String(port), "--inspector-port", String(inspector),
      "--show-interactive-dev-session=false", "--log-level", "warn"],
    { cwd: ROOT, env: ENV, detached: true, stdio: ["ignore", "pipe", "pipe"] },
  );
  let log = "";
  child.stdout.on("data", (d) => (log += d));
  child.stderr.on("data", (d) => (log += d));
  let exited = null;
  child.on("exit", (code, signal) => (exited = code ?? signal));

  const stop = () =>
    new Promise((ok) => {
      if (exited !== null) {
        rmSync(state, { recursive: true, force: true });
        return ok();
      }
      child.once("exit", () => {
        rmSync(state, { recursive: true, force: true });
        ok();
      });
      try {
        process.kill(-child.pid, "SIGTERM");
      } catch {
        // Already gone between the check and the kill.
      }
      setTimeout(() => {
        try {
          process.kill(-child.pid, "SIGKILL");
        } catch {
          // Exited after SIGTERM, as intended.
        }
      }, 5_000).unref();
    });

  const url = `http://127.0.0.1:${port}`;
  const deadline = Date.now() + READY_TIMEOUT_MS;
  while (Date.now() < deadline) {
    if (exited !== null) throw new Error(`wrangler dev exited early (${exited}):\n${log}`);
    try {
      await fetch(`${url}/healthz`);
      return { url, stop };
    } catch {
      await new Promise((r) => setTimeout(r, 200));
    }
  }
  await stop();
  throw new Error(`wrangler dev not ready after ${READY_TIMEOUT_MS} ms:\n${log}`);
}
