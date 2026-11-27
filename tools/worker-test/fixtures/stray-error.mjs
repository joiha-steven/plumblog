// Run by harness.test.mjs under `node --test`: test 1 throws from a timer (stray error inside a
// test); test 2 must still find the shared Worker alive.
import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { startWorker } from "../harness.mjs";

let w;
before(async () => {
  w = await startWorker();
});
after(async () => {
  await w?.stop();
});
test("stray error", async () => {
  setTimeout(() => {
    throw new Error("stray-marker");
  }, 10);
  await new Promise((r) => setTimeout(r, 300));
});
test("the shared Worker is still up", async () => {
  assert.equal((await fetch(`${w.url}/healthz`)).status, 200);
});
