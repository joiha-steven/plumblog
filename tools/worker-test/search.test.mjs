// Reader search through the real Worker and local D1: the FTS5 index, accent folding and
// hostile queries. The sample posts are saved into D1 first (POST /__seed, test builds only).
// Vietnamese is escaped because repo files must be ASCII.
import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { startWorker } from "./harness.mjs";

let worker;
before(async () => {
  worker = await startWorker();
  const res = await fetch(`${worker.url}/__seed`, { method: "POST" });
  assert.equal(res.status, 200, await res.text());
});
after(async () => {
  await worker?.stop();
});

const search = async (q) => {
  const res = await fetch(`${worker.url}/search?q=${encodeURIComponent(q)}`);
  return { res, html: await res.text() };
};
/** Post links in the result list, in order. */
const hits = (html) => {
  const list = html.split('<ol class="posts">')[1]?.split("</ol>")[0] ?? "";
  return [...list.matchAll(/href="([^"]+)"/g)].map((m) => m[1]).filter((h) => !h.startsWith("/tag/"));
};

test("search is never cached and never indexed", async () => {
  const { res, html } = await search("duong");
  assert.equal(res.status, 200);
  assert.equal(res.headers.get("cache-control"), "no-store");
  assert.equal(res.headers.get("x-plumblog-cache"), "bypass");
  assert.equal(res.headers.get("etag"), null);
  assert.match(html, /<meta name="robots" content="noindex">/);
});

test("Vietnamese with and without marks finds the same post", async () => {
  for (const q of [
    "d\u01B0\u1EDDng", // duong with horn and grave
    "\u0111\u01B0\u1EDDng", // with d-stroke: the app folds it before FTS5
    "duong",
    "DUONG",
    "du\u031Bo\u031B\u0300ng", // typed decomposed (macOS)
    "Ng\u01B0\u1EDDi Vi\u1EC7t",
    "nguoi viet",
  ]) {
    const { html } = await search(q);
    assert.deepEqual(hits(html), ["/notes-from-da-lat"], q);
  }
  for (const q of ["\u0110\u00E0 L\u1EA1t", "da lat"]) {
    assert.ok(hits((await search(q)).html).includes("/notes-from-da-lat"), q);
  }
});

test("empty and no-result states", async () => {
  const empty = await (await fetch(`${worker.url}/search`)).text();
  assert.match(empty, /Search titles and text of every post\./);
  assert.deepEqual(hits(empty), []);
  const none = (await search("duong hanoi")).html;
  assert.match(none, /Nothing found for/);
  assert.deepEqual(hits(none), []);
});

test("FTS5 syntax in a query is plain text, never an error", async () => {
  for (const q of ['"', '""', "*", "NEAR(", "NEAR(a b)", "OR", "a OR b", "-", "-duong", '"unbalanced', "title:duong", "^duong", "(", ")", "duong*", "{x}", "'", ";--", "AND NOT", "\u0000", "a".repeat(5000)]) {
    const { res } = await search(q);
    assert.equal(res.status, 200, JSON.stringify(q.slice(0, 20)));
  }
  assert.deepEqual(hits((await search("duong OR tools")).html), [], "OR is a word");
  assert.deepEqual(hits((await search("-duong")).html), ["/notes-from-da-lat"], "- is not NOT");
});

test("results link to pages that exist, newest first", async () => {
  const links = hits((await search("tools")).html);
  assert.ok(links.length >= 2, links.join());
  assert.equal(links[0], "/why-small-tools");
  for (const href of links) assert.equal((await fetch(`${worker.url}${href}`)).status, 200, href);
});

// Search is per language, like the homes: the sample site lists en and vi, and its one vi
// post ("Bai viet dau tien": "cong cu" = tools) is only found under /vi/search.
const searchAt = async (path, q) => {
  const res = await fetch(`${worker.url}${path}?q=${encodeURIComponent(q)}`, { redirect: "manual" });
  return { res, html: await res.text() };
};

test("a Vietnamese phrase is found at /vi/search and not at /search", async () => {
  for (const q of ["c\u00f4ng c\u1ee5", "cong cu"]) {
    const vi = await searchAt("/vi/search", q);
    assert.equal(vi.res.status, 200, q);
    assert.deepEqual(hits(vi.html), ["/vi/bai-viet-dau-tien"], q);
    assert.deepEqual(hits((await search(q)).html), [], `${q} at /search`);
  }
  // And the other way round: an English word does not appear in the Vietnamese results.
  assert.deepEqual(hits((await searchAt("/vi/search", "tools")).html), []);
  assert.ok(hits((await search("tools")).html).includes("/why-small-tools"));
});

test("the Vietnamese search page keeps the search limits and follows the page language", async () => {
  const { res, html } = await searchAt("/vi/search", "cong cu");
  assert.equal(res.headers.get("cache-control"), "no-store");
  assert.equal(res.headers.get("x-plumblog-cache"), "bypass");
  assert.equal(res.headers.get("etag"), null);
  assert.match(html, /<html lang="vi"/);
  assert.match(html, /<meta name="robots" content="noindex">/);
  assert.match(html, /action="\/vi\/search"/);
  assert.doesNotMatch(html, /action="\/search"/);
  // Any Vietnamese page submits to /vi/search, any English page to /search.
  assert.match(await (await fetch(`${worker.url}/vi/bai-viet-dau-tien`)).text(), /action="\/vi\/search"/);
  assert.match(await (await fetch(`${worker.url}/vi/`)).text(), /action="\/vi\/search"/);
  assert.match(await (await fetch(`${worker.url}/first-post`)).text(), /action="\/search"/);
  // A query without words is the form again.
  assert.deepEqual(hits((await searchAt("/vi/search", "**")).html), []);
});

test("one URL per language's search: the default's is /search, unlisted languages are 404", async () => {
  const en = await searchAt("/en/search", "da lat");
  assert.equal(en.res.status, 301);
  assert.equal(en.res.headers.get("location"), `${worker.url}/search?q=da+lat`);
  const bare = await fetch(`${worker.url}/en/search`, { redirect: "manual" });
  assert.equal(bare.status, 301);
  assert.equal(bare.headers.get("location"), `${worker.url}/search`);
  const slash = await fetch(`${worker.url}/vi/search/?q=x`, { redirect: "manual" });
  assert.equal(slash.status, 301);
  assert.equal(slash.headers.get("location"), `${worker.url}/vi/search`);
  assert.equal((await searchAt("/xx/search", "tools")).res.status, 404);
  assert.equal((await searchAt("/api/search", "tools")).res.status, 404);
});
