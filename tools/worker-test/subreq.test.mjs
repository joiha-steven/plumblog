// Subrequest budgets of the cron jobs besides publishing and page pre-render (those are in
// r2cron.test.mjs): webhook delivery, the daily retention and prune jobs, and the whole daily
// slot with everything at once. Free plan: 50 subrequests per run, D1, R2 and fetch each one.
// Each job holds counted <= budgeted <= 50, and where it has a per-item cost the test derives
// the cost from two runs and holds it to the constant in crates/worker/src/webhooks/mod.rs.
import assert from "node:assert/strict";
import { test } from "node:test";

import { seedSample, startWorker } from "./harness.mjs";
import { contentVersion, seed, versionId } from "./r2.mjs";
import { DAILY, NORMAL, assertWithinBudget, createHook, cron, dueDeliveries, duePosts, jobRuns, receiver, subrequests, withinBudget } from "./subreq.mjs";

// webhooks::DELIVERY_FIXED, PER_DELIVERY, RETENTION_USED, PRUNE_USED, PUBLISH_PER_RUN,
// PER_PUBLISHED, PUBLISH_FIXED.
const DELIVERY_FIXED = 2;
const PER_DELIVERY = 3;
const RETENTION_USED = 2;
const PRUNE_USED = 2;
const PUBLISH_CAP = 14;
// system_log Retention::default().per_run and webhook::PRUNE_PER_RUN.
const LOG_PER_RUN = 2000;
const PRUNE_PER_RUN = 1000;

async function withWorker(fn) {
  const recv = await receiver();
  const w = await startWorker({});
  try {
    await seedSample(w);
    await fn(w, recv);
  } finally {
    await w.stop();
    await recv.close();
  }
}
/** `n` rows far past retention (1970), as a backlog the daily jobs cannot clear in one run. */
function oldRows(w, hook, logRows, deliveryRows) {
  w.exec(
    `WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ${logRows}) ` +
      "INSERT INTO system_log (at_ms, request_id, kind, detail) SELECT 1000 + i, 'seed', 'job.run', 'seed' FROM n",
  );
  w.exec(
    `WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ${deliveryRows}) ` +
      `INSERT INTO webhook_deliveries (webhook_id, msg_id, event, payload, next_at_ms, created_at_ms) SELECT ${hook}, 'old_' || i, 'post.published', '{}', NULL, 1000 + i FROM n`,
  );
}
const deleted = (w, job) => [...w.log().matchAll(new RegExp(`job\\.run ${job.replace(".", "\\.")} deleted=(\\d+)`, "g"))].map((m) => Number(m[1]));

test("webhook delivery with more due rows than fit: sends what the budget allows, each delivery counts PER_DELIVERY", async () => {
  await withWorker(async (w, recv) => {
    const hook = await createHook(w, recv);
    dueDeliveries(w, hook, 20);
    for (const n of [1, 2, 3]) {
      await cron(w);
      withinBudget(await subrequests(w, n), `run ${n}`);
    }
    const runs = assertWithinBudget(jobRuns(w, "webhook_deliveries.send"), "webhook_deliveries.send");
    console.log("webhook_deliveries.send:", JSON.stringify(runs));
    assert.equal(runs.length, 3);
    // A normal run has 1 call used before deliveries: (50 - 1 - 2) / 3 = 15 claimed, then 5, then none.
    assert.deepEqual(runs.map((r) => r.budgeted), [DELIVERY_FIXED + 15 * PER_DELIVERY, DELIVERY_FIXED + 5 * PER_DELIVERY, DELIVERY_FIXED]);
    // With rows claimed it counts exactly what it calls; with none the claim alone is made,
    // one under the fixed cost it budgets (it does not know that before claiming).
    for (const r of runs.slice(0, 2)) assert.equal(r.counted, r.budgeted, "the delivery job counts exactly what it calls");
    assert.equal(runs[2].counted, DELIVERY_FIXED - 1, "nothing due: the claim only");
    assert.equal((runs[0].counted - runs[1].counted) / 10, PER_DELIVERY, "calls per delivery");
    assert.equal(w.query("SELECT count(*) AS n FROM webhook_deliveries WHERE next_at_ms IS NULL")[0].n, 20);
  });
});

test("daily retention and prune jobs at their per-run bounds: each counts its constant", async () => {
  await withWorker(async (w, recv) => {
    const hook = await createHook(w, recv);
    oldRows(w, hook, LOG_PER_RUN + 300, PRUNE_PER_RUN + 200);
    await cron(w, DAILY);
    withinBudget(await subrequests(w, 1), "daily run");
    // The bound is what makes the cost fixed: it deletes the most rows it may, in one query.
    assert.deepEqual(deleted(w, "system_log.retention"), [LOG_PER_RUN]);
    assert.deepEqual(deleted(w, "webhook_deliveries.retention"), [PRUNE_PER_RUN]);
    const retention = assertWithinBudget(jobRuns(w, "system_log.retention"), "system_log.retention");
    const prune = assertWithinBudget(jobRuns(w, "webhook_deliveries.retention"), "webhook_deliveries.retention");
    assert.deepEqual(retention, [{ counted: RETENTION_USED, budgeted: RETENTION_USED }]);
    assert.deepEqual(prune, [{ counted: PRUNE_USED, budgeted: PRUNE_USED }]);
    // The backlog drains over the next daily runs at the same cost.
    await cron(w, DAILY + 86_400_000);
    withinBudget(await subrequests(w, 2), "second daily run");
    assert.deepEqual(deleted(w, "system_log.retention"), [LOG_PER_RUN, 300]);
    assert.deepEqual(deleted(w, "webhook_deliveries.retention"), [PRUNE_PER_RUN, 200]);
    for (const r of [...jobRuns(w, "system_log.retention"), ...jobRuns(w, "webhook_deliveries.retention")]) assert.equal(r.counted, r.budgeted);
  });
});

test("the whole daily slot with everything at once: every job and the run hold their budget", async () => {
  for (const posts of [PUBLISH_CAP + 2, 5]) {
    await withWorker(async (w, recv) => {
      w.exec("INSERT OR REPLACE INTO settings (key, value_json, updated_at) VALUES ('comments.enabled', 'true', 0); UPDATE posts SET comments_enabled = 1");
      const hook = await createHook(w, recv);
      const id = await versionId(w);
      const cv = contentVersion(w);
      for (let i = 1; i <= 8; i++) seed(w, [`pages/old-${i}/.seen`, `pages/old-${i}/v${cv}/`], Date.now() - i * 3_600_000);
      seed(w, Array.from({ length: 40 }, (_, i) => `pages/${id}/v${cv}/junk-${i}`));
      dueDeliveries(w, hook, 30);
      oldRows(w, hook, LOG_PER_RUN + 300, PRUNE_PER_RUN + 200);
      duePosts(w, posts, "slot-due");
      await cron(w, DAILY);
      await cron(w, NORMAL);
      for (const n of [1, 2]) {
        const run = await subrequests(w, n);
        console.log(`slot posts=${posts} run ${n}:`, JSON.stringify(run));
        withinBudget(run, `posts=${posts} run ${n}`);
      }
      const perJob = {
        "posts.publish_due": assertWithinBudget(jobRuns(w, "posts.publish_due"), "posts.publish_due"),
        "system_log.retention": assertWithinBudget(jobRuns(w, "system_log.retention"), "system_log.retention"),
        "webhook_deliveries.retention": assertWithinBudget(jobRuns(w, "webhook_deliveries.retention"), "webhook_deliveries.retention"),
        "webhook_deliveries.send": assertWithinBudget(jobRuns(w, "webhook_deliveries.send"), "webhook_deliveries.send"),
        "pages.prerender": assertWithinBudget(jobRuns(w, "pages.prerender"), "pages.prerender"),
      };
      for (const [job, runs] of Object.entries(perJob)) {
        console.log(job, JSON.stringify(runs));
        // The daily jobs run once, in the 03:12 run.
        assert.equal(runs.length, job.endsWith(".retention") ? 1 : 2, `${job}: one count per run it ran in`);
        for (const r of runs) assert.ok(r.counted <= r.budgeted, `${job}: counted ${r.counted} > budgeted ${r.budgeted}`);
      }
      for (const p of perJob["posts.publish_due"]) assert.equal(p.counted, p.budgeted, "the publish job counts exactly what it calls");
    });
  }
});
