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
import { PORT_DIR, claim, startWorker } from "./harness.mjs";

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
    const racer = () =>
      new Promise((ok, fail) => {
        const c = spawn(process.execPath, [join(HERE, "fixtures", "race-claim.mjs"), String(start), dir, String(COUNT)], { env: CHILD_ENV });
        let out = "";
        c.stdout.on("data", (d) => (out += d));
        c.on("error", fail);
        c.on("exit", (code) => (code === 0 ? ok(JSON.parse(out.split("\n").find((l) => l.startsWith("[")))) : fail(new Error(`racer exited ${code}: ${out}`))));
      });
    const [a, b] = await Promise.all([racer(), racer()]);
    assert.equal(a.length + b.length, COUNT, `claimed ${a.length} + ${b.length} of ${COUNT}`);
    assert.equal(new Set([...a, ...b]).size, COUNT, "a lock went to both");
    assert.equal(claim(join(dir, "empty")), false);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
