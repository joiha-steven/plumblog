// Rendered pages in R2 (crates/worker pages/cache.rs store_key, read by js/entry.mjs): a wasm
// miss stores the page under pages/<version id>/v<content version><theme tag><path>, and a
// Cache API miss (it is per data centre) is then served from R2 by the JS entry, without wasm.
// The tests read and edit miniflare's local R2 and Cache API files directly (assertions only):
// emptying the Cache API stands in for "another data centre".
import assert from "node:assert/strict";
import { createHash, randomBytes } from "node:crypto";
import { after, before, describe, test } from "node:test";

import { seedSample, startWorker } from "./harness.mjs";
import { cacheKeys, contentVersion, emptyCache, pageKeys, r2Body, r2Row, renameR2, versionId } from "./r2.mjs";

const MARK = "x-plumblog-entry";
const CACHE = "x-plumblog-cache";
const WASM = "x-plumblog-wasm";
const PATH_FAIL = "/reading-on-screens";
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const get = (w, path, init = {}) => fetch(`${w.url}${path}`, { redirect: "manual", ...init });
async function read(w, path, init) {
  const res = await get(w, path, init);
  return { res, body: await res.text() };
}

const keyOf = async (w, path) => `pages/${await versionId(w)}/v${contentVersion(w)}${path}`;

/** The R2 row once the put (after the response) lands. */
async function stored(w, key) {
  for (let i = 0; i < 50; i++) {
    const row = r2Row(w, key);
    if (row) return row;
    await sleep(100);
  }
  assert.fail(`${key} never reached R2; pages in R2: ${pageKeys(w).join(", ")}`);
}

/** Waits for a Cache API entry (written after the response). */
async function cached(w, key) {
  for (let i = 0; i < 50; i++) {
    if (cacheKeys(w).includes(key)) return;
    await sleep(100);
  }
  assert.fail(`${key} never reached the Cache API`);
}

async function hit(w, path) {
  for (let i = 0; i < 30; i++) {
    const r = await read(w, path);
    if (r.res.headers.get(CACHE) === "hit") return r;
    await sleep(100);
  }
  assert.fail(`${path} never came from cache`);
}

/** Headers a reader sees, minus the ones that say who answered and how (and transport ones). */
const IGNORED = new Set([CACHE, MARK, WASM, "server-timing", "date", "age", "cf-cache-status", "content-length", "transfer-encoding", "content-encoding"]);
const headersOf = (res) => Object.fromEntries([...res.headers].filter(([k]) => !IGNORED.has(k)));

const DAY = 86_400_000;
const T = Date.now();
const b64u = (b) => b.toString("base64url");
const session = b64u(randomBytes(32));
const token = `plb_${b64u(randomBytes(32))}`;
const sha = (s) => createHash("sha256").update(s).digest("hex");
const ownerCookie = { cookie: `__Host-plumblog_session=${session}` };
const ownerBearer = { authorization: `Bearer ${token}` };
function addOwner(w) {
  w.exec(
    [
      `INSERT INTO users (id, login, slug, display_name, role, password_hash, disabled_at, created_at, updated_at) VALUES (900, 'r2owner', 'r2owner', 'R2 Owner', 'owner', NULL, NULL, ${T}, ${T});`,
      `INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES ('${sha(session)}', 900, ${T}, ${T + DAY});`,
      `INSERT INTO api_tokens (id, user_id, name, token_hash, scope, created_at, expires_at, revoked_at) VALUES (900, 900, 'r2', '${sha(token)}', 'owner', ${T}, NULL, NULL);`,
    ].join("\n"),
  );
}
async function api(w, method, path, body) {
  const headers = { ...ownerBearer };
  if (body !== undefined) headers["content-type"] = "application/json";
  const res = await fetch(w.url + path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) });
  const text = await res.text();
  return { status: res.status, json: text ? JSON.parse(text) : null, text };
}
async function newPost(w, slug) {
  const r = await api(w, "POST", "/api/v1/posts", { lang: "en", slug, title: `Title ${slug}`, markdown: `Body of ${slug}.` });
  assert.equal(r.status, 201, r.text);
  return r.json;
}
async function patch(w, post, fields) {
  const r = await api(w, "PATCH", `/api/v1/posts/${post.id}`, { expected_version: post.version, ...fields });
  assert.equal(r.status, 200, r.text);
  return r.json;
}

describe("R2 page store", () => {
  let w;
  before(async () => {
    w = await startWorker();
    await seedSample(w);
    addOwner(w);
  });
  after(async () => {
    await w?.stop();
  });

  const PATH = "/reading-on-screens";
  let miss;

  test("a miss stores the page in R2: the response's bytes, content type, ETag and Cache-Control", async () => {
    miss = await read(w, PATH);
    assert.equal(miss.res.status, 200);
    assert.equal(miss.res.headers.get(CACHE), "miss");
    const row = await stored(w, await keyOf(w, PATH));
    assert.equal(Number(row.size), Buffer.byteLength(miss.body));
    assert.equal(r2Body(w, row), miss.body);
    assert.deepEqual(JSON.parse(row.http_metadata), { contentType: miss.res.headers.get("content-type") });
    assert.deepEqual(JSON.parse(row.custom_metadata), {
      etag: miss.res.headers.get("etag"),
      "cache-control": miss.res.headers.get("cache-control"),
    });
  });

  test("on a Cache API miss the entry serves the R2 copy, byte-identical, and the next request is a Cache API hit", async () => {
    emptyCache(w);
    const r = await read(w, PATH);
    assert.equal(r.res.status, 200);
    assert.equal(r.res.headers.get(CACHE), "store");
    assert.equal(r.res.headers.get(MARK), "js");
    assert.equal(r.body, miss.body, "body");
    assert.deepEqual(headersOf(r.res), headersOf(miss.res), "headers");
    const h = await hit(w, PATH);
    assert.equal(h.res.headers.get(MARK), "js");
    assert.equal(h.body, miss.body);
    assert.deepEqual(headersOf(h.res), headersOf(miss.res));
  });

  test("If-None-Match with the R2 copy's ETag is a 304 from the entry, ETag only; HEAD goes to wasm", async () => {
    emptyCache(w);
    const etag = miss.res.headers.get("etag");
    const res = await get(w, PATH, { headers: { "if-none-match": etag } });
    assert.equal(res.status, 304);
    assert.equal(res.headers.get(MARK), "js");
    assert.equal(res.headers.get("etag"), etag);
    assert.equal(await res.text(), "");
    for (const k of ["content-type", "cache-control", CACHE]) assert.equal(res.headers.get(k), null, k);
    const head = await get(w, PATH, { method: "HEAD" });
    assert.equal(head.status, 200);
    assert.equal(head.headers.get(MARK), null, "HEAD never reads R2");
    assert.equal(head.headers.get(CACHE), "miss");
    // Another ETag: the head does not match, so the get serves the page. The HEAD's render
    // lands in the Cache API after the response; wait for it before emptying.
    await cached(w, `${w.url}/__page/${await versionId(w)}/v${contentVersion(w)}${PATH}`);
    emptyCache(w);
    const other = await read(w, PATH, { headers: { "if-none-match": `"0-0"` } });
    assert.equal(other.res.status, 200);
    assert.equal(other.res.headers.get(CACHE), "store");
    assert.equal(other.body, miss.body);
  });

  test("never stored: drafts, a Cookie or Authorization, HEAD, search, admin, API, MCP, 404s", async () => {
    await newPost(w, "r2-draft");
    const v = contentVersion(w);
    const id = await versionId(w);
    const cacheKey = (p) => `${w.url}/__page/${id}/v${v}${p}`;
    // [path, init, status, whether the Cache API keeps it]. A kept page proves its R2 step is
    // over (pages::cache::put_later puts to R2 first); the others write nothing at all.
    const cases = [
      ["/r2-draft", {}, 404, false],
      ["/r2-draft", { headers: ownerCookie }, 404, false],
      ["/r2-draft", { headers: ownerBearer }, 404, false],
      ["/tag/tools", { headers: ownerCookie }, 200, true],
      ["/archive/2025", { headers: ownerBearer }, 200, true],
      ["/page/2", { method: "HEAD" }, 200, true],
      ["/search?q=plum", {}, 200, false],
      ["/admin/", {}, null, false],
      ["/api/v1/site", {}, null, false],
      ["/mcp", {}, null, false],
      ["/no-such-post", {}, 404, false],
    ];
    for (const [p, init, status, kept] of cases) {
      const r = await read(w, p, init);
      if (status !== null) assert.equal(r.res.status, status, p);
      assert.notEqual(r.res.headers.get(CACHE), "store", p);
      if (kept) await cached(w, cacheKey(p));
    }
    const keys = pageKeys(w);
    for (const [p] of cases) {
      const path = p.split("?")[0];
      assert.ok(!keys.some((k) => k.endsWith(`/v${v}${path}`)), `${p} reached R2: ${keys.join(", ")}`);
    }
    assert.ok(!keys.some((k) => /\/(admin|api|mcp|search)/.test(k)), keys.join(", "));
    // The Cache API kept the credentialed render (reader pages never vary by viewer): an
    // anonymous request is a Cache API hit, and hits never write to R2.
    const anon = await read(w, "/tag/tools");
    assert.equal(anon.res.headers.get(CACHE), "hit");
    assert.equal(anon.res.headers.get(MARK), "js");
    assert.ok(!pageKeys(w).some((k) => k.endsWith(`/v${v}/tag/tools`)));
  });

  test("a path whose R2 key is over 1,024 bytes is never looked up: a wasm 404, no warning", async () => {
    const path = `/${"a".repeat(1100)}`;
    const r = await read(w, path);
    assert.equal(r.res.status, 404);
    assert.equal(r.res.headers.get(MARK), null);
    // A marker logged by a later request: once it is in the log, this request's lines are too.
    const ok = await read(w, "/r2-log-marker");
    assert.equal(ok.res.status, 404);
    for (let i = 0; i < 50 && !/GET \/r2-log-marker 404/.test(w.log()); i++) await sleep(100);
    assert.match(w.log(), /GET \/r2-log-marker 404/);
    assert.doesNotMatch(w.log(), /fast path failed/);
  });

  test("unpublish, slug change, settings change: stored copies are never served stale", async () => {
    // Unpublish.
    let p = await newPost(w, "r2-pub");
    p = await patch(w, p, { status: "published" });
    assert.match((await read(w, "/")).body, /r2-pub/);
    assert.equal((await read(w, "/r2-pub")).res.status, 200);
    const oldPage = await stored(w, await keyOf(w, "/r2-pub"));
    const oldHome = await stored(w, await keyOf(w, "/"));
    p = await patch(w, p, { status: "draft", published_at: null });
    emptyCache(w);
    const gone = await read(w, "/r2-pub");
    assert.equal(gone.res.status, 404);
    assert.equal(gone.res.headers.get(MARK), null, "wasm renders the 404");
    const home = await read(w, "/");
    assert.equal(home.res.headers.get(CACHE), "miss");
    assert.equal(home.res.headers.get(MARK), null);
    assert.doesNotMatch(home.body, /r2-pub/);
    assert.ok(r2Row(w, oldPage.key) && r2Row(w, oldHome.key), "the old objects remain; their key is retired");

    // Slug change.
    let q = await newPost(w, "r2-old-slug");
    q = await patch(w, q, { status: "published" });
    assert.equal((await read(w, "/r2-old-slug")).res.status, 200);
    await stored(w, await keyOf(w, "/r2-old-slug"));
    q = await patch(w, q, { slug: "r2-new-slug" });
    emptyCache(w);
    const old = await read(w, "/r2-old-slug");
    assert.notEqual(old.res.status, 200, "the old slug is not the stored page");
    assert.equal(old.res.headers.get(MARK), null);
    const moved = await read(w, "/r2-new-slug");
    assert.equal(moved.res.status, 200);
    assert.equal(moved.res.headers.get(CACHE), "miss");

    // Settings change.
    await read(w, "/");
    await stored(w, await keyOf(w, "/"));
    const r = await api(w, "PUT", "/api/v1/settings/site_title", { value: "R2 Retitled" });
    assert.equal(r.status, 204, r.text);
    emptyCache(w);
    const retitled = await read(w, "/");
    assert.equal(retitled.res.headers.get(CACHE), "miss");
    assert.equal(retitled.res.headers.get(MARK), null);
    assert.match(retitled.body, /R2 Retitled/);
  });

  // wrangler dev gives every start a new random version id, as a deploy does. Renaming one
  // R2 object to the new id stands in for "same version, a data centre that never saw it".
  test("a new version id serves neither the old id's Cache API entries nor its R2 objects", async () => {
    await read(w, "/feed.xml");
    await read(w, PATH);
    const feed = await hit(w, "/feed.xml");
    await hit(w, PATH);
    const oldId = await versionId(w);
    const feedKey = await keyOf(w, "/feed.xml");
    const pageKey = await keyOf(w, PATH);
    await stored(w, feedKey);
    await stored(w, pageKey);
    await w.stop({ keepState: true });
    // Same port: the Cache API key carries the origin.
    w = await startWorker({ state: w.state, port: Number(new URL(w.url).port) });
    const newId = await versionId(w);
    assert.notEqual(newId, oldId, "a restart is a new version");
    assert.ok(cacheKeys(w).some((k) => k.includes(`/__page/${oldId}/`)), "the old id's Cache API entries are still there");

    renameR2(w, feedKey, feedKey.replace(oldId, newId));
    const r = await read(w, "/feed.xml");
    assert.equal(r.res.headers.get(CACHE), "store", "the R2 copy, not the old id's Cache API entry");
    assert.equal(r.res.headers.get(MARK), "js");
    assert.equal(r.res.headers.get(WASM), null, "an R2 hit on a new isolate loaded wasm");
    assert.equal(r.body, feed.body);
    assert.deepEqual(headersOf(r.res), headersOf(feed.res));

    const page = await read(w, PATH);
    assert.equal(page.res.headers.get(CACHE), "miss", "neither the old Cache API entry nor the old R2 object is served");
    assert.equal(page.res.headers.get(MARK), null);
    assert.equal(page.res.headers.get(WASM), "loaded");
    await stored(w, pageKey.replace(oldId, newId));
  });
});

describe("R2 page store limits and failures (test-build knobs)", () => {
  const workers = {};
  before(async () => {
    const setups = {
      small: { vars: { PLUMBLOG_TEST_PAGE_LIMIT: "1000" } },
      broken: { vars: { PLUMBLOG_TEST_PAGE_BUCKET: "NO_SUCH_BUCKET" } },
    };
    await Promise.all(
      Object.entries(setups).map(async ([k, o]) => {
        workers[k] = await startWorker(o);
        await seedSample(workers[k]);
      }),
    );
  });
  after(async () => {
    await Promise.all(Object.values(workers).map((w) => w.stop()));
  });

  test("a page over the limit goes to the Cache API only", async () => {
    const w = workers.small;
    const big = await read(w, "/");
    assert.equal(big.res.status, 200);
    assert.ok(Buffer.byteLength(big.body) > 1000);
    // A Cache API hit means that miss's R2 step is over (pages::cache::put_later).
    await hit(w, "/");
    assert.deepEqual(pageKeys(w), []);
  });

  test("an R2 put failure still serves the page and logs pages.store_failed", async () => {
    const w = workers.broken;
    const r = await read(w, PATH_FAIL);
    assert.equal(r.res.status, 200);
    assert.equal(r.res.headers.get(CACHE), "miss");
    let rows = [];
    for (let i = 0; i < 50 && rows.length === 0; i++) {
      rows = w.query("SELECT kind, error_code, detail FROM system_log WHERE error_code = 'pages.store_failed'");
      if (rows.length === 0) await sleep(100);
    }
    assert.equal(rows.length, 1);
    assert.equal(rows[0].kind, "error");
    assert.match(rows[0].detail, new RegExp(`^${PATH_FAIL}\\n`));
    // wrangler's log arrives a moment after the response.
    for (let i = 0; i < 50 && !/pages\.store_failed path=\/reading-on-screens/.test(w.log()); i++) await sleep(100);
    assert.match(w.log(), /pages\.store_failed path=\/reading-on-screens/);
    const h = await hit(w, PATH_FAIL);
    assert.equal(h.body, r.body, "the Cache API still keeps it");
    assert.deepEqual(pageKeys(w), []);
  });
});
