// The cron's R2 page work (crates/worker pages/prerender.rs): it pre-renders home, the newest
// posts, the feed and the sitemap for the running version id and content version, deletes
// retired groups and version ids within bounds, and marks each version id once. Each test
// starts its own Worker and seeds its own state; the tests read and seed miniflare's local R2
// files directly (r2.mjs).
import assert from "node:assert/strict";
import { test } from "node:test";

import { seedSample, startWorker } from "./harness.mjs";
import { DAILY, assertWithinBudget, cron, duePosts, jobRuns, receiver, subrequests } from "./subreq.mjs";
import { contentVersion, emptyCache, pageKeys, r2, r2Body, r2Row, seed, versionId } from "./r2.mjs";

const CACHE = "x-plumblog-cache";
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
async function read(w, path) {
  const res = await fetch(`${w.url}${path}`, { redirect: "manual" });
  return { res, body: await res.text() };
}
async function withWorker({ sample = false, vars = {} }, fn) {
  const w = await startWorker({ vars });
  try {
    if (sample) await seedSample(w);
    await fn(w);
  } finally {
    await w.stop();
  }
}
const doneKey = async (w) => `pages/${await versionId(w)}/.done-v${contentVersion(w)}`;
/** Runs the cron until this version's work is marked done; returns how many runs it took. */
async function cronUntilDone(w, max = 3) {
  const done = await doneKey(w);
  for (let i = 1; i <= max; i++) {
    await cron(w);
    if (r2Row(w, done)) return i;
  }
  assert.fail(`${done} not written after ${max} runs; pages in R2: ${pageKeys(w).join(", ")}`);
}
/** Every page object with its version, so any put (new or overwrite) shows. */
const snapshot = (w) =>
  r2(w, (db) => db.prepare("SELECT key, version FROM _mf_objects WHERE key LIKE 'pages/%' ORDER BY key").all().map((r) => `${r.key} ${r.version}`), []);
const deleteKeys = (w, keys) =>
  r2(w, (db) => {
    for (const k of keys) db.prepare("DELETE FROM _mf_objects WHERE key = ?").run(k);
  });
const bump = (w) => w.exec("UPDATE site SET content_version = content_version + 1 WHERE id = 1");
/** The default language's newest published posts, as the feed lists them. */
const newest = (w, n) =>
  w.query(`SELECT slug FROM posts WHERE status = 'published' AND kind = 'post' AND lang = 'en' ORDER BY published_at DESC, id DESC LIMIT ${n}`).map((r) => `/${r.slug}`);
async function storedRow(w, key) {
  for (let i = 0; i < 50; i++) {
    const row = r2Row(w, key);
    if (row) return row;
    await sleep(100);
  }
  assert.fail(`${key} never reached R2; pages in R2: ${pageKeys(w).join(", ")}`);
}
/** A reader miss on `/`, which stores it and the version's `.seen` marker. */
async function storeHome(w) {
  assert.equal((await read(w, "/")).res.headers.get(CACHE), "miss");
  const id = await versionId(w);
  await storedRow(w, `pages/${id}/v${contentVersion(w)}/`);
  await storedRow(w, `pages/${id}/.seen`);
}
test("after a content change the cron stores home, the 3 newest posts, the feed and the sitemap as a reader miss would, in 2 runs; a further run stores nothing", async () => {
  await withWorker({ sample: true }, async (w) => {
    bump(w);
    const id = await versionId(w);
    const key = (p) => `pages/${id}/v${contentVersion(w)}${p}`;
    const paths = ["/", ...newest(w, 3), "/feed.xml", "/sitemap.xml"];
    assert.equal(paths.length, 6);
    // What a reader miss renders and stores; then removed, so the cron must store each itself.
    const want = {};
    for (const p of paths) {
      const r = await read(w, p);
      assert.equal(r.res.status, 200, p);
      assert.equal(r.res.headers.get(CACHE), "miss", p);
      const row = await storedRow(w, key(p));
      assert.equal(r2Body(w, row), r.body, p);
      want[p] = { body: r.body, http: row.http_metadata, custom: row.custom_metadata };
    }
    deleteKeys(w, paths.map(key));
    emptyCache(w);

    // Cleanup runs first, then as many pages as the budget holds (home and 2 posts); the
    // second run heads those and stores the rest (pages/prerender.rs module docs).
    const runs = await cronUntilDone(w);
    console.log(`pre-render done in ${runs} run(s)`);
    assert.equal(runs, 2);
    for (const p of paths) {
      const row = r2Row(w, key(p));
      assert.ok(row, `${p} not pre-rendered`);
      assert.equal(r2Body(w, row), want[p].body, `${p} body`);
      assert.deepEqual(JSON.parse(row.http_metadata), JSON.parse(want[p].http), `${p} content type`);
      assert.deepEqual(JSON.parse(row.custom_metadata), JSON.parse(want[p].custom), `${p} ETag and Cache-Control`);
    }
    const before = snapshot(w);
    await cron(w);
    assert.deepEqual(snapshot(w), before, "a run after the done marker puts nothing");
  });
});

test("with no published posts the cron stores home, the feed and the sitemap only", async () => {
  await withWorker({}, async (w) => {
    await cronUntilDone(w);
    const group = `pages/${await versionId(w)}/v${contentVersion(w)}`;
    const pages = pageKeys(w).filter((k) => k.startsWith(`${group}/`)).map((k) => k.slice(group.length));
    assert.deepEqual(pages, ["/", "/feed.xml", "/sitemap.xml"]);
  });
});

test("the version marker is written once: neither a later miss nor cron runs overwrite it; the cron alone writes it too", async () => {
  await withWorker({}, async (w) => {
    await storeHome(w);
    const seen = `pages/${await versionId(w)}/.seen`;
    const first = r2Row(w, seen);
    bump(w);
    await storeHome(w);
    // Each run tries the conditional put again (no per-isolate flag on the cron path).
    await cronUntilDone(w);
    bump(w);
    await cronUntilDone(w);
    const now = r2Row(w, seen);
    assert.equal(now.version, first.version, "overwritten");
    assert.equal(now.uploaded, first.uploaded);
  });
  // No reader ever served this version.
  await withWorker({}, async (w) => {
    await cronUntilDone(w);
    assert.ok(r2Row(w, `pages/${await versionId(w)}/.seen`));
  });
});

test("cleanup keeps the running id and the 5 newest marked ids; deletes an older marked id and an unmarked one", async () => {
  await withWorker({}, async (w) => {
    await storeHome(w);
    const cv = contentVersion(w);
    const now = Date.now();
    const keep = [];
    const gone = [];
    // Six marked ids, newest first: the first five are kept.
    for (let i = 1; i <= 6; i++) {
      const fake = `fake-${i}`;
      const keys = [`pages/${fake}/.seen`, `pages/${fake}/v${cv}/`, `pages/${fake}/v${cv}/a`];
      seed(w, keys, now - i * 3_600_000);
      (i <= 5 ? keep : gone).push(...keys);
    }
    // No marker: counts as oldest, so it goes although it is newer than all of them.
    const unmarked = [`pages/unmarked/v${cv}/`, `pages/unmarked/v${cv}/b`];
    seed(w, unmarked);
    gone.push(...unmarked);
    const id = await versionId(w);
    const current = pageKeys(w).filter((k) => k.startsWith(`pages/${id}/`));

    await cronUntilDone(w);
    const after = pageKeys(w);
    console.log("after cleanup:", after.join(", "));
    for (const k of [...keep, ...current]) assert.ok(after.includes(k), `${k} was deleted`);
    for (const k of gone) assert.ok(!after.includes(k), `${k} was kept`);
    assert.ok(!after.some((k) => !k.startsWith(`pages/${id}/`) && !keep.includes(k)), "nothing else kept");
  });
});

test("dead content versions go by group: v1 goes, v10 and v11 stay (v1 is a prefix of both)", async () => {
  await withWorker({}, async (w) => {
    w.exec("UPDATE site SET content_version = 10 WHERE id = 1");
    await storeHome(w);
    const id = await versionId(w);
    const dead = [`pages/${id}/v1/`, `pages/${id}/v1/a`, `pages/${id}/v1-t0123456789abcdef/a`, `pages/${id}/.done-v1`];
    const live = [`pages/${id}/v10/x`, `pages/${id}/v11/x`];
    seed(w, [...dead, ...live]);
    await cronUntilDone(w);
    const after = pageKeys(w);
    console.log("after cleanup:", after.join(", "));
    for (const k of dead) assert.ok(!after.includes(k), `${k} was kept`);
    for (const k of live) assert.ok(after.includes(k), `${k} was deleted`);
  });
});

test("one run deletes at most the bound (10 here) of the running id's dead groups; the next runs continue, then it is done", async () => {
  await withWorker({ vars: { PLUMBLOG_TEST_PAGE_DELETES: "10" } }, async (w) => {
    await storeHome(w);
    const id = await versionId(w);
    const cv = contentVersion(w);
    const group = `pages/${id}/v${cv}/`;
    seed(w, Array.from({ length: 25 }, (_, i) => `${group}bulk-${String(i).padStart(2, "0")}`));
    // 26 dead keys: the 25 and the old home.
    bump(w);
    const deleted = [];
    for (let i = 0; i < 3; i++) {
      const before = pageKeys(w);
      await cron(w);
      const after = new Set(pageKeys(w));
      const gone = before.filter((k) => !after.has(k));
      assert.ok(gone.every((k) => k.startsWith(group)), gone.join(", "));
      deleted.push(gone.length);
      assert.equal(Boolean(r2Row(w, await doneKey(w))), i === 2, `done marker after run ${i + 1}`);
    }
    assert.deepEqual(deleted, [10, 10, 6]);
    assert.ok(!pageKeys(w).some((k) => k.startsWith(group)));
  });
});

test("an id whose listing is cut before its marker is not ranked, so not deleted as unmarked", async () => {
  await withWorker({}, async (w) => {
    await storeHome(w);
    const cv = contentVersion(w);
    // 1,001 live done markers sort before `.seen`: the id's one list call (1,000 keys) ends
    // before the marker.
    const many = Array.from({ length: 1001 }, (_, i) => `pages/many/.done-v${cv}-t${String(i).padStart(16, "0")}`);
    seed(w, [...many, `pages/many/.seen`, `pages/many/v${cv}/`]);
    for (let i = 0; i < 2; i++) await cron(w);
    const after = new Set(pageKeys(w));
    assert.ok(after.has("pages/many/.seen"), "marker deleted");
    assert.ok(after.has(`pages/many/v${cv}/`), "page deleted");
    assert.equal([...after].filter((k) => k.startsWith("pages/many/")).length, 1003);
  });
});

test("an R2 failure during the cron logs pages.prerender_failed; the publish job still publishes", async () => {
  await withWorker({ sample: true, vars: { PLUMBLOG_TEST_PAGE_BUCKET: "NO_SUCH_BUCKET" } }, async (w) => {
    duePosts(w, 1, "r2cron-due");
    await cron(w);
    assert.equal(w.query("SELECT status FROM posts WHERE slug = 'r2cron-due-0'")[0].status, "published");
    const jobs = w.query("SELECT error_code, detail FROM system_log WHERE kind = 'job.run' ORDER BY id");
    console.log("job.run:", JSON.stringify(jobs));
    assert.ok(jobs.some((j) => j.error_code === null && j.detail.startsWith("posts.publish_due\npublished\t1")));
    const failed = jobs.filter((j) => j.error_code === "pages.prerender_failed");
    assert.equal(failed.length, 1);
    assert.match(failed[0].detail, /^pages\.prerender\n/);
    for (let i = 0; i < 50 && !/pages\.prerender_failed job=pages\.prerender/.test(w.log()); i++) await sleep(100);
    assert.match(w.log(), /pages\.prerender_failed job=pages\.prerender failed=bucket/);
  });
});

test("a post rendered without its comments (not cacheable) is never stored, is marked handled, and does not hold back the done marker", async () => {
  await withWorker({ sample: true }, async (w) => {
    w.exec(
      "INSERT OR REPLACE INTO settings (key, value_json, updated_at) VALUES ('comments.enabled', 'true', 0); " +
        "UPDATE posts SET comments_enabled = 1; ALTER TABLE comments RENAME TO comments_gone",
    );
    bump(w);
    const posts = newest(w, 3);
    const r = await read(w, posts[0]);
    assert.equal(r.res.status, 200);
    assert.equal(r.res.headers.get(CACHE), "bypass", "the reader gets it uncached");
    const runs = await cronUntilDone(w);
    console.log(`done in ${runs} run(s)`);
    const id = await versionId(w);
    const group = `pages/${id}/v${contentVersion(w)}`;
    const pages = pageKeys(w).filter((k) => k.startsWith(`${group}/`)).map((k) => k.slice(group.length));
    assert.deepEqual(pages, ["/", "/feed.xml", "/sitemap.xml"], "shared pages stored, posts not");
    const skip = `pages/${id}/.skip-v${contentVersion(w)}`;
    const handled = pageKeys(w).filter((k) => k.startsWith(`${skip}/`)).map((k) => k.slice(skip.length));
    assert.deepEqual(handled.sort(), [...posts].sort(), "each post marked handled");
    // A further run renders nothing: no new comment read, no put.
    const reads = (w.log().match(/comments\.read_failed/g) ?? []).length;
    const before = snapshot(w);
    await cron(w);
    await subrequests(w, runs + 1);
    assert.equal((w.log().match(/comments\.read_failed/g) ?? []).length, reads, "a post was rendered again");
    assert.deepEqual(snapshot(w), before);
    // A reader still gets the post, rendered.
    const again = await read(w, posts[1]);
    assert.equal(again.res.status, 200);
    assert.equal(again.res.headers.get(CACHE), "bypass");
  });
});

test("worst-case runs make no more subrequests than they budget, and budget at most 50", async () => {
  const recv = await receiver();
  try {
    await withWorker({ sample: true }, async (w) => {
      // Comments open: the costliest post pages.
      w.exec("INSERT OR REPLACE INTO settings (key, value_json, updated_at) VALUES ('comments.enabled', 'true', 0); UPDATE posts SET comments_enabled = 1");
      const hook = await fetch(`${w.url}/__webhooks/create?${new URLSearchParams({ url: recv.url, events: "post.published" })}`, { method: "POST" });
      assert.equal(hook.status, 200, await hook.text());
      await storeHome(w);
      const id = await versionId(w);
      const cv = contentVersion(w);
      // Eight old marked ids (more lists, three to delete) and dead groups under the running id.
      for (let i = 1; i <= 8; i++) seed(w, [`pages/old-${i}/.seen`, `pages/old-${i}/v${cv}/`], Date.now() - i * 3_600_000);
      seed(w, Array.from({ length: 40 }, (_, i) => `pages/${id}/v${cv}/junk-${i}`));
      bump(w);
      // Due webhook deliveries and posts to publish, on the daily run (its prunes too).
      const now = Date.now();
      const webhookId = w.query("SELECT id FROM webhooks")[0].id;
      for (let i = 0; i < 4; i++) {
        w.exec(`INSERT INTO webhook_deliveries (webhook_id, msg_id, event, payload, next_at_ms, created_at_ms) VALUES (${webhookId}, 'msg_${i}', 'post.published', '{}', ${now}, ${now})`);
      }
      duePosts(w, 2, "budget-due");
      const runs = [];
      await cron(w, DAILY);
      runs.push(await subrequests(w, 1));
      await cron(w);
      runs.push(await subrequests(w, 2));
      console.log("subrequests per run:", JSON.stringify(runs));
      for (const [i, r] of runs.entries()) {
        assert.ok(r.counted <= r.budgeted, `run ${i + 1}: counted ${r.counted} > budgeted ${r.budgeted}`);
        assert.ok(r.budgeted <= 50, `run ${i + 1}: budgeted ${r.budgeted}`);
      }
      assert.ok(runs[0].counted > 30, `the first run is heavy: ${runs[0].counted}`);
      assert.equal(assertWithinBudget(jobRuns(w, "pages.prerender"), "pages.prerender").length, 2);
    });
  } finally {
    await recv.close();
  }
});

const published = (w, name) => Number(w.query(`SELECT count(*) AS n FROM posts WHERE slug LIKE '${name}-%' AND status = 'published'`)[0].n);

test("more due posts than the cap: the cap (14) is published, the rest next run; calls per post are measured, not assumed", async () => {
  // webhooks::PUBLISH_PER_RUN and PER_PUBLISHED.
  const CAP = 14;
  const PER_PUBLISHED = 3;
  const recv = await receiver();
  try {
    await withWorker({ sample: true }, async (w) => {
      const hook = await fetch(`${w.url}/__webhooks/create?${new URLSearchParams({ url: recv.url, events: "post.published" })}`, { method: "POST" });
      assert.equal(hook.status, 200, await hook.text());
      duePosts(w, CAP + 2, "budget-many");
      await cron(w);
      const run1 = await subrequests(w, 1);
      assert.equal(published(w, "budget-many"), CAP);
      await cron(w);
      const run2 = await subrequests(w, 2);
      assert.equal(published(w, "budget-many"), CAP + 2);
      const pub = assertWithinBudget(jobRuns(w, "posts.publish_due"), "posts.publish_due");
      console.log("runs:", JSON.stringify([run1, run2]), "publish job:", JSON.stringify(pub));
      for (const r of [run1, run2]) {
        assert.ok(r.counted <= r.budgeted, `counted ${r.counted} > budgeted ${r.budgeted}`);
        assert.ok(r.budgeted <= 50, `budgeted ${r.budgeted}`);
      }
      assert.equal(pub.length, 2);
      // Same fixed calls in both runs, so the difference is the posts': 14 against 2.
      const perPost = (pub[0].counted - pub[1].counted) / (CAP - 2);
      assert.equal(perPost, PER_PUBLISHED, "calls per published post");
      for (const p of pub) assert.equal(p.counted, p.budgeted, "the publish job counts exactly what it calls");
    });
  } finally {
    await recv.close();
  }
});
