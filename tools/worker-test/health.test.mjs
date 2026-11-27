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
  // From the CF_VERSION_METADATA binding: `blog deploy` waits for the new version's id here.
  assert.match(res.headers.get("x-plumblog-version") ?? "", /^[0-9a-f-]{8,}$/i);
});

test("unknown path returns 404", async () => {
  const res = await fetch(`${worker.url}/no-such-page`);
  assert.equal(res.status, 404);
});
