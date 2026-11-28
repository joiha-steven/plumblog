// System log on the real Worker and local D1: page views write nothing, and the daily cron
// prunes by age and leaves one `job.run` entry tied to its Worker log line by request id.
import assert from "node:assert/strict";
import { after, before, describe, test } from "node:test";
import { createHash, randomBytes } from "node:crypto";
import * as gen from "../../web/admin/src/api/model.gen.ts";
import { checkShape } from "../shape-check.mjs";
import { d1Sql, startWorker } from "./harness.mjs";

const DAY_MS = 24 * 60 * 60 * 1000;
// The one cron runs every 10 minutes; the run scheduled 03:10 UTC also does the daily jobs
// (crates/worker/src/jobs.rs). Miniflare's trigger route takes the scheduled `time`
// (wrangler's /__scheduled does not); the jobs still read the clock.
const daily = () => {
  const t = new Date();
  t.setUTCHours(3, 10, 0, 0);
  return `/cdn-cgi/local/scheduled?cron=${encodeURIComponent("*/10 * * * *")}&time=${t.getTime()}`;
};

let worker;
before(async () => {
  worker = await startWorker();
});
after(async () => {
  await worker?.stop();
});

function sql(command) {
  return d1Sql(worker.state, command);
}

// The daily run also records the webhook delivery prune; this file is about the log's own.
const rows = () =>
  sql(
    "SELECT request_id, kind, actor_id, error_code, detail FROM system_log " +
      "WHERE detail NOT LIKE 'webhook_deliveries.%' ORDER BY at_ms, id",
  );

test("page views write nothing; the daily job prunes and records itself", async () => {
  const now = Date.now();
  sql(
    "INSERT INTO system_log (at_ms, request_id, kind, detail) VALUES " +
      `(${now - 120 * DAY_MS}, 'seed-old', 'system.deploy', 'older than 90 days'), ` +
      `(${now - DAY_MS}, 'seed-new', 'system.deploy', 'yesterday')`,
  );

  for (const path of ["/healthz", "/", "/no-such-page"]) await fetch(`${worker.url}${path}`);
  assert.deepEqual(rows().map((r) => r.request_id), ["seed-old", "seed-new"], "views must not write");

  const res = await fetch(`${worker.url}${daily()}`);
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

/** The first Worker log line after `from` (an offset into the log) matching `re`. */
async function logLine(from, re) {
  for (let i = 0; i < 50; i++) {
    const line = worker.log().slice(from).split("\n").find((l) => re.test(l));
    if (line) return line;
    await new Promise((r) => setTimeout(r, 100));
  }
  return undefined;
}

test("a backlog drains in bounded runs (Retention::per_run)", async () => {
  const old = Date.now() - 120 * DAY_MS;
  sql(
    "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 2500) " +
      `INSERT INTO system_log (at_ms, request_id, kind, detail) SELECT ${old} + i, 'bulk', 'system.deploy', '' FROM n`,
  );
  const runs = [];
  for (const want of [2000, 500]) {
    const from = worker.log().length;
    const res = await fetch(`${worker.url}${daily()}`);
    assert.equal(res.status, 200);
    const line = await logLine(from, /prune_log deleted=/);
    console.log("Worker log line:", line);
    assert.match(line ?? "", new RegExp(`prune_log deleted=${want} rows_written=\\d+`));
    runs.push(Number(/rows_written=(\d+)/.exec(line)[1]));
  }
  const left = sql("SELECT count(*) AS n FROM system_log WHERE request_id = 'bulk'")[0].n;
  assert.equal(left, 0, "the second run finishes the backlog");
  // Each deleted row is written once per table and index (4 for system_log).
  assert.ok(runs[0] <= 2000 * 4, `first run wrote ${runs[0]} rows`);
});

// --- GET /api/v1/log, on its own Worker so the seeded rows are the whole log. ---
describe("log read", () => {
  const { SHAPES, ...rest } = gen;
  const lists = Object.fromEntries(Object.entries(rest).filter(([, v]) => Array.isArray(v)));
  const sha = (s) => createHash("sha256").update(s).digest("hex");
  const T = Date.now();
  const ownerSession = randomBytes(32).toString("base64url");
  const writerSession = randomBytes(32).toString("base64url");
  const as = (s) => ({ cookie: `__Host-plumblog_session=${s}` });
  let lw;

  before(async () => {
    // Ids follow insertion order; three rows share at_ms 3000.
    lw = await startWorker({
      seedSql: [
        `INSERT INTO users (id, login, slug, display_name, role, password_hash, disabled_at, created_at, updated_at) VALUES
          (1, 'olga', 'olga', 'Olga', 'owner', NULL, NULL, ${T}, ${T}),
          (2, 'will', 'will', 'Will', 'writer', NULL, NULL, ${T}, ${T});`,
        `INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES
          ('${sha(ownerSession)}', 1, ${T}, ${T + DAY_MS}), ('${sha(writerSession)}', 2, ${T}, ${T + DAY_MS});`,
        `INSERT INTO system_log (at_ms, request_id, kind, actor_id, error_code, detail) VALUES
          (1000, 'seed-a', 'auth.sign_in', 1, NULL, 'olga'),
          (2000, 'seed-b', 'auth.sign_in_failed', NULL, NULL, 'login\nuser\tOlga'),
          (3000, 'seed-c', 'error', NULL, 'store.failed', 'boom'),
          (3000, 'seed-d', 'content.changed', 2, NULL, 'post-1\naction\tupdate'),
          (3000, 'seed-e', 'system.deploy', NULL, NULL, '100% done_ok'),
          (4000, 'seed-f', 'settings.changed', 1, 'store.conflict', 'title');`,
      ].join("\n"),
    });
  });
  after(async () => {
    await lw?.stop();
  });

  async function get(query = "", who = as(ownerSession)) {
    const res = await fetch(`${lw.url}/api/v1/log${query}`, { headers: { origin: lw.url, ...who } });
    const text = await res.text();
    return { status: res.status, json: text ? JSON.parse(text) : null, text };
  }
  async function ids(query) {
    const r = await get(query);
    assert.equal(r.status, 200, r.text);
    assert.deepEqual(checkShape(SHAPES, lists, "log_page", r.json), [], r.text);
    return r.json.entries.map((e) => e.id);
  }

  test("newest first, in the log_page shape, with names, levels and fields", async () => {
    const r = await get("?limit=100");
    assert.equal(r.status, 200, r.text);
    assert.deepEqual(checkShape(SHAPES, lists, "log_page", r.json), [], r.text);
    assert.deepEqual(r.json.entries.map((e) => e.id), [6, 5, 4, 3, 2, 1]);
    assert.equal(r.json.next_cursor, null);
    const by = Object.fromEntries(r.json.entries.map((e) => [e.id, e]));
    assert.deepEqual(by[4], {
      id: 4, at: 3000, request_id: "seed-d", kind: "content.changed", level: "info", actor_id: 2,
      actor_name: "Will", error_code: null, subject: "post-1", fields: [{ key: "action", value: "update" }],
    });
    assert.equal(by[6].level, "warning", "an error code raises info to warning");
    assert.equal(by[6].error_code, "store.conflict");
    assert.equal(by[3].level, "error");
    assert.equal(by[2].level, "warning");
    assert.equal(by[5].actor_name, null);
    assert.equal(by[1].actor_name, "Olga");
  });

  test("filters: kind, level, code, actor, q, from/to", async () => {
    assert.deepEqual(await ids("?kind=auth.sign_in_failed"), [2]);
    assert.deepEqual(await ids("?level=warning"), [6, 3, 2]);
    assert.deepEqual(await ids("?level=error"), [3]);
    assert.deepEqual(await ids("?level=info"), [6, 5, 4, 3, 2, 1]);
    assert.deepEqual(await ids("?code=store.conflict"), [6]);
    assert.deepEqual(await ids("?actor=2"), [4]);
    assert.deepEqual(await ids("?q=seed-d"), [4], "request id");
    assert.deepEqual(await ids("?q=seed"), [], "a request id matches whole");
    assert.deepEqual(await ids("?q=OLGA"), [2, 1], "detail, any case");
    assert.deepEqual(await ids(`?q=${encodeURIComponent("%")}`), [5], "% is literal");
    assert.deepEqual(await ids(`?q=${encodeURIComponent("_ok")}`), [5], "_ is literal");
    assert.deepEqual(await ids(`?q=${encodeURIComponent("d_n")}`), [], "_ is no wildcard");
    assert.deepEqual(await ids("?from=2000&to=4000"), [5, 4, 3, 2], "from inclusive, to exclusive");
    assert.deepEqual(await ids("?level=warning&from=3000&q=boom"), [3], "filters combine");
  });

  test("pages by cursor across equal times: no duplicates, none missing", async () => {
    const seen = [];
    let cursor = null;
    let pages = 0;
    do {
      const r = await get(`?limit=2${cursor ? `&cursor=${cursor}` : ""}`);
      assert.equal(r.status, 200, r.text);
      assert.ok(r.json.entries.length <= 2);
      seen.push(...r.json.entries.map((e) => e.id));
      cursor = r.json.next_cursor;
      pages++;
    } while (cursor && pages < 10);
    assert.deepEqual(seen, [6, 5, 4, 3, 2, 1]);
    assert.equal(pages, 3);
  });

  test("a writer is 403, a signed-out caller 401", async () => {
    const w = await get("", as(writerSession));
    assert.equal(w.status, 403, w.text);
    assert.deepEqual(w.json, { error: "auth.forbidden" });
    const anon = await get("", {});
    assert.equal(anon.status, 401, anon.text);
    assert.deepEqual(anon.json, { error: "auth.unauthenticated" });
  });

  test("a bad filter is 400 input.invalid naming the field", async () => {
    const long = "x".repeat(101);
    for (const [query, field] of [
      ["?kind=nope", "kind"],
      ["?level=loud", "level"],
      ["?code=nope", "code"],
      ["?cursor=!!", "cursor"],
      [`?cursor=${Buffer.from("9.x").toString("base64url")}`, "cursor"],
      ["?limit=0", "limit"],
      ["?limit=101", "limit"],
      ["?q=", "q"],
      [`?q=${long}`, "q"],
      ["?from=-1", "from"],
      ["?actor=abc", "actor"],
    ]) {
      const r = await get(query);
      assert.equal(r.status, 400, `${query}: ${r.text}`);
      assert.deepEqual(r.json, { error: "input.invalid", field }, query);
    }
  });
});
