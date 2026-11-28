// Outgoing webhooks on the real Worker and local D1, to a local receiver that checks the
// Standard Webhooks signature with node:crypto (independent of the Rust code that signs).
// Webhooks are created and events emitted through the test-only /__webhooks/* routes
// (crates/worker/src/webhooks/test_routes.rs); retries and publishing go through cron.
import assert from "node:assert/strict";
import { createHmac, timingSafeEqual } from "node:crypto";
import { createServer } from "node:http";
import { after, before, beforeEach, test } from "node:test";
import { d1Sql, startWorker } from "./harness.mjs";

const MIN_MS = 60_000;
let worker;
let recv;

/** Records every request; answers by path. `/hang` never answers. */
function startReceiver() {
  const hits = [];
  const server = createServer((req, res) => {
    let body = "";
    req.on("data", (d) => (body += d));
    req.on("end", () => {
      hits.push({ path: req.url, headers: req.headers, body, at: Date.now() });
      if (req.url === "/fail") return res.writeHead(500).end("no");
      if (req.url === "/redirect") {
        return res.writeHead(302, { location: `http://127.0.0.1:${server.address().port}/redirected` }).end();
      }
      if (req.url === "/hang") return; // never answers
      res.writeHead(200).end("ok");
    });
  });
  return new Promise((ok) =>
    server.listen(0, "127.0.0.1", () =>
      ok({
        hits,
        url: (path) => `http://127.0.0.1:${server.address().port}${path}`,
        close: () => new Promise((done) => { server.closeAllConnections(); server.close(done); }),
      }),
    ),
  );
}

/** Standard Webhooks verification, as a receiver would write it. */
function verify(hit, secret) {
  const id = hit.headers["webhook-id"];
  const ts = hit.headers["webhook-timestamp"];
  const key = Buffer.from(secret.replace(/^whsec_/, ""), "base64");
  const want = createHmac("sha256", key).update(`${id}.${ts}.${hit.body}`).digest();
  const fresh = Math.abs(Date.now() / 1000 - Number(ts)) <= 300;
  const ok = (hit.headers["webhook-signature"] ?? "").split(" ").some((part) => {
    const [v, sig] = part.split(",");
    const got = Buffer.from(sig ?? "", "base64");
    return v === "v1" && got.length === want.length && timingSafeEqual(got, want);
  });
  return ok && fresh;
}

function sql(command) {
  return d1Sql(worker.state, command);
}

async function post(path, params = {}) {
  const res = await fetch(`${worker.url}${path}?${new URLSearchParams(params)}`, { method: "POST" });
  return { status: res.status, body: await res.text() };
}

async function until(what, fn, ms = 15_000) {
  const end = Date.now() + ms;
  for (;;) {
    const v = await fn();
    if (v) return v;
    if (Date.now() > end) throw new Error(`timed out waiting for ${what}`);
    await new Promise((r) => setTimeout(r, 100));
  }
}

const deliveries = () =>
  sql("SELECT id, webhook_id, msg_id, event, attempts, next_at_ms, last_status, last_error FROM webhook_deliveries ORDER BY id");
const deliveryLog = () => sql("SELECT kind, error_code, detail FROM system_log WHERE kind = 'webhook.delivery' ORDER BY id");
// The Worker saves the delivery row first and logs the outcome after it, so a read of the log
// right after the row appears can come first. Wait for the entries the test expects (the
// assertions after it still check the exact count).
const deliveryLogAfter = (n) => until(`${n} delivery log entries`, () => {
  const rows = deliveryLog();
  return rows.length >= n && rows;
});
const finished = (row) => row && row.next_at_ms === null;

async function create(path, events = "post.published", params = {}) {
  const r = await post("/__webhooks/create", { url: recv.url(path), events, ...params });
  assert.equal(r.status, 200, r.body);
  return JSON.parse(r.body);
}

let postId;
before(async () => {
  worker = await startWorker();
  recv = await startReceiver();
  const seeded = await post("/__seed");
  assert.equal(seeded.status, 200, seeded.body);
  postId = sql("SELECT id FROM posts WHERE status = 'published' ORDER BY id LIMIT 1")[0].id;
});
after(async () => {
  await recv?.close();
  await worker?.stop();
});
beforeEach(() => {
  sql("DELETE FROM webhook_deliveries; DELETE FROM webhooks; DELETE FROM system_log;");
  recv.hits.length = 0;
});

test("success: one signed request, row delivered, one log entry with the host only", async () => {
  const hook = await create("/ok");
  assert.match(hook.secret, /^whsec_[A-Za-z0-9+/]{43}=$/);
  const r = await post("/__webhooks/emit", { event: "post.published", post: postId });
  assert.equal(r.status, 200, r.body);
  const row = await until("delivered row", () => finished(deliveries()[0]) && deliveries()[0]);
  await new Promise((res) => setTimeout(res, 500)); // nothing more may arrive
  assert.equal(recv.hits.length, 1, "exactly one request");
  const hit = recv.hits[0];
  const body = JSON.parse(hit.body);
  console.log("received headers:", JSON.stringify({ ...hit.headers, "webhook-signature": "(checked)" }));
  console.log("received body:", hit.body);
  console.log("signature valid (node:crypto):", verify(hit, hook.secret));
  console.log("delivery row:", JSON.stringify(row));
  assert.ok(verify(hit, hook.secret), "Standard Webhooks signature");
  assert.equal(hit.headers["content-type"], "application/json");
  assert.equal(hit.headers["webhook-id"], row.msg_id);
  assert.match(row.msg_id, /^msg_[0-9a-f]{32}$/);
  assert.deepEqual(Object.keys(body).sort(), ["created_at", "data", "id", "type"]);
  assert.equal(body.id, row.msg_id);
  assert.equal(body.type, "post.published");
  assert.equal(body.data.post_id, postId);
  assert.match(body.data.url, /^https:\/\/blog\.test\/[a-z]+\/[a-z0-9-]+$/);
  assert.equal(body.data.previous_url, undefined);
  assert.deepEqual([row.attempts, row.last_status, row.last_error], [1, 200, null]);

  const log = await deliveryLogAfter(1);
  console.log("system_log webhook.delivery:", JSON.stringify(log));
  assert.equal(log.length, 1);
  assert.equal(log[0].error_code, null);
  assert.match(log[0].detail, /\nhost\t/);
  assert.ok(!log[0].detail.includes("/ok") && !log[0].detail.includes("http"), "no URL or path");
  const b64 = hook.secret.slice("whsec_".length);
  assert.ok(!log[0].detail.includes(b64), "no secret");

  const stored = sql("SELECT secret FROM webhooks")[0].secret;
  console.log("webhooks.secret in D1:", stored);
  assert.match(stored, /^v1:/);
  assert.ok(!stored.includes(b64), "stored sealed, not in clear");
});

test("failure then retry: 500 schedules +10 min; cron resends the same id and body", async () => {
  const hook = await create("/fail");
  await post("/__webhooks/emit", { event: "post.published", post: postId });
  const first = await until("attempt 1", () => {
    const d = deliveries()[0];
    return d && d.attempts === 1 && d.last_status === 500 && d;
  });
  const delay = first.next_at_ms - Date.now();
  console.log("after attempt 1:", JSON.stringify(first), `next in ${Math.round(delay / 1000)} s`);
  assert.equal(first.last_error, "status");
  assert.ok(delay > 9 * MIN_MS && delay <= 10 * MIN_MS, `next_at about +10 min, got ${delay} ms`);
  const log1 = await deliveryLogAfter(1);
  console.log("system_log after attempt 1:", JSON.stringify(log1));
  assert.equal(log1.length, 1);
  assert.equal(log1[0].error_code, "webhook.delivery_failed");
  assert.match(log1[0].detail, /\nreason\tstatus/);

  sql(`UPDATE webhook_deliveries SET next_at_ms = ${Date.now()} WHERE id = ${first.id}`);
  const res = await fetch(`${worker.url}/__scheduled?cron=${encodeURIComponent("*/10 * * * *")}`);
  assert.equal(res.status, 200);
  await until("attempt 2", () => recv.hits.length >= 2);
  const second = deliveries()[0];
  console.log("after attempt 2:", JSON.stringify(second));
  const [a, b] = recv.hits;
  assert.equal(b.headers["webhook-id"], a.headers["webhook-id"], "same webhook-id for dedup");
  assert.equal(b.body, a.body, "same bytes");
  assert.ok(verify(b, hook.secret));
  assert.equal(second.attempts, 2);
  const delay2 = second.next_at_ms - Date.now();
  assert.ok(delay2 > 55 * MIN_MS && delay2 <= 60 * MIN_MS, `then +1 h, got ${delay2} ms`);
});

test("redirect: a 302 is not followed and counts as a failure", async () => {
  await create("/redirect");
  await post("/__webhooks/emit", { event: "post.published", post: postId });
  const row = await until("attempt 1", () => {
    const d = deliveries()[0];
    return d && d.attempts === 1 && d.last_error && d;
  });
  await new Promise((r) => setTimeout(r, 300));
  console.log("redirect row:", JSON.stringify(row), "paths hit:", recv.hits.map((h) => h.path));
  assert.deepEqual([row.last_status, row.last_error], [302, "redirect"]);
  assert.deepEqual(recv.hits.map((h) => h.path), ["/redirect"], "/redirected never hit");
});

test("timeout: a receiver that never answers ends as timeout after about 10 s", async () => {
  await create("/hang");
  const start = Date.now();
  await post("/__webhooks/emit", { event: "post.published", post: postId });
  const row = await until("timeout", () => {
    const d = deliveries()[0];
    return d && d.last_error && d;
  }, 25_000);
  const took = Date.now() - start;
  console.log("timeout row:", JSON.stringify(row), `after ${took} ms`);
  assert.equal(row.last_error, "timeout");
  assert.equal(row.last_status, null);
  assert.ok(took >= 9_500 && took < 20_000, `took ${took} ms`);
});

test("SSRF: refused URLs save nothing; a tampered row is checked again before sending", async () => {
  const refused = [
    "https://localhost./",
    "https://127.0.0.1/",
    "http://example.com/",
    "https://[::ffff:127.0.0.1]/",
    recv.url("/ok"), // the test receiver itself, under the production policy
  ];
  for (const url of refused) {
    const r = await post("/__webhooks/create", { url, events: "post.published", policy: "production" });
    console.log(`create ${url} (production policy) ->`, r.status, r.body);
    assert.equal(r.status, 400);
    assert.equal(JSON.parse(r.body).error, "webhook.url_blocked");
  }
  assert.equal(sql("SELECT count(*) AS n FROM webhooks")[0].n, 0, "nothing saved");

  // A row changed in D1 after creation: the production policy refuses it before any fetch.
  const hook = await create("/ok");
  sql(`UPDATE webhooks SET url = '${recv.url("/tampered")}' WHERE id = ${hook.id}`);
  const now = Date.now();
  sql(
    "INSERT INTO webhook_deliveries (webhook_id, msg_id, event, payload, next_at_ms, created_at_ms) " +
      `VALUES (${hook.id}, 'msg_tampered', 'post.published', '{}', ${now}, ${now})`,
  );
  const run = await post("/__webhooks/deliver", { policy: "production" });
  const row = deliveries()[0];
  console.log("production run:", run.body, "row:", JSON.stringify(row));
  assert.equal(row.last_error, "url_blocked");
  assert.equal(recv.hits.length, 0, "receiver never contacted");

  // The cron path (test policy here, which still refuses everything but http://127.0.0.1).
  sql(`UPDATE webhooks SET url = 'https://169.254.169.254/latest/meta-data' WHERE id = ${hook.id}`);
  sql(`UPDATE webhook_deliveries SET next_at_ms = ${Date.now()}, last_error = NULL WHERE id = ${row.id}`);
  const res = await fetch(`${worker.url}/__scheduled?cron=${encodeURIComponent("*/10 * * * *")}`);
  assert.equal(res.status, 200);
  const cronRow = await until("cron attempt", () => {
    const d = deliveries()[0];
    return d.last_error && d;
  });
  const log = await deliveryLogAfter(2);
  console.log("cron row:", JSON.stringify(cronRow), "log:", JSON.stringify(log));
  assert.equal(cronRow.last_error, "url_blocked");
  assert.equal(log.length, 2, "one entry per refused attempt (production run, then cron)");
  assert.ok(log.every((l) => l.error_code === "webhook.url_blocked" && /\nurl_rule\t/.test(l.detail)));
  assert.ok(!log.some((l) => l.detail.includes("169.254") || l.detail.includes("tampered")));
});

test("cron publish_due: a scheduled post that is due sends post.published", async () => {
  const hook = await create("/ok");
  const now = Date.now();
  sql(
    "INSERT INTO posts (kind, translation_group, lang, slug, status, published_at, author_id, title, markdown, html, created_at, updated_at) " +
      `SELECT 'post', 0, 'en', 'due-now', 'scheduled', ${now - 1000}, id, 'Due now', 'x', '<p>x</p>', ${now}, ${now} FROM users ORDER BY id LIMIT 1; ` +
      "UPDATE posts SET translation_group = id WHERE slug = 'due-now';",
  );
  const res = await fetch(`${worker.url}/__scheduled?cron=${encodeURIComponent("*/10 * * * *")}`);
  assert.equal(res.status, 200);
  const hit = await until("post.published", () => recv.hits.find((h) => JSON.parse(h.body).data.slug === "due-now"));
  const body = JSON.parse(hit.body);
  const status = sql("SELECT status FROM posts WHERE slug = 'due-now'")[0].status;
  const job = sql("SELECT error_code, detail FROM system_log WHERE kind = 'job.run' ORDER BY id");
  console.log("cron delivered:", hit.body);
  console.log("post status:", status, "job.run:", JSON.stringify(job));
  assert.ok(verify(hit, hook.secret));
  assert.equal(body.type, "post.published");
  assert.equal(body.data.url, "https://blog.test/en/due-now");
  assert.equal(status, "published");
  assert.ok(job.some((j) => j.detail.startsWith("posts.publish_due\npublished\t1")));
});
