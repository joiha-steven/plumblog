// Shared by the cron subrequest tests (r2cron.test.mjs, subreq.test.mjs): fire the cron, seed
// worst-case rows, and read what each job counted against what it budgeted. Test builds log
// `job.subrequests <job> counted=N budgeted=M` per job (crates/worker/src/subrequests.rs `lap`)
// and one `job.run subrequests counted=N budgeted=M` per run, written last.
import assert from "node:assert/strict";
import { createServer } from "node:http";

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

/** A normal run (not in the daily slot) and the daily run: 03:12 UTC. */
export const DAILY = Date.UTC(2026, 10, 28, 3, 12);
export const NORMAL = DAILY + 3_600_000;
/**
 * Fires the cron once, as scheduled at `time` (ms; a normal run by default); the answer comes
 * after the handler finished. `time` decides whether the daily jobs run, so a test never
 * leaves it to the clock: only miniflare's own route takes it (wrangler's /__scheduled
 * ignores it and uses the current time, which is daily between 03:10 and 03:19 UTC).
 */
export async function cron(w, time = NORMAL) {
  const res = await fetch(`${w.url}/cdn-cgi/local/scheduled?cron=${encodeURIComponent("*/10 * * * *")}&time=${time}`);
  assert.equal(res.status, 200, await res.text());
}

/** A webhook receiver that answers 200 to everything. */
export function receiver() {
  const server = createServer((req, res) => {
    req.resume();
    req.on("end", () => res.writeHead(200).end("ok"));
  });
  return new Promise((ok) =>
    server.listen(0, "127.0.0.1", () =>
      ok({
        url: `http://127.0.0.1:${server.address().port}/hook`,
        close: () =>
          new Promise((done) => {
            server.closeAllConnections();
            server.close(done);
          }),
      }),
    ),
  );
}

/** Creates a webhook for `post.published` pointing at `recv`; returns its id. */
export async function createHook(w, recv) {
  const res = await fetch(`${w.url}/__webhooks/create?${new URLSearchParams({ url: recv.url, events: "post.published" })}`, { method: "POST" });
  assert.equal(res.status, 200, await res.text());
  return w.query("SELECT id FROM webhooks")[0].id;
}

/** Scheduled posts due a second ago, `n` of them. */
export function duePosts(w, n, name) {
  const now = Date.now();
  for (let i = 0; i < n; i++) {
    w.exec(
      "INSERT INTO posts (kind, translation_group, lang, slug, status, published_at, author_id, title, markdown, html, created_at, updated_at) " +
        `SELECT 'post', 0, 'en', '${name}-${i}', 'scheduled', ${now - 1000}, id, 'Due', 'x', '<p>x</p>', ${now}, ${now} FROM users ORDER BY id LIMIT 1; ` +
        `UPDATE posts SET translation_group = id WHERE slug = '${name}-${i}';`,
    );
  }
}

/** `n` deliveries due now for webhook `hook`. */
export function dueDeliveries(w, hook, n) {
  const now = Date.now();
  for (let i = 0; i < n; i++) {
    w.exec(`INSERT INTO webhook_deliveries (webhook_id, msg_id, event, payload, next_at_ms, created_at_ms) VALUES (${hook}, 'msg_${i}', 'post.published', '{}', ${now}, ${now})`);
  }
}

/** The n-th (1-based) `job.run subrequests` line of this Worker's log: the whole run. */
export async function subrequests(w, n) {
  for (let i = 0; i < 50; i++) {
    const all = [...w.log().matchAll(/job\.run subrequests counted=(\d+) budgeted=(\d+)/g)];
    if (all.length >= n) return { counted: Number(all[n - 1][1]), budgeted: Number(all[n - 1][2]) };
    await sleep(100);
  }
  assert.fail(`no subrequest line ${n} in the log`);
}

/** Each run's count for `job`, in order; read after [`subrequests`] has seen the run's last line. */
export const jobRuns = (w, job) =>
  [...w.log().matchAll(new RegExp(`job\\.subrequests ${job.replaceAll(".", "\\.")} counted=(\\d+) budgeted=(\\d+)`, "g"))].map((m) => ({
    counted: Number(m[1]),
    budgeted: Number(m[2]),
  }));

/**
 * Holds every run of one job to counted <= budgeted <= 50 and returns them. The guard
 * (subreq-guard.test.mjs) requires each job's `assertWithinBudget(jobRuns(w, "<job>"), "<job>")`.
 */
export function assertWithinBudget(runs, job) {
  assert.ok(runs.length > 0, `${job}: no count logged`);
  for (const [i, r] of runs.entries()) withinBudget(r, `${job} run ${i + 1}`);
  return runs;
}

/** A run holds to its budget, and the budget to the plan's 50. */
export function withinBudget(r, label) {
  assert.ok(r.counted <= r.budgeted, `${label}: counted ${r.counted} > budgeted ${r.budgeted}`);
  assert.ok(r.budgeted <= 50, `${label}: budgeted ${r.budgeted} > 50`);
}
