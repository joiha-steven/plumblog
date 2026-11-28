// Feeds, sitemap, robots, llms.txt and page JSON-LD through the real Worker.
import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { request } from "node:http";
import { seedSample, startWorker } from "./harness.mjs";

let worker;
before(async () => {
  worker = await startWorker();
  await seedSample(worker);
});
after(async () => {
  await worker?.stop();
});

const get = (path) => fetch(`${worker.url}${path}`, { redirect: "manual" });
// The sample site's configured origin (crates/worker/sample/site.txt): public URLs use it,
// whatever host the request came to.
const SITE = "https://blog.test";
const RFC3339 = /^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$/;

test("JSON Feed 1.1: required fields, absolute URLs, dates", async () => {
  const res = await get("/feed.json");
  assert.equal(res.status, 200);
  assert.equal(res.headers.get("content-type"), "application/feed+json; charset=utf-8");
  const feed = await res.json();
  assert.equal(feed.version, "https://jsonfeed.org/version/1.1");
  assert.equal(typeof feed.title, "string");
  assert.equal(feed.feed_url, `${SITE}/feed.json`);
  assert.equal(feed.home_page_url, `${SITE}/`);
  assert.ok(feed.items.length >= 7);
  const ids = new Set();
  for (const item of feed.items) {
    assert.ok(item.id && !ids.has(item.id), "unique id");
    ids.add(item.id);
    assert.ok(item.url.startsWith(SITE + "/"));
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
    assert.ok(item.item.startsWith(SITE + "/"));
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
  assert.equal(article.url, `${SITE}/reading-on-screens`);
  assert.ok(article.author?.name, "author has a name");
  assert.equal(article.publisher["@type"], "Organization");
  checkBreadcrumbs(crumbs, `${SITE}/reading-on-screens`);
  assert.match(html, /<meta property="og:type" content="article">/);
  assert.match(html, /<meta property="og:url" content="https:\/\/blog\.test\/reading-on-screens">/);
});

test("home is a Blog; tag and archive pages are collections with breadcrumbs", async () => {
  const [blog] = jsonLd(await (await get("/")).text());
  assert.equal(blog["@type"], "Blog");
  assert.equal(blog.url, `${SITE}/`);
  for (const path of ["/tag/tools", "/archive/2026"]) {
    const [page, crumbs] = jsonLd(await (await get(path)).text());
    assert.equal(page["@type"], "CollectionPage", path);
    checkBreadcrumbs(crumbs, `${SITE}${path}`);
  }
});

test("pages link all three feeds", async () => {
  const html = await (await get("/")).text();
  for (const [type, href] of [["application/atom+xml", "/feed.xml"], ["application/rss+xml", "/rss.xml"], ["application/feed+json", "/feed.json"]]) {
    assert.ok(html.includes(`type="${type}" title="Plum Notes" href="${href}"`), type);
    assert.equal((await get(href)).status, 200);
  }
});

// fetch() may not let a caller choose Host, so this sends one with node:http.
function getWithHost(path, host) {
  const { hostname, port } = new URL(worker.url);
  return new Promise((resolve, reject) => {
    const req = request({ hostname, port, path, headers: { host } }, (res) => {
      let body = "";
      res.setEncoding("utf8");
      res.on("data", (c) => (body += c));
      res.on("end", () => resolve({ status: res.statusCode, headers: res.headers, body }));
    });
    req.on("error", reject);
    req.end();
  });
}

test("an arbitrary Host header never reaches a public URL", async () => {
  // Proof the Host arrives: redirects stay on the host the reader used.
  const probe = await getWithHost("/page/1", "evil.test");
  assert.equal(probe.status, 301);
  assert.match(probe.headers.location, /^http:\/\/evil\.test(:\d+)?\/$/);
  for (const path of ["/", "/reading-on-screens", "/feed.xml", "/rss.xml", "/feed.json", "/sitemap.xml", "/robots.txt", "/llms.txt"]) {
    const { status, body } = await getWithHost(path, "evil.test");
    assert.equal(status, 200, path);
    assert.ok(!body.includes("evil.test"), `${path} used the Host header`);
    assert.ok(body.includes(`${SITE}/`), path);
  }
});
