// The admin shell, manifest and icon from the real Worker, and the built client running in
// the system Chrome under the shell's CSP (docs/ARCHITECTURE.md, Admin and editor).
import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { chromium } from "playwright-core";
import { startWorker } from "./harness.mjs";

let worker;
before(async () => {
  worker = await startWorker();
});
after(async () => {
  await worker?.stop();
});

const boot = (html) => JSON.parse(html.match(/<script id="boot" type="application\/json">(.*?)<\/script>/s)[1]);

test("every admin path gets the same shell, with a strict CSP and no caching", async () => {
  for (const path of ["/admin/", "/admin/pages", "/admin/posts/new", "/admin/no/such/screen"]) {
    const res = await fetch(worker.url + path);
    assert.equal(res.status, 200, path);
    assert.match(res.headers.get("content-type"), /^text\/html/);
    const csp = res.headers.get("content-security-policy");
    assert.match(csp, /script-src 'self';/);
    // The nonce admits only the editor's own <style> (CodeMirror); fresh on every response.
    const nonce = /style-src 'self' 'nonce-([0-9a-f]{32})';/.exec(csp)?.[1];
    assert.ok(nonce, csp);
    const again = (await fetch(res.url)).headers.get("content-security-policy");
    assert.ok(!again.includes(nonce), "a new nonce per response");
    assert.match(csp, /frame-ancestors 'none'/);
    assert.equal(res.headers.get("cache-control"), "no-cache");
    assert.equal(res.headers.get("x-robots-tag"), "noindex");
    const html = await res.text();
    assert.match(html, /<script type="module" src="\/admin\/app.js"><\/script>/);
    // The UI text is the admin bundle's, preloaded; the shell carries only the language.
    assert.equal(boot(html).lang, "en");
    assert.equal(boot(html).messages, undefined);
    assert.match(html, /<link rel="modulepreload" href="\/admin\/i18n\/en.js">/);
  }
});

test("only the sign-in, setup and invite pages admit Turnstile in their CSP", async () => {
  for (const path of ["/admin/signin", "/admin/setup", "/admin/invite", "/admin/signin/"]) {
    const csp = (await fetch(worker.url + path)).headers.get("content-security-policy");
    assert.match(csp, /script-src 'self' https:\/\/challenges\.cloudflare\.com;/, path);
    assert.match(csp, /frame-src https:\/\/challenges\.cloudflare\.com;/, path);
    assert.match(csp, /frame-ancestors 'none'/, path);
  }
  for (const path of ["/admin/", "/admin/settings", "/admin/signin/x", "/admin/posts/1"]) {
    const csp = (await fetch(worker.url + path)).headers.get("content-security-policy");
    assert.ok(!csp.includes("challenges.cloudflare.com"), path);
  }
});

test("/admin redirects to /admin/", async () => {
  const res = await fetch(`${worker.url}/admin`, { redirect: "manual" });
  assert.equal(res.status, 301);
  assert.equal(res.headers.get("location"), "/admin/");
});

test("the language comes from the cookie, then Accept-Language", async () => {
  const vi = await (await fetch(`${worker.url}/admin/`, { headers: { cookie: "pb_lang=vi" } })).text();
  assert.match(vi, /<html lang="vi"/);
  assert.equal(boot(vi).lang, "vi");
  assert.match(vi, /href="\/admin\/i18n\/vi.js"/);
  const header = await (await fetch(`${worker.url}/admin/`, { headers: { "accept-language": "vi-VN,en;q=0.5" } })).text();
  assert.match(header, /<html lang="vi"/);
});

test("manifest and icon make an installable app scoped to /admin/", async () => {
  const res = await fetch(`${worker.url}/admin/manifest.webmanifest`);
  assert.match(res.headers.get("content-type"), /^application\/manifest\+json/);
  const m = await res.json();
  assert.equal(m.scope, "/admin/");
  assert.equal(m.start_url, "/admin/");
  assert.equal(m.display, "standalone");
  assert.equal(m.icons[0].src, "/admin/icon.svg");
  const icon = await fetch(`${worker.url}/admin/icon.svg`);
  assert.match(icon.headers.get("content-type"), /^image\/svg\+xml/);
  assert.match(await icon.text(), /^<svg /);
});

test("the built client, its stylesheet and service worker are static assets", async () => {
  for (const [path, type] of [
    ["/admin/app.js", /javascript/],
    ["/admin/app.css", /css/],
    ["/admin/sw.js", /javascript/],
  ]) {
    const res = await fetch(worker.url + path);
    assert.equal(res.status, 200, path);
    assert.match(res.headers.get("content-type"), type, path);
    // Not hashed: revalidated on every load, so a deploy is picked up (_headers from admin-build).
    assert.equal(res.headers.get("cache-control"), "no-cache", path);
  }
});

test("each language's UI text is a static module with English behind it", async () => {
  const mod = async (l) => {
    const res = await fetch(`${worker.url}/admin/i18n/${l}.js`);
    assert.equal(res.status, 200, l);
    assert.match(res.headers.get("content-type"), /javascript/, l);
    assert.equal(res.headers.get("cache-control"), "no-cache", l);
    const src = await res.text();
    return JSON.parse(src.replace(/^export default /, "").replace(/;\s*$/, ""));
  };
  const en = await mod("en");
  assert.equal(en.messages["admin.nav.posts"], "Posts");
  assert.deepEqual(en.fallback, {});
  const vi = await mod("vi");
  assert.equal(vi.messages["admin.nav.posts"], "B\u00e0i vi\u1ebft");
});

test("hashed chunks (the editor) are cached for a year", async () => {
  const app = await (await fetch(`${worker.url}/admin/app.js`)).text();
  const chunk = /chunks\/editor-[\w-]+\.js/.exec(app)?.[0];
  assert.ok(chunk, "app.js loads the editor chunk");
  const res = await fetch(`${worker.url}/admin/${chunk}`);
  assert.equal(res.status, 200);
  assert.match(res.headers.get("content-type"), /javascript/);
  assert.equal(res.headers.get("cache-control"), "public, max-age=31536000, immutable");
});

test("a missing admin file is a 404, never the shell under a script URL", async () => {
  // An open tab after a deploy still asks for the old chunk; HTML here would break it.
  for (const path of ["/admin/chunks/editor-OLDHASH.js", "/admin/app.js.map", "/admin/old.css"]) {
    const res = await fetch(worker.url + path);
    assert.equal(res.status, 404, path);
    assert.match(res.headers.get("content-type"), /^text\/plain/, path);
    assert.equal(res.headers.get("cache-control"), "no-store", path);
  }
});

test("the client runs under the CSP and shows the shell with a plain error while the API is missing", async () => {
  const browser = await chromium.launch({ channel: "chrome", headless: true });
  try {
    // A fixed browser language, so the shell is English on every machine.
    const page = await browser.newPage({ viewport: { width: 1440, height: 900 }, locale: "en-US" });
    const problems = [];
    page.on("pageerror", (e) => problems.push(`page error: ${e}`));
    page.on("console", (m) => {
      // A failed API call logs; that is the client doing its job. CSP violations are not.
      if (/Content Security Policy|Refused to/.test(m.text())) problems.push(`csp: ${m.text()}`);
    });
    await page.goto(`${worker.url}/admin/`, { waitUntil: "networkidle" });
    await page.waitForSelector("main [role=alert], main h1");
    assert.deepEqual(problems, []);
    const nav = await page.locator("nav.side a").allTextContents();
    assert.deepEqual(nav.slice(0, 3), ["Posts", "Pages", "Media"]);
    // No signed-in user yet: no role, so no Settings link, and a retry button, not a blank page.
    assert.equal(await page.locator('nav.side a[href="/admin/settings"]').count(), 0);
    assert.equal(await page.locator("main [role=alert] button").count(), 1);
  } finally {
    await browser.close();
  }
});

test("a draft previews as the published page, with no script for the frame to block", async () => {
  const body = "## Ch\u01b0\u01a1ng m\u1ed9t\n\nC\u00f3 $x^2$ v\u00e0 <b>raw</b>.\n\n```rust\nfn main() {}\n```\n";
  const res = await fetch(`${worker.url}/__preview`, {
    method: "POST",
    body: new URLSearchParams({ title: "B\u00e0i <m\u1edbi>", body, lang: "vi" }),
  });
  assert.equal(res.status, 200);
  assert.equal(res.headers.get("cache-control"), "no-store");
  assert.equal(res.headers.get("x-robots-tag"), "noindex");
  assert.match(res.headers.get("server-timing"), /^render;dur=\d+$/);
  // No script even opened on its own; framed only by the admin.
  assert.equal(res.headers.get("content-security-policy"), "sandbox allow-same-origin; script-src 'none'; frame-ancestors 'self'");
  const html = await res.text();
  assert.match(html, /B\u00e0i &lt;m\u1edbi&gt;/);
  assert.match(html, /<math/);
  assert.doesNotMatch(html, /<b>raw/);
  assert.doesNotMatch(html, /<script/i);
});
