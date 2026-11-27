// System log on the real Worker and local D1: page views write nothing, and the daily cron
// prunes by age and leaves one `job.run` entry tied to its Worker log line by request id.
import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { startWorker, wrangler } from "./harness.mjs";

const DAY_MS = 24 * 60 * 60 * 1000;

let worker;
before(async () => {
  worker = await startWorker();
});
after(async () => {
  await worker?.stop();
});

function sql(command) {
  const out = wrangler(["d1", "execute", "DB", "--local", "--persist-to", worker.state, "--json", "--command", command]);
  const start = out.indexOf("[");
  assert.ok(start >= 0, `no JSON in wrangler output:\n${out}`);
  return JSON.parse(out.slice(start)).at(-1).results ?? [];
}

const rows = () => sql("SELECT request_id, kind, actor_id, error_code, detail FROM system_log ORDER BY at_ms, id");

test("page views write nothing; the daily job prunes and records itself", async () => {
  const now = Date.now();
  sql(
    "INSERT INTO system_log (at_ms, request_id, kind, detail) VALUES " +
      `(${now - 120 * DAY_MS}, 'seed-old', 'system.deploy', 'older than 90 days'), ` +
      `(${now - DAY_MS}, 'seed-new', 'system.deploy', 'yesterday')`,
  );

  for (const path of ["/healthz", "/", "/no-such-page"]) await fetch(`${worker.url}${path}`);
  assert.deepEqual(rows().map((r) => r.request_id), ["seed-old", "seed-new"], "views must not write");

  const res = await fetch(`${worker.url}/__scheduled?cron=${encodeURIComponent("17 3 * * *")}`);
  assert.equal(res.status, 200, await res.text());

  const after = rows();
  console.log("system_log after the cron run:", JSON.stringify(after, null, 1));
  assert.deepEqual(after.map((r) => r.request_id).slice(0, 1), ["seed-new"], "the 120-day-old row is gone");
  assert.equal(after.length, 2);
  const job = after[1];
  assert.match(job.request_id, /^job-[0-9a-f]+$/);
  assert.equal(job.kind, "job.run");
  assert.equal(job.actor_id, null);
  assert.equal(job.error_code, null);
  assert.equal(job.detail, "system_log.retention\ndeleted\t1");
  // The Worker log line of that run carries the same id, so the admin entry leads to the trace.
  // wrangler prints Worker output a moment after the response.
  const want = `[req=${job.request_id}]`;
  let line;
  for (let i = 0; i < 50 && !line; i++) {
    line = worker.log().split("\n").find((l) => l.includes(want));
    if (!line) await new Promise((r) => setTimeout(r, 100));
  }
  console.log("Worker log line:", line);
  assert.match(line ?? "", /job\.run system_log\.retention deleted=1/);
});
