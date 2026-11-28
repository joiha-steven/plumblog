// The public site reads the store (issue #76): what the owner saves through /api/v1 shows up
// on the reader pages, drafts and scheduled posts do not, languages live under their own
// prefix, and an empty site renders with defaults. Runs against an empty database: no seed.
import assert from "node:assert/strict";
import { createHash, randomBytes } from "node:crypto";
import { after, before, test } from "node:test";
import { startWorker } from "./harness.mjs";

const T = Date.now();
const DAY = 86_400_000;
const TOKEN = `plb_${randomBytes(32).toString("base64url")}`;
const hash = createHash("sha256").update(TOKEN).digest("hex");

let worker;
let hello;
before(async () => {
  worker = await startWorker({
    seedSql: `INSERT INTO users (id, login, slug, display_name, role, password_hash, disabled_at, created_at, updated_at) VALUES (1, 'olga', 'olga', 'Olga', 'owner', NULL, NULL, ${T}, ${T});
      INSERT INTO api_tokens (id, user_id, name, token_hash, scope, created_at, expires_at, revoked_at) VALUES (1, 1, 'boss', '${hash}', 'owner', ${T}, NULL, NULL);
      INSERT INTO settings (key, value_json, updated_at) VALUES ('origin', '"https://blog.test"', ${T});`,
  });
});
after(async () => {
  await worker?.stop();
});

const get = (path) => fetch(`${worker.url}${path}`, { redirect: "manual" });
const text = async (path) => {
  const res = await get(path);
  return { status: res.status, body: await res.text(), location: res.headers.get("location") };
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
  const r = await api("POST", "/posts", { markdown: "Some **words** here.", ...post });
  assert.equal(r.status, 201, r.raw);
  return r.json;
};

test("an empty site renders with defaults, never a 500", async () => {
  const home = await text("/");
  assert.equal(home.status, 200, home.body);
  assert.match(home.body, /<title>Untitled site<\/title>/);
  for (const path of ["/feed.xml", "/rss.xml", "/feed.json", "/sitemap.xml", "/llms.txt", "/robots.txt"]) {
    assert.equal((await text(path)).status, 200, path);
  }
  assert.doesNotMatch((await text("/feed.xml")).body, /<entry>/);
  // The site's own setting writes date the feeds; the sitemap states no date it does not have.
  for (const path of ["/feed.xml", "/rss.xml", "/feed.json", "/sitemap.xml"]) {
    assert.doesNotMatch((await text(path)).body, /1970/, path);
  }
  assert.doesNotMatch((await text("/sitemap.xml")).body, /<lastmod>/);
  for (const path of ["/nothing", "/page/2", "/tag/none", "/archive/2025", "/vi/nothing"]) {
    assert.equal((await text(path)).status, 404, path);
  }
});

test("site settings go through the settings API and show on the pages", async () => {
  for (const [key, value] of [
    ["site_title", "My Blog"],
    ["site_description", "Notes."],
    ["site_lang", "en"],
    ["site_languages", ["en", "vi"]],
    ["site_per_page", 10],
  ]) {
    const r = await api("PUT", `/settings/${key}`, { value });
    assert.equal(r.status, 204, `${key}: ${r.raw}`);
  }
  const home = await text("/");
  assert.match(home.body, /<title>My Blog<\/title>/);
});

test("published posts appear everywhere; drafts and scheduled posts do not", async () => {
  hello = await create({ lang: "en", slug: "hello-world", title: "Hello world", status: "published" });
  await create({ lang: "en", slug: "just-a-draft", title: "Just a draft", status: "draft", published_at: null });
  await create({ lang: "en", slug: "later-on", title: "Later on", status: "scheduled", published_at: T + DAY });

  const home = await text("/");
  assert.match(home.body, /href="\/hello-world"/);
  assert.equal((await text("/hello-world")).status, 200);
  assert.match((await text("/feed.xml")).body, /hello-world/);
  assert.match((await text("/sitemap.xml")).body, /<loc>https:\/\/blog\.test\/hello-world<\/loc>/);

  for (const slug of ["just-a-draft", "later-on"]) {
    assert.equal((await text(`/${slug}`)).status, 404, slug);
    assert.doesNotMatch(home.body, new RegExp(slug));
    for (const path of ["/feed.xml", "/sitemap.xml", "/llms.txt"]) {
      assert.doesNotMatch((await text(path)).body, new RegExp(slug), `${slug} in ${path}`);
    }
  }
});

test("published with a future date is refused, and reserved slugs are not for the default language", async () => {
  const future = await api("POST", "/posts", { lang: "en", slug: "too-early", title: "T", markdown: "x", status: "published", published_at: T + DAY });
  assert.equal(future.status, 400, future.raw);
  assert.deepEqual(future.json, { error: "post.published_in_future", field: "published_at" });
  for (const slug of ["page", "tag", "series", "archive", "search", "theme", "plugin", "media", "admin", "api", "healthz"]) {
    const r = await api("POST", "/posts", { lang: "en", slug, title: "T", markdown: "x", status: "draft" });
    assert.deepEqual(r.json, { error: "input.invalid", field: "slug" }, slug);
  }
  // `search` is refused in every language: /<lang>/search is that language's search page.
  const viSearch = await api("POST", "/posts", { lang: "vi", slug: "search", title: "T", markdown: "x", status: "draft" });
  assert.equal(viSearch.status, 400, viSearch.raw);
  assert.deepEqual(viSearch.json, { error: "input.invalid", field: "slug" });
  // Another language lives under /<lang>/, where the other words are free.
  const vi = await api("POST", "/posts", { lang: "vi", slug: "page", title: "T", markdown: "x", status: "draft" });
  assert.equal(vi.status, 201, vi.raw);
});

test("another language lives under /<lang>/, with alternates both ways", async () => {
  const en = hello;
  await create({ lang: "vi", slug: "chao-the-gioi", title: "Chao the gioi", status: "published", translation_of: en.id });

  assert.equal((await text("/chao-the-gioi")).status, 404, "not at the root");
  const vi = await text("/vi/chao-the-gioi");
  assert.equal(vi.status, 200, vi.body);
  assert.match(vi.body, /<html lang="vi"/);
  assert.match(vi.body, /hreflang="en" href="https:\/\/blog\.test\/hello-world"/);
  const page = await text("/hello-world");
  assert.match(page.body, /hreflang="vi" href="https:\/\/blog\.test\/vi\/chao-the-gioi"/);
  assert.match(page.body, /hreflang="x-default" href="https:\/\/blog\.test\/hello-world"/);

  // The default language has one URL: its prefixed form redirects.
  const moved = await text("/en/hello-world");
  assert.equal(moved.status, 301);
  assert.match(moved.location, /^https?:\/\/[^/]+\/hello-world$/);
  assert.equal((await text("/vi/hello-world")).status, 404, "a post is in one language");

  // The home page and /page/N are the default language only, like the feed.
  assert.doesNotMatch((await text("/")).body, /chao-the-gioi/);
  assert.doesNotMatch((await text("/nothing")).body, /chao-the-gioi/);
  assert.match((await text("/vi/feed.xml")).body, /chao-the-gioi/);
  assert.doesNotMatch((await text("/feed.xml")).body, /chao-the-gioi/);
  const map = (await text("/sitemap.xml")).body;
  assert.match(map, /<loc>https:\/\/blog\.test\/vi\/chao-the-gioi<\/loc>/);
  assert.match(map, /hreflang="vi"/);
});

test("each language has its own home, with its own feed links and hreflang", async () => {
  const vi = await text("/vi/");
  assert.equal(vi.status, 200, vi.body);
  assert.match(vi.body, /<html lang="vi"/);
  assert.match(vi.body, /href="\/vi\/chao-the-gioi"/);
  assert.doesNotMatch(vi.body, /hello-world/, "only vi posts");
  assert.match(vi.body, /<link rel="canonical" href="https:\/\/blog\.test\/vi\/">/);
  assert.match(vi.body, /href="\/vi\/feed\.xml"/);
  assert.doesNotMatch(vi.body, /href="\/feed\.xml"/);
  assert.match(vi.body, /hreflang="en" href="https:\/\/blog\.test\/"/);
  assert.match(vi.body, /hreflang="vi" href="https:\/\/blog\.test\/vi\/"/);
  assert.match(vi.body, /hreflang="x-default" href="https:\/\/blog\.test\/"/);
  assert.match((await text("/")).body, /hreflang="vi" href="https:\/\/blog\.test\/vi\/"/);

  // One URL per page; unknown languages and pages past the end are 404.
  assert.equal((await text("/vi/page/1")).status, 301);
  assert.equal((await text("/vi/page/2")).status, 404);
  const en = await text("/en/");
  assert.equal(en.status, 301);
  assert.match(en.location, /^https?:\/\/[^/]+\/$/);
  assert.equal((await text("/xx/")).status, 404);
  assert.equal((await text("/fr/page/1")).status, 404, "not a language: no redirect into a 404");
  assert.match(vi.body, /class="site-name" href="\/vi\/"/);
  assert.match((await text("/sitemap.xml")).body, /<loc>https:\/\/blog\.test\/vi\/<\/loc>/);
});

test("pages past the end are 404 from the count, not errors", async () => {
  assert.equal((await text("/page/1")).status, 301);
  for (const path of ["/page/2", "/page/1001", "/page/999999", "/tag/none/page/2", "/archive/2026/page/1001"]) {
    assert.equal((await text(path)).status, 404, path);
  }
});

test("unpublishing makes the post a 404 and removes it from every listing", async () => {
  const en = hello;
  const r = await api("PATCH", `/posts/${en.id}`, { expected_version: en.version, status: "draft", published_at: null });
  assert.equal(r.status, 200, r.raw);
  assert.equal((await text("/hello-world")).status, 404);
  assert.doesNotMatch((await text("/")).body, /hello-world/);
  assert.doesNotMatch((await text("/feed.xml")).body, /hello-world/);
  assert.doesNotMatch((await text("/sitemap.xml")).body, /hello-world/);
});
