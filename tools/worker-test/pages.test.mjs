// Reader pages through the real Worker: status, key content, cache headers, assets.
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

const get = (path, init = {}) => fetch(`${worker.url}${path}`, { redirect: "manual", ...init });

// The cache write runs after the response (waitUntil), so a hit can take a moment to appear.
async function eventuallyHit(path) {
  for (let i = 0; i < 20; i++) {
    const res = await get(path);
    await res.arrayBuffer();
    if (res.headers.get("x-plumblog-cache") === "hit") return res;
    await new Promise((r) => setTimeout(r, 100));
  }
  assert.fail(`${path} never came from cache`);
}

test("home renders, then comes from the versioned cache", async () => {
  const res = await get("/");
  assert.equal(res.status, 200);
  assert.match(res.headers.get("content-type"), /^text\/html/);
  assert.equal(res.headers.get("x-plumblog-cache"), "miss");
  assert.match(res.headers.get("server-timing"), /^render;dur=\d+$/);
  assert.match(res.headers.get("cache-control"), /max-age=0/);
  const html = await res.text();
  assert.match(html, /<title>Plum Notes<\/title>/);
  assert.ok(html.indexOf("Why I keep my tools small") < html.indexOf("Reading on screens"), "pinned first");
  assert.match(html, /href="\/page\/2" rel="next"/);

  const hit = await eventuallyHit("/");
  assert.equal(hit.status, 200);
  assert.equal(hit.headers.get("server-timing"), null, "a hit reports no render");
  assert.equal(hit.headers.get("etag"), res.headers.get("etag"));
});

test("a matching ETag gets 304 with no body", async () => {
  const etag = (await get("/reading-on-screens")).headers.get("etag");
  assert.match(etag, /^"\d+-[\d.]+"$/);
  const res = await get("/reading-on-screens", { headers: { "if-none-match": etag } });
  assert.equal(res.status, 304);
  assert.equal(await res.text(), "");
});

test("page 2, and page 1 redirects to the home page", async () => {
  const res = await get("/page/2");
  assert.equal(res.status, 200);
  assert.match(await res.text(), /First post/);
  const one = await get("/page/1");
  assert.equal(one.status, 301);
  assert.equal(new URL(one.headers.get("location")).pathname, "/");
  const slash = await get("/reading-on-screens/");
  assert.equal(slash.status, 301);
  assert.equal(new URL(slash.headers.get("location")).pathname, "/reading-on-screens");
});

test("hostile paths never redirect off site or to themselves", async () => {
  for (const path of ["//evil.com/", "/\\evil.com/", "//", "///", "/%2F%2Fevil.com/", "/post//"]) {
    const res = await get(path);
    assert.equal(res.status, 404, path);
    assert.equal(res.headers.get("location"), null, path);
    await res.arrayBuffer();
  }
});

test("post page", async () => {
  const res = await get("/reading-on-screens");
  assert.equal(res.status, 200);
  const html = await res.text();
  assert.match(html, /<title>Reading on screens, done properly - Plum Notes<\/title>/);
  assert.match(html, /<section class="footnotes"/);
  assert.match(html, /href="\/tag\/typography" rel="tag"/);
  assert.match(html, /<link rel="canonical" href="http:\/\/127\.0\.0\.1:\d+\/reading-on-screens">/);
});

test("tag and yearly archive pages", async () => {
  const tag = await get("/tag/tools");
  assert.equal(tag.status, 200);
  const t = await tag.text();
  assert.match(t, /<h1 class="list-title">#tools<\/h1>/);
  assert.doesNotMatch(t, /Slow mornings/);
  const year = await get("/archive/2025");
  assert.equal(year.status, 200);
  assert.match(await year.text(), /Posts from 2025/);
});

test("404 is friendly, uncached and never stored", async () => {
  for (const path of ["/no-such-post", "/tag/nope", "/archive/1999", "/a/b"]) {
    const res = await get(path);
    assert.equal(res.status, 404, path);
    assert.equal(res.headers.get("cache-control"), "no-store", path);
    const html = await res.text();
    assert.match(html, /Page not found/, path);
    assert.match(html, /Why I keep my tools small/, `${path} suggests posts`);
  }
  await new Promise((r) => setTimeout(r, 300));
  assert.notEqual((await get("/no-such-post")).headers.get("x-plumblog-cache"), "hit");
});

test("theme fonts and script are served as static assets", async () => {
  const html = await (await get("/")).text();
  const font = html.match(/<link rel="preload" href="([^"]+\.woff2)"/)[1];
  const js = html.match(/<script src="([^"]+\.js)" defer>/)[1];
  for (const path of [font, js]) {
    const res = await get(path);
    assert.equal(res.status, 200, path);
    await res.arrayBuffer();
  }
});

test("the code highlighter loads only on posts with code", async () => {
  const code = await (await get("/code-in-many-languages")).text();
  const src = code.match(/<script src="([^"]*highlight\.js)" defer>/)?.[1];
  assert.ok(src, "post with code links the highlighter");
  const res = await get(src);
  assert.equal(res.status, 200);
  assert.match(await res.text(), /tok-/);
  assert.doesNotMatch(await (await get("/slow-mornings")).text(), /highlight\.js/);
});

test("writes are refused", async () => {
  assert.equal((await get("/", { method: "POST" })).status, 405);
});
