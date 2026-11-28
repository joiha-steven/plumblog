// The admin shell, manifest and icon from the real Worker, and the built client running in
// the system Chrome under the shell's CSP (docs/ARCHITECTURE.md, Admin and editor).
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
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

// The shell names the entry's static imports so the browser fetches them beside app.js. The
// expected list is computed here from esbuild's metafile, not from the file the Worker was
// built with, so a Worker built against an older admin (or none) fails.
test("the shell preloads exactly the static chunks of app.js, from the build's metafile", () => {
  const outputs = JSON.parse(readFileSync(new URL("../../target/admin.meta.json", import.meta.url), "utf8")).outputs;
  const seen = new Set();
  const todo = ["target/admin/app.js"];
  while (todo.length) {
    const f = todo.pop();
    if (seen.has(f)) continue;
    seen.add(f);
    for (const i of outputs[f].imports) if (i.kind === "import-statement") todo.push(i.path);
  }
  seen.delete("target/admin/app.js");
  const expected = [...seen].map((f) => "/admin/" + f.slice("target/admin/".length)).sort();
  assert.ok(expected.length > 0 && expected.every((u) => u.startsWith("/admin/chunks/")), "the entry has static chunks");
  return fetch(worker.url + "/admin/").then(async (res) => {
    const html = await res.text();
    const got = [...html.matchAll(/<link rel="modulepreload" href="(\/admin\/chunks\/[^"]+)">/g)].map((m) => m[1]).sort();
    assert.deepEqual(got, expected);
    for (const u of expected) assert.equal((await fetch(worker.url + u)).status, 200, u);
  });
});

// Faces nobody on the page uses only compete with what the first paint waits for: the posts
// list scored 1.8 s LCP with them, 1.2 s without.
test("the writing faces are preloaded on the editor and sign-in pages only", async () => {
  const faces = async (path) => ((await (await fetch(worker.url + path)).text()).match(/<link rel="preload"[^>]*as="font"/g) ?? []).length;
  for (const path of ["/admin/posts/1", "/admin/posts/new", "/admin/signin"]) assert.ok((await faces(path)) >= 2, path);
  for (const path of ["/admin/", "/admin/pages", "/admin/media", "/admin/settings"]) assert.equal(await faces(path), 0, path);
});

test("only the sign-in, setup and invite pages admit Turnstile in their CSP", async () => {
  for (const path of ["/admin/signin", "/admin/setup", "/admin/invite", "/admin/signin/"]) {
    const csp = (await fetch(worker.url + path)).headers.get("content-security-policy");
    assert.match(csp, /script-src 'self' https:\/\/challenges\.cloudflare\.com;/, path);
    assert.match(csp, /frame-src https:\/\/challenges\.cloudflare\.com;/, path);
    assert.match(csp, /frame-ancestors 'none'/, path);
  }
  for (const path of ["/admin/", "/admin/settings", "/admin/signin/x", "/admin/posts/1", "/admin/signin//", "/admin/setup///"]) {
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

test("signed out, the client runs under the CSP and sends the writer to a plain sign-in page", async () => {
  const browser = await chromium.launch({ channel: "chrome", headless: true });
  try {
    // A fixed browser language, so the page is English on every machine.
    const page = await browser.newPage({ viewport: { width: 1440, height: 900 }, locale: "en-US" });
    const problems = [];
    page.on("pageerror", (e) => problems.push(`page error: ${e}`));
    page.on("console", (m) => {
      // A failed API call logs; that is the client doing its job. CSP violations are not.
      if (/Content Security Policy|Refused to/.test(m.text())) problems.push(`csp: ${m.text()}`);
    });
    await page.goto(`${worker.url}/admin/settings?tab=x`, { waitUntil: "networkidle" });
    // A full page load to the sign-in page (its CSP differs), remembering where the writer was.
    await page.waitForSelector("form #login");
    const url = new URL(page.url());
    assert.equal(url.pathname, "/admin/signin");
    assert.equal(url.searchParams.get("next"), "/admin/settings?tab=x");
    assert.equal(await page.locator("h1").innerText(), "Sign in");
    assert.equal(await page.locator("#password").getAttribute("autocomplete"), "current-password");
    assert.equal(await page.locator("nav.side").count(), 0);
    assert.deepEqual(problems, []);
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

// --- System log screen (web/admin/src/syslog/) ---

const sq = (v) => `'${String(v).replaceAll("'", "''")}'`;
const sha256hex = (b) => createHash("sha256").update(b).digest("hex");
const en = (() => {
  const out = {};
  let section = "";
  for (const line of readFileSync(new URL("../../locales/en.toml", import.meta.url), "utf8").split("\n")) {
    const s = /^\[(.+)\]$/.exec(line);
    if (s) section = s[1];
    const kv = /^(\w+) = "((?:[^"\\]|\\.)*)"$/.exec(line);
    if (kv) out[`${section}.${kv[1]}`] = kv[2].replaceAll('\\"', '"');
  }
  return out;
})();
const tx = (key) => {
  assert.ok(en[key], `locale key ${key}`);
  return en[key];
};

/** A page that records page errors, CSP violations and console errors other than refused calls. */
async function logPage(browser) {
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, locale: "en-US" });
  await context.addInitScript(() => {
    document.addEventListener("securitypolicyviolation", (e) => console.error(`CSPVIOLATION ${e.violatedDirective} ${e.blockedURI}`));
  });
  const page = await context.newPage();
  const problems = [];
  page.on("pageerror", (e) => problems.push(`page error: ${e}`));
  page.on("console", (m) => {
    if (m.type() === "error" && !/^Failed to load resource: the server responded with a status of 40[134]/.test(m.text())) problems.push(`console: ${m.text()}`);
  });
  return { page, problems };
}

test("the owner reads the System log and opens an entry; a writer sees the lock and never asks the API", async () => {
  const T = Date.now();
  const SETUP_CODE = "setup-code-syslog-0123456789";
  const INVITE = "invite-writer-syslog-code";
  const at = (w, path) => `${w.url.replace("127.0.0.1", "localhost")}${path}`;
  const [owner, invited] = await Promise.all([
    startWorker({
      seedSql: (url) => `INSERT INTO settings (key, value_json, updated_at) VALUES
        ('site_id', ${sq(JSON.stringify("site-syslog-1"))}, ${T}),
        ('origin', ${sq(JSON.stringify(url.replace("127.0.0.1", "localhost")))}, ${T}),
        ('setup_code_hash', ${sq(JSON.stringify(sha256hex(SETUP_CODE)))}, ${T}),
        ('setup_code_expires_at', '${T + 86_400_000}', ${T});
        INSERT INTO system_log (at_ms, request_id, kind, detail) VALUES (${T - 60_000}, 'req-smoke-1', 'job.run', ${sq("Smoke test entry\nrows\t3")});`,
    }),
    startWorker({
      seedSql: `INSERT INTO settings (key, value_json, updated_at) VALUES ('site_id', ${sq(JSON.stringify("site-syslog-2"))}, ${T});
        INSERT INTO users (id, login, slug, display_name, role, password_hash, disabled_at, created_at, updated_at)
          VALUES (1, 'boss', 'boss', 'Boss', 'owner', 'p1$1000$AAAAAAAAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA', NULL, ${T}, ${T});
        INSERT INTO invites (code_hash, role, created_by, created_at, expires_at)
          VALUES (${sq(sha256hex(INVITE))}, 'writer', 1, ${T}, ${T + 86_400_000});`,
    }),
  ]);
  const browser = await chromium.launch({ channel: "chrome", headless: true });
  try {
    const signUp = async (page, w, path, values) => {
      await page.goto(at(w, path));
      await page.getByLabel(tx("admin.auth.login")).fill(values.login);
      await page.getByLabel(tx("admin.auth.slug")).fill(values.slug);
      if (values.name) await page.getByLabel(tx("admin.auth.display_name")).fill(values.name);
      await page.getByLabel(tx("admin.auth.password"), { exact: true }).fill(values.password);
      await page.getByLabel(tx("admin.auth.password_repeat")).fill(values.password);
      await page.getByRole("button", { name: tx("admin.auth.create") }).click();
      await page.waitForURL(/\/admin\/$/);
    };

    // Owner: the entry is there, opens in place and shows its request id.
    const o = await logPage(browser);
    await signUp(o.page, owner, `/admin/setup?code=${SETUP_CODE}`, { login: "Hana", slug: "hana", name: "Hana Owner", password: "correct horse battery" });
    await o.page.goto(at(owner, "/admin/settings/log"), { waitUntil: "networkidle" });
    assert.equal(await o.page.locator("h1").innerText(), tx("admin.nav.settings"), "the log wears the Settings header");
    await o.page.locator(".log-btn").first().waitFor();
    assert.ok((await o.page.locator(".log-btn").count()) >= 1);
    const smoke = o.page.locator(".log-btn", { hasText: "Smoke test entry" });
    assert.equal(await smoke.getAttribute("aria-expanded"), "false");
    await smoke.click();
    assert.equal(await smoke.getAttribute("aria-expanded"), "true");
    const detail = o.page.locator(".log-detail:not([hidden])");
    await detail.getByText("req-smoke-1", { exact: true }).waitFor();
    await detail.getByRole("button", { name: tx("admin.syslog.copy_ai") }).waitFor();
    assert.equal(await o.page.locator('.nav a[aria-current="page"]').count(), 1, "Settings is the current item");
    // The Settings section list: System log is the current section and General links back.
    const subnav = o.page.locator(".settings-main").locator("xpath=preceding-sibling::nav[contains(@class,'subnav')]");
    assert.equal(await subnav.locator('a[aria-current="page"]').innerText(), tx("admin.syslog.title"));
    assert.equal(await subnav.getByRole("link", { name: tx("admin.settings.general") }).getAttribute("href"), "/admin/settings");
    // A filter with no match says so, and Clear brings the entry back.
    await o.page.getByRole("searchbox", { name: tx("admin.syslog.search") }).fill("no such text");
    await o.page.getByText(tx("admin.syslog.none_title")).waitFor();
    assert.match(o.page.url(), /\?q=no\+such\+text$/);
    await o.page.getByRole("button", { name: tx("admin.syslog.clear") }).click();
    await o.page.locator(".log-btn", { hasText: "Smoke test entry" }).waitFor();
    assert.deepEqual(o.problems, []);

    // Writer: the lock, and no request to the log API.
    const w = await logPage(browser);
    const asked = [];
    w.page.on("request", (r) => /\/api\/v1\/log/.test(r.url()) && asked.push(r.url()));
    await w.page.goto(at(invited, `/admin/invite?code=${INVITE}`));
    await w.page.getByLabel(tx("admin.auth.login")).fill("wren");
    await w.page.getByLabel(tx("admin.auth.slug")).fill("wren");
    await w.page.getByLabel(tx("admin.auth.display_name")).fill("Wren Writer");
    await w.page.getByLabel(tx("admin.auth.password"), { exact: true }).fill("writer pass phrase 1");
    await w.page.getByLabel(tx("admin.auth.password_repeat")).fill("writer pass phrase 1");
    await w.page.getByRole("button", { name: tx("admin.auth.create") }).click();
    await w.page.waitForURL(/\/admin\/$/);
    await w.page.goto(at(invited, "/admin/settings/log"), { waitUntil: "networkidle" });
    await w.page.getByText(tx("admin.state.no_permission_title")).waitFor();
    assert.equal(await w.page.locator(".log-btn").count(), 0);
    assert.deepEqual(asked, [], "a writer never calls the log API");
    assert.deepEqual(w.problems, []);
  } finally {
    await browser.close();
    await Promise.all([owner.stop(), invited.stop()]);
  }
});
