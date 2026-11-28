// The JS entry (crates/worker/js/entry.mjs) answers /healthz and page-cache hits before the
// wasm loads. These tests pin it to the Rust side: the same cache key, the same bytes, the
// same ETag, and wasm still serving every miss and every failure. `x-plumblog-entry: js`
// says who answered.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { cpSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, relative, resolve } from "node:path";
import { after, before, describe, test } from "node:test";
import { fileURLToPath } from "node:url";

import { ASSETS_DIR, seedSample, startWorker } from "./harness.mjs";

const MARK = "x-plumblog-entry";
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const get = (w, path, init = {}) => fetch(`${w.url}${path}`, { redirect: "manual", ...init });

/** The response with its body read, so the cache write (waitUntil) has a chance to land. */
async function read(w, path, init) {
  const res = await get(w, path, init);
  return { res, body: await res.text() };
}

/** Polls until the page is a cache hit; the write runs after the response. */
async function hit(w, path) {
  for (let i = 0; i < 30; i++) {
    const r = await read(w, path);
    if (r.res.headers.get("x-plumblog-cache") === "hit") return r;
    await sleep(100);
  }
  assert.fail(`${path} never came from cache`);
}

/** The first request is a wasm miss; a later one is a hit from the JS entry with the same bytes. */
async function assertMissThenJsHit(w, path) {
  const miss = await read(w, path);
  assert.equal(miss.res.headers.get(MARK), null, `${path}: first request is wasm`);
  assert.equal(miss.res.headers.get("x-plumblog-cache"), "miss", path);
  const h = await hit(w, path);
  assert.equal(h.res.headers.get(MARK), "js", `${path}: hit comes from the entry`);
  assert.equal(h.res.status, miss.res.status, path);
  assert.equal(h.body, miss.body, `${path}: body`);
  for (const name of ["content-type", "cache-control", "etag"]) {
    assert.equal(h.res.headers.get(name), miss.res.headers.get(name), `${path}: ${name}`);
  }
  assert.equal(h.res.headers.get("server-timing"), null, `${path}: a hit reports no render`);
  return h;
}

describe("embedded theme", () => {
  let w;
  before(async () => {
    w = await startWorker();
    await seedSample(w);
  });
  after(async () => {
    await w?.stop();
  });

  test("/healthz is answered by the entry, with the wasm Worker's bytes", async () => {
    const res = await get(w, "/healthz");
    assert.equal(res.status, 200);
    assert.equal(await res.text(), "ok");
    assert.equal(res.headers.get("content-type"), "text/plain; charset=utf-8");
    assert.match(res.headers.get("x-plumblog-version") ?? "", /^[0-9a-f-]{8,}$/i);
    assert.equal(res.headers.get(MARK), "js", "healthz never loads wasm");
  });

  const PATHS = [
    "/",
    "/page/2",
    "/reading-on-screens",
    "/vi/bai-viet-dau-tien",
    "/tag/tools",
    "/feed.xml",
    "/sitemap.xml",
    "/archive/2025",
  ];
  for (const path of PATHS) {
    test(`${path}: wasm miss, then a byte-identical hit from the entry`, async () => {
      await assertMissThenJsHit(w, path);
    });
  }

  test("the query string is not part of the key", async () => {
    await hit(w, "/reading-on-screens");
    const r = await read(w, "/reading-on-screens?utm_source=x");
    assert.equal(r.res.headers.get(MARK), "js");
    assert.equal(r.res.headers.get("x-plumblog-cache"), "hit");
  });

  test("If-None-Match with the cached ETag gets a 304 from the entry, ETag only", async () => {
    const path = "/reading-on-screens";
    const h = await hit(w, path);
    const etag = h.res.headers.get("etag");
    assert.match(etag, /^"\d+-[\d.]+"$/);
    const res = await get(w, path, { headers: { "if-none-match": etag } });
    assert.equal(res.status, 304);
    assert.equal(res.headers.get(MARK), "js");
    assert.equal(res.headers.get("etag"), etag);
    assert.equal(await res.text(), "");
    // Only the ETag, as Rust sends: none of the page's own headers.
    for (const name of ["content-type", "cache-control", "x-plumblog-cache", "server-timing"]) {
      assert.equal(res.headers.get(name), null, name);
    }
    // A different ETag is a full response.
    const other = await get(w, path, { headers: { "if-none-match": `"0-0"` } });
    assert.equal(other.status, 200);
    await other.arrayBuffer();
  });

  test("HEAD on a cached page is a hit from the entry with no body", async () => {
    const path = "/tag/tools";
    await hit(w, path);
    const res = await get(w, path, { method: "HEAD" });
    assert.equal(res.status, 200);
    assert.equal(res.headers.get(MARK), "js");
    assert.equal(res.headers.get("x-plumblog-cache"), "hit");
    assert.equal(await res.text(), "");
  });

  test("a content write makes the old ETag stale and the next request a wasm miss", async () => {
    const path = "/reading-on-screens";
    const before = await hit(w, path);
    const old = before.res.headers.get("etag");
    w.exec("UPDATE site SET content_version = content_version + 1");
    const res = await get(w, path, { headers: { "if-none-match": old } });
    assert.equal(res.status, 200, "the old ETag no longer matches");
    assert.equal(res.headers.get(MARK), null);
    assert.equal(res.headers.get("x-plumblog-cache"), "miss");
    assert.notEqual(res.headers.get("etag"), old);
    await res.arrayBuffer();
    const again = await hit(w, path);
    assert.equal(again.res.headers.get(MARK), "js");
    assert.notEqual(again.res.headers.get("etag"), old);
  });

  test("404s are never cached, so never marked", async () => {
    for (const path of ["/no-such-post", "/adminx", "/caf%C3%A9", "/tag/nope"]) {
      for (let i = 0; i < 2; i++) {
        const r = await read(w, path);
        assert.equal(r.res.status, 404, path);
        assert.equal(r.res.headers.get(MARK), null, path);
        await sleep(50);
      }
    }
  });

  test("search, admin, API, MCP and writes never come from the entry", async () => {
    await hit(w, "/");
    const reqs = [
      ["/search?q=plum"],
      ["/search?q=plum"],
      ["/admin/"],
      ["/admin"],
      ["/api/v1/site"],
      ["/mcp"],
      ["/", { method: "POST" }],
    ];
    for (const [path, init] of reqs) {
      const res = await get(w, path, init);
      assert.equal(res.headers.get(MARK), null, `${init?.method ?? "GET"} ${path}`);
      await res.arrayBuffer();
    }
  });

  test("a D1 failure in the fast path hands over to wasm, which fails exactly as before", async () => {
    const path = "/reading-on-screens";
    await hit(w, path);
    w.exec("ALTER TABLE site RENAME TO site_gone");
    try {
      const res = await get(w, path);
      await res.text();
      assert.equal(res.status, 500, "the wasm error page, as without the entry");
      assert.equal(res.headers.get(MARK), null);
    } finally {
      w.exec("ALTER TABLE site_gone RENAME TO site");
    }
    // wrangler's log arrives a moment after the response, longer when the machine is busy.
    for (let i = 0; i < 50 && !/entry: cache fast path failed, wasm takes over/.test(w.log()); i++) await sleep(100);
    assert.match(w.log(), /entry: cache fast path failed, wasm takes over/);
    assert.equal((await hit(w, path)).res.headers.get(MARK), "js", "recovers once D1 is back");
  });
});

// A pushed theme, as theme.test.mjs builds one: only the cache tag matters here.
describe("pushed theme", () => {
  const sha256hex = (s) => createHash("sha256").update(s).digest("hex");
  const dirs = [];
  const root = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
  const minimal = join(root, "themes/minimal");
  const walk = (d) => readdirSync(d, { withFileTypes: true }).flatMap((e) => (e.isDirectory() ? walk(join(d, e.name)) : [join(d, e.name)]));
  // The minimal theme renamed, built as theme.test.mjs builds a pushed theme.json.
  function pushedThemeJson(name) {
    const files = {};
    for (const p of walk(minimal).sort()) {
      const rel = relative(minimal, p).replaceAll("\\", "/");
      if (/\.(html|css)$/.test(rel) && (!rel.startsWith("assets/") || rel === "assets/fonts/faces.css")) files[rel] = readFileSync(p, "utf8");
    }
    Object.assign(
      files,
      JSON.parse(
        execFileSync("cargo", ["run", "-q", "-p", "plumblog-render", "--example", "theme_json", "--", minimal], {
          cwd: root,
          encoding: "utf8",
          maxBuffer: 16 * 1024 * 1024,
        }),
      ),
    );
    files["theme.toml.json"] = files["theme.toml.json"].replace('"name":"minimal"', `"name":"${name}"`);
    const hash = sha256hex(JSON.stringify(files));
    return { json: JSON.stringify({ api: 2, name, version: "0.2.0", hash, files }), hash };
  }

  function assetsWith(name, json) {
    const dir = mkdtempSync(join(tmpdir(), "plumblog-entry-assets-"));
    dirs.push(dir);
    cpSync(ASSETS_DIR, dir, { recursive: true });
    const at = join(dir, "_theme", name);
    mkdirSync(at, { recursive: true });
    cpSync(join(minimal, "assets"), join(at, "assets"), { recursive: true });
    writeFileSync(join(at, "theme.json"), json);
    return dir;
  }

  const workers = {};
  let hash;
  before(async () => {
    const t = pushedThemeJson("paper");
    hash = t.hash;
    const good = assetsWith("paper", t.json);
    const broken = assetsWith("paper", "{not json");
    const setups = {
      tagged: { vars: { PLUMBLOG_THEME: "paper", PLUMBLOG_THEME_TAG: hash.slice(0, 16) }, assets: good },
      untagged: { vars: { PLUMBLOG_THEME: "paper" }, assets: good },
      badtag: { vars: { PLUMBLOG_THEME: "paper", PLUMBLOG_THEME_TAG: hash.slice(0, 16).toUpperCase() }, assets: good },
      broken: { vars: { PLUMBLOG_THEME: "paper", PLUMBLOG_THEME_TAG: hash.slice(0, 16) }, assets: broken },
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
    for (const d of dirs) rmSync(d, { recursive: true, force: true });
  });

  test("with the tag binding, hits come from the entry under the tagged key", async () => {
    const h = await assertMissThenJsHit(workers.tagged, "/reading-on-screens");
    assert.match(h.res.headers.get("etag"), /^"\d+-t[0-9a-f]{16}-[\d.]+"$/);
    assert.equal(h.res.headers.get("x-plumblog-theme"), hash.slice(0, 16));
    const res = await get(workers.tagged, "/reading-on-screens", { headers: { "if-none-match": h.res.headers.get("etag") } });
    assert.equal(res.status, 304);
    assert.equal(res.headers.get(MARK), "js");
  });

  for (const k of ["untagged", "badtag"]) {
    test(`${k}: hits still happen, from wasm`, async () => {
      const w = workers[k];
      await read(w, "/");
      const h = await hit(w, "/");
      assert.equal(h.res.status, 200);
      assert.equal(h.res.headers.get(MARK), null);
      assert.equal((await read(w, "/")).res.headers.get(MARK), null);
    });
  }

  test("a broken pushed theme with a tag binding is never served stale: wasm serves, under the default key", async () => {
    const w = workers.broken;
    await read(w, "/");
    const h = await hit(w, "/");
    assert.equal(h.res.headers.get(MARK), null, "the entry looks under -t<tag>, wasm stored under the default key");
    assert.doesNotMatch(h.res.headers.get("etag"), /-t[0-9a-f]{16}-/);
    w.exec("UPDATE site SET content_version = content_version + 1");
    const next = await read(w, "/");
    assert.equal(next.res.headers.get("x-plumblog-cache"), "miss");
    assert.equal(next.res.headers.get(MARK), null);
  });
});
