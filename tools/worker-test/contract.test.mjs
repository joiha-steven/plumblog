// The store contract suite (crates/store/src/contract) against real local D1, through the
// /__contract route that only test builds have. Every case runs on its own subtest, one at a
// time: they share one database, which the route empties before each case.
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

test("store contract on D1", async (t) => {
  const list = await fetch(`${worker.url}/__contract`);
  assert.equal(list.status, 200, "GET /__contract (is the worker built with --features contract-tests?)");
  const cases = (await list.text()).split("\n").filter(Boolean);
  assert.ok(cases.length > 0, "the contract suite lists no cases");
  for (const name of cases) {
    // Sequential on purpose (await inside the loop): cases share one database.
    await t.test(name, async () => {
      const res = await fetch(`${worker.url}/__contract/${encodeURIComponent(name)}`, { method: "POST" });
      const body = await res.text();
      assert.equal(res.status, 200, `${name} failed on D1: ${body}`);
      assert.equal(body, "ok");
    });
  }
});
