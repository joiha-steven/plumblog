// run.mjs: a failing run shows the failing test and its error in both reporters, and the
// whole-run bound names the unfinished file, kills it and exits non-zero.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const HERE = dirname(fileURLToPath(import.meta.url));
const { NODE_TEST_CONTEXT, ...BASE_ENV } = process.env; // else node treats the child as a test worker
const runner = (fixture, env) =>
  spawnSync(process.execPath, [join(HERE, "run.mjs")], {
    encoding: "utf8",
    timeout: 60_000,
    env: { ...BASE_ENV, WORKER_TEST_DIR: join(HERE, "fixtures", fixture), ...env },
  });

for (const [reporter, summary] of [["tap", /# duration_ms/], ["spec", /duration_ms/]]) {
  test(`a failing run prints the failing test, its error and the summary (${reporter} reporter)`, () => {
    const r = runner("run-fail", { WORKER_TEST_REPORTER: reporter });
    assert.equal(r.status, 1, r.stdout + r.stderr);
    assert.match(r.stdout, /the failing one/);
    assert.match(r.stdout, /marker-expected-one-got-two/);
    assert.match(r.stdout, /passes/);
    assert.match(r.stdout, summary);
  });
}

test("the whole-run bound names the unfinished file, kills it and exits non-zero", () => {
  const r = runner("run-hang", { WORKER_TEST_TOTAL_MS: "3000" });
  assert.notEqual(r.error?.code, "ETIMEDOUT", "run.mjs never ended");
  assert.equal(r.status, 1, r.stdout + r.stderr);
  assert.match(r.stderr, /run exceeded 3 s/);
  assert.match(r.stderr, /stuck\.test\.mjs/);
});
