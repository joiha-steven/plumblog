// The harness itself: parallel starts never share a port or reach each other's Worker, a start
// on a taken port fails loudly instead of returning the other Worker, and a process that ends
// without stop() leaves nothing running and is not kept alive by it.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { after, test } from "node:test";
import { fileURLToPath } from "node:url";
import { startWorker } from "./harness.mjs";

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
