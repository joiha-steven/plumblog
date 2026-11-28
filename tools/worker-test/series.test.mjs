// Tag and series pages are per language (issue #132), through the real Worker and local D1:
// `/tag/<slug>` and `/series/<slug>` list the default language, `/<lang>/...` another listed
// one; empty language pages are 404s and not in the sitemap; slugs already stored as `series`
// or `search` stay reachable.
import assert from "node:assert/strict";
import { createHash, randomBytes } from "node:crypto";
import { after, before, test } from "node:test";
import { startWorker } from "./harness.mjs";

const T = Date.now();
const DAY = 86_400_000;
const TOKEN = `plb_${randomBytes(32).toString("base64url")}`;
const hash = createHash("sha256").update(TOKEN).digest("hex");

let worker;
before(async () => {
  worker = await startWorker({
    seedSql: `INSERT INTO users (id, login, slug, display_name, role, password_hash, disabled_at, created_at, updated_at) VALUES (1, 'olga', 'olga', 'Olga', 'owner', NULL, NULL, ${T}, ${T});
      INSERT INTO api_tokens (id, user_id, name, token_hash, scope, created_at, expires_at, revoked_at) VALUES (1, 1, 'boss', '${hash}', 'owner', ${T}, NULL, NULL);
      INSERT INTO settings (key, value_json, updated_at) VALUES ('origin', '"https://blog.test"', ${T}), ('site_languages', '["en","vi"]', ${T});`,
  });
  await seed();
});
after(async () => {
  await worker?.stop();
});

const get = (path) => fetch(`${worker.url}${path}`, { redirect: "manual" });
const text = async (path) => {
  const res = await get(path);
  return { status: res.status, body: await res.text(), location: res.headers.get("location"), res };
};
async function api(method, path, body) {
  const res = await fetch(`${worker.url}/api/v1${path}`, {
    method,
    headers: { authorization: `Bearer ${TOKEN}`, origin: worker.url, "content-type": "application/json" },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const raw = await res.text();
  return { status: res.status, raw, json: raw ? JSON.parse(raw) : null };
}
const create = async (post) => {
  const r = await api("POST", "/posts", { markdown: "Some words here.", status: "published", published_at: T - DAY, ...post });
  assert.equal(r.status, 201, r.raw);
  return r.json;
};
/** Post links in a listing, in order. */
const hits = (html) => {
  const list = html.split('<ol class="posts">')[1]?.split("</ol>")[0] ?? "";
  return [...list.matchAll(/<a class="entry[^"]*" href="([^"]+)"/g)].map((m) => m[1]);
};

async function seed() {
  const tag = (await api("POST", "/tags", { slug: "plums", name: "Plums" })).json;
  const vi = (await api("POST", "/tags", { slug: "chi-vi", name: "Chi vi" })).json;
  const series = (await api("POST", "/series", { slug: "orchard", title: "The Orchard", description: "Growing plums." })).json;
  const solo = (await api("POST", "/series", { slug: "solo", title: "Solo", description: "" })).json;
  const base = { tags: [tag.id], series_id: series.id };
  await create({ ...base, lang: "en", slug: "e-two", title: "E two", series_position: 2, published_at: T - 3 * DAY });
  await create({ ...base, lang: "en", slug: "e-one", title: "E one", series_position: 1, published_at: T - 2 * DAY });
  await create({ ...base, lang: "vi", slug: "v-three", title: "V three", series_position: 3, published_at: T - 5 * DAY });
  await create({ ...base, lang: "vi", slug: "v-one", title: "V one", series_position: 1, published_at: T - 4 * DAY });
  await create({ lang: "vi", slug: "v-only", title: "V only", tags: [vi.id], series_id: solo.id, series_position: 1 });
  await create({ ...base, lang: "en", slug: "e-draft", title: "E draft", series_position: 0, status: "draft", published_at: null });
}

test("a tag page lists one language, with hreflang between the versions", async () => {
  const en = await text("/tag/plums");
  assert.equal(en.status, 200, en.body);
  assert.deepEqual(hits(en.body), ["/e-one", "/e-two"], "newest first, default language only");
  const vi = await text("/vi/tag/plums");
  assert.equal(vi.status, 200, vi.body);
  assert.deepEqual(hits(vi.body), ["/vi/v-one", "/vi/v-three"]);
  assert.match(vi.body, /<html lang="vi"/);
  assert.match(vi.body, /<link rel="canonical" href="https:\/\/blog\.test\/vi\/tag\/plums">/);
  for (const html of [en.body, vi.body]) {
    assert.match(html, /hreflang="en" href="https:\/\/blog\.test\/tag\/plums"/);
    assert.match(html, /hreflang="vi" href="https:\/\/blog\.test\/vi\/tag\/plums"/);
    assert.match(html, /hreflang="x-default" href="https:\/\/blog\.test\/tag\/plums"/);
  }
  assert.equal((await text("/tag/chi-vi")).status, 404, "no default-language post: not an empty page");
  assert.equal((await text("/vi/tag/chi-vi")).status, 200);
  assert.equal((await text("/de/tag/plums")).status, 404, "not a listed language");
});

test("a series page is in reading order, per language", async () => {
  const en = await text("/series/orchard");
  assert.equal(en.status, 200, en.body);
  assert.deepEqual(hits(en.body), ["/e-one", "/e-two"], "by position, not by date");
  assert.match(en.body, /<h1 class="list-title">The Orchard<\/h1>/);
  assert.match(en.body, /Growing plums\./);
  const vi = await text("/vi/series/orchard");
  assert.equal(vi.status, 200, vi.body);
  assert.deepEqual(hits(vi.body), ["/vi/v-one", "/vi/v-three"]);
  assert.match(vi.body, /hreflang="en" href="https:\/\/blog\.test\/series\/orchard"/);
  assert.equal((await text("/series/solo")).status, 404, "a series with no posts in the language");
  assert.equal((await text("/vi/series/solo")).status, 200);
  assert.equal((await text("/series/nope")).status, 404);
});

test("one URL per page", async () => {
  const moved = async (path, to) => {
    const r = await text(path);
    assert.equal(r.status, 301, path);
    assert.equal(new URL(r.location).pathname, to, path);
  };
  await moved("/en/tag/plums", "/tag/plums");
  await moved("/en/series/orchard", "/series/orchard");
  await moved("/en/series/orchard/page/1", "/series/orchard");
  await moved("/vi/series/orchard/page/1", "/vi/series/orchard");
  await moved("/vi/tag/plums/", "/vi/tag/plums");
  assert.equal((await text("/vi/series/orchard/page/2")).status, 404);
});

test("a post links its series and tags in its own language", async () => {
  const en = (await text("/e-one")).body;
  assert.match(en, /<p class="series">Series: <a href="\/series\/orchard">The Orchard<\/a> \(1\)<\/p>/);
  assert.match(en, /href="\/tag\/plums" rel="tag"/);
  const vi = (await text("/vi/v-three")).body;
  assert.match(vi, /<a href="\/vi\/series\/orchard">The Orchard<\/a> \(3\)<\/p>/);
  assert.match(vi, /href="\/vi\/tag\/plums" rel="tag"/);
});

test("the sitemap lists each non-empty page per language", async () => {
  const map = (await text("/sitemap.xml")).body;
  for (const loc of ["/tag/plums", "/vi/tag/plums", "/vi/tag/chi-vi", "/series/orchard", "/vi/series/orchard", "/vi/series/solo"]) {
    assert.ok(map.includes(`<loc>https://blog.test${loc}</loc>`), loc);
  }
  for (const loc of ["/tag/chi-vi", "/series/solo"]) {
    assert.ok(!map.includes(`<loc>https://blog.test${loc}</loc>`), loc);
  }
});

test("the pages are cached like a tag page always was", async () => {
  for (const path of ["/series/orchard", "/vi/tag/plums"]) {
    const first = await text(path);
    assert.equal(first.status, 200);
    assert.match(first.res.headers.get("cache-control") ?? "", /s-maxage=31536000/, path);
    const etag = first.res.headers.get("etag");
    assert.ok(etag, path);
    const again = await fetch(`${worker.url}${path}`, { headers: { "if-none-match": etag } });
    assert.equal(again.status, 304, path);
  }
});

test("`series` is refused as a default-language slug, free in another language", async () => {
  const en = await api("POST", "/posts", { lang: "en", slug: "series", title: "T", markdown: "x", status: "draft" });
  assert.deepEqual(en.json, { error: "input.invalid", field: "slug" });
  const vi = await api("POST", "/posts", { lang: "vi", slug: "series", title: "T", markdown: "x", status: "draft" });
  assert.equal(vi.status, 201, vi.raw);
});

test("posts stored with a slug the router took stay reachable", async () => {
  // The API refuses these slugs now; a post stored before is written to the database directly.
  const now = T;
  worker.exec(
    `INSERT INTO posts (id, kind, translation_group, lang, slug, status, published_at, author_id, title, markdown, html, created_at, updated_at) VALUES
     (901, 'post', 901, 'en', 'series', 'published', ${now - DAY}, 1, 'Old series post', 'x', '<p>old</p>', ${now}, ${now}),
     (902, 'post', 902, 'vi', 'search', 'published', ${now - DAY}, 1, 'Old search post', 'x', '<p>cu</p>', ${now}, ${now})`,
  );
  const series = await text("/series");
  assert.equal(series.status, 200, series.body);
  assert.match(series.body, /Old series post/);
  assert.equal((await text("/series/orchard")).status, 200, "and /series/<slug> is still the listing");

  const old = await text("/vi/search");
  assert.equal(old.status, 200, old.body);
  assert.match(old.body, /Old search post/);
  assert.match(old.body, /<link rel="canonical" href="https:\/\/blog\.test\/vi\/search">/);
  const q = await text("/vi/search?q=plums");
  assert.equal(q.res.headers.get("cache-control"), "no-store");
  assert.doesNotMatch(q.body, /Old search post/);
  assert.match(q.body, /noindex/);
});
