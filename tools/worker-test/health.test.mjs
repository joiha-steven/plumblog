import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { startWorker } from "./harness.mjs";

let worker;
before(async () => {
  worker = await startWorker();
});
after(async () => {
  await worker?.stop();
});

test("GET /healthz returns 200 ok", async () => {
  const res = await fetch(`${worker.url}/healthz`);
  assert.equal(res.status, 200);
  assert.equal(await res.text(), "ok");
});

test("unknown path returns 404", async () => {
  const res = await fetch(`${worker.url}/no-such-page`);
  assert.equal(res.status, 404);
});
