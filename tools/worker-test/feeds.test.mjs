// Feeds, sitemap, robots, llms.txt and page JSON-LD through the real Worker.
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

const get = (path) => fetch(`${worker.url}${path}`, { redirect: "manual" });
const RFC3339 = /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$/;

test("JSON Feed 1.1: required fields, absolute URLs, dates", async () => {
  const res = await get("/feed.json");
  assert.equal(res.status, 200);
  assert.equal(res.headers.get("content-type"), "application/feed+json; charset=utf-8");
  const feed = await res.json();
  assert.equal(feed.version, "https://jsonfeed.org/version/1.1");
  assert.equal(typeof feed.title, "string");
  assert.equal(feed.feed_url, `${worker.url}/feed.json`);
  assert.equal(feed.home_page_url, `${worker.url}/`);
  assert.ok(feed.items.length >= 7);
  const ids = new Set();
  for (const item of feed.items) {
    assert.ok(item.id && !ids.has(item.id), "unique id");
    ids.add(item.id);
    assert.ok(item.url.startsWith(worker.url + "/"));
    assert.ok(item.content_html.length > 0);
    assert.match(item.date_published, RFC3339);
    assert.doesNotMatch(item.content_html, /(href|src)="\//, "content links are absolute");
  }
});

test("feeds and sitemap go through the versioned cache", async () => {
  for (const path of ["/feed.xml", "/rss.xml", "/feed.json", "/sitemap.xml", "/robots.txt", "/llms.txt"]) {
    const first = await get(path);
    assert.equal(first.status, 200, path);
    assert.ok(first.headers.get("etag"), `${path} has an ETag`);
    await first.arrayBuffer();
    let hit = false;
    for (let i = 0; i < 20 && !hit; i++) {
      const r = await get(path);
      await r.arrayBuffer();
      hit = r.headers.get("x-plumblog-cache") === "hit";
      if (!hit) await new Promise((ok) => setTimeout(ok, 100));
    }
    assert.ok(hit, `${path} came from cache`);
  }
});

function jsonLd(html) {
  const m = html.match(/<script type="application\/ld\+json">([\s\S]*?)<\/script>/);
  assert.ok(m, "page has JSON-LD");
  const doc = JSON.parse(m[1]);
  assert.equal(doc["@context"], "https://schema.org");
  return doc["@graph"];
}

function checkBreadcrumbs(list, lastUrl) {
  assert.equal(list["@type"], "BreadcrumbList");
  list.itemListElement.forEach((item, i) => {
    assert.equal(item["@type"], "ListItem");
    assert.equal(item.position, i + 1);
    assert.ok(item.name);
    assert.ok(item.item.startsWith(worker.url + "/"));
  });
  assert.equal(list.itemListElement.at(-1).item, lastUrl);
}

test("post JSON-LD: BlogPosting with the fields rich results need, plus breadcrumbs", async () => {
  const html = await (await get("/reading-on-screens")).text();
  const [article, crumbs] = jsonLd(html);
  assert.equal(article["@type"], "BlogPosting");
  assert.equal(article.headline, "Reading on screens, done properly");
  assert.ok(article.headline.length <= 110);
  assert.match(article.datePublished, RFC3339);
  assert.match(article.dateModified, RFC3339);
  assert.equal(article.url, `${worker.url}/reading-on-screens`);
  assert.ok(article.author?.name, "author has a name");
  assert.equal(article.publisher["@type"], "Organization");
  checkBreadcrumbs(crumbs, `${worker.url}/reading-on-screens`);
  assert.match(html, /<meta property="og:type" content="article">/);
  assert.match(html, /<meta property="og:url" content="http:\/\/127\.0\.0\.1:\d+\/reading-on-screens">/);
});

test("home is a Blog; tag and archive pages are collections with breadcrumbs", async () => {
  const [blog] = jsonLd(await (await get("/")).text());
  assert.equal(blog["@type"], "Blog");
  assert.equal(blog.url, `${worker.url}/`);
  for (const path of ["/tag/tools", "/archive/2026"]) {
    const [page, crumbs] = jsonLd(await (await get(path)).text());
    assert.equal(page["@type"], "CollectionPage", path);
    checkBreadcrumbs(crumbs, `${worker.url}${path}`);
  }
});

test("pages link all three feeds", async () => {
  const html = await (await get("/")).text();
  for (const [type, href] of [["application/atom+xml", "/feed.xml"], ["application/rss+xml", "/rss.xml"], ["application/feed+json", "/feed.json"]]) {
    assert.ok(html.includes(`type="${type}" title="Plum Notes" href="${href}"`), type);
    assert.equal((await get(href)).status, 200);
  }
});
