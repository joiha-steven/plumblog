// Theme options through the real Worker and local D1: a stored value reaches the rendered
// page (the content version bumps, so the cached copy is replaced), and a value that does not
// fit its declaration falls back to the default without breaking the page.
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

const get = (path) => fetch(`${worker.url}${path}`);
const put = async (key, json) => {
  const res = await fetch(`${worker.url}/__option?owner=theme&id=minimal&key=${key}`, { method: "POST", body: json });
  assert.equal(res.status, 200, await res.text());
};
const excerpts = (html) => (html.match(/<p class="excerpt">/g) ?? []).length;

test("an option change reaches the cached page", async () => {
  const first = await get("/");
  const before = await first.text();
  assert.equal((await get("/")).headers.get("x-plumblog-cache"), "hit", "cached before the change");
  assert.match(before, /--c-accent:light-dark\(#8a2d6f,#e2a3d2\)/);

  await put("accent", '{"light":"#123456","dark":"#abcdef"}');
  await put("excerpt_words", "0");

  const res = await get("/");
  assert.equal(res.headers.get("x-plumblog-cache"), "miss", "new content version, new key");
  assert.notEqual(res.headers.get("etag"), first.headers.get("etag"));
  const html = await res.text();
  assert.match(html, /--c-accent:light-dark\(#123456,#abcdef\)/);
  assert.ok(excerpts(html) < excerpts(before), `${excerpts(before)} -> ${excerpts(html)}`);
});

test("a stored value that does not fit falls back to the default", async () => {
  await put("accent", '{"light":"red}body{display:none","dark":"#000"}');
  await put("excerpt_words", '"lots"');
  const res = await get("/");
  assert.equal(res.status, 200);
  const html = await res.text();
  assert.match(html, /--c-accent:light-dark\(#8a2d6f,#e2a3d2\)/);
  assert.ok(!html.includes("red}body{display:none"), "the injected value never reaches the CSS");
});
