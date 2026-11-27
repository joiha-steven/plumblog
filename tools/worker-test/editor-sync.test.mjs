// The editor saving a draft to the server (web/admin/src/editor/sync.ts) in the system Chrome
// against the real Worker and a real local D1: create, the URL moving to the post, an update
// with the next version, and a concurrent change from elsewhere answered with the choice.
// UI text is read from locales/en.toml, never hard-coded here.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { after, before, test } from "node:test";
import { chromium } from "playwright-core";
import { startWorker } from "./harness.mjs";

const sq = (s) => `'${String(s).replaceAll("'", "''")}'`;
const T = Date.now();
const SETUP_CODE = "setup-code-sync-0123456789";
const OWNER = { login: "Wren", slug: "wren", name: "Wren Owner", password: "correct horse battery" };
let session;

// String values of locales/en.toml as "section.key" (as in auth-ui.test.mjs).
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

let w, browser;
// Opened as localhost, the name the configured origin uses.
const at = (path = "") => `${w.url.replace("127.0.0.1", "localhost")}${path}`;

before(async () => {
  const hash = createHash("sha256").update(SETUP_CODE).digest("hex");
  w = await startWorker({
    seedSql: (url) => `INSERT INTO settings (key, value_json, updated_at) VALUES
        ('site_id', ${sq(JSON.stringify("site-sync-1"))}, ${T}),
        ('origin', ${sq(JSON.stringify(url.replace("127.0.0.1", "localhost")))}, ${T}),
        ('setup_code_hash', ${sq(JSON.stringify(hash))}, ${T}),
        ('setup_code_expires_at', '${T + 86_400_000}', ${T});`,
  });
  browser = await chromium.launch({ channel: "chrome", headless: true });
});
after(async () => {
  await browser?.close();
  await w?.stop();
});

/** The API as another device: the same session, from Node. */
async function api(method, path, body) {
  const headers = { origin: at(), cookie: `__Host-plumblog_session=${session}`, "content-type": "application/json" };
  const res = await fetch(w.url + path, { method, headers, body: JSON.stringify(body) });
  return { status: res.status, json: await res.json() };
}

test("a new post is saved to the server, then updated, and a concurrent change asks", async () => {
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, locale: "en-US" });
  await context.addInitScript(() => {
    document.addEventListener("securitypolicyviolation", (e) => console.error(`CSPVIOLATION ${e.violatedDirective} ${e.blockedURI}`));
  });
  const page = await context.newPage();
  const problems = [];
  page.on("response", (r) => r.status() === 404 && problems.push(`404 ${r.url()}`));
  page.on("pageerror", (e) => problems.push(`page error: ${e}`));
  page.on("console", (m) => {
    const text = m.text();
    // The stale save below answers 409 on purpose; Chrome logs it by itself.
    if (m.type() === "error" && !/^Failed to load resource: the server responded with a status of 409/.test(text)) problems.push(`console: ${text}`);
    if (/Content Security Policy|Refused to/.test(text)) problems.push(`csp: ${text}`);
  });
  const status = page.locator(".ed-actions .saved");
  const savedAfter = async (edit) => {
    await edit();
    await status.filter({ hasText: tx("admin.editor.saved_here") }).waitFor();
    await status.filter({ hasText: new RegExp(`^${tx("admin.editor.saved")}$`) }).waitFor({ timeout: 15_000 });
  };
  try {
    // Signed in through setup (as auth-ui.test.mjs does); the API calls below share its session.
    await page.goto(at(`/admin/setup?code=${SETUP_CODE}`));
    await page.getByLabel(tx("admin.auth.login")).fill(OWNER.login);
    await page.getByLabel(tx("admin.auth.slug")).fill(OWNER.slug);
    await page.getByLabel(tx("admin.auth.display_name")).fill(OWNER.name);
    await page.getByLabel(tx("admin.auth.password"), { exact: true }).fill(OWNER.password);
    await page.getByLabel(tx("admin.auth.password_repeat")).fill(OWNER.password);
    await page.getByRole("button", { name: tx("admin.auth.create") }).click();
    await page.waitForURL(/\/admin\/$/);
    session = (await context.cookies()).find((c) => c.name === "__Host-plumblog_session")?.value;
    assert.ok(session, "signed in");

    await page.goto(at("/admin/posts/new"));
    await page.getByRole("toolbar", { name: tx("admin.editor.toolbar") }).waitFor();
    assert.match(page.url(), /\/admin\/posts\/new\?draft=[a-z0-9]{16}$/);

    // 1. Title and body: one post in D1 with that text, and the URL names it.
    await savedAfter(async () => {
      await page.getByLabel(tx("admin.editor.title_label")).fill("M\u1ed9t ng\u00e0y \u1edf \u0110\u00e0 L\u1ea1t");
      await page.locator(".cm-content").click();
      await page.keyboard.type("First line.");
    });
    await page.waitForURL(/\/admin\/posts\/\d+$/);
    const rows = w.query("SELECT id, slug, title, markdown, status, version FROM posts");
    assert.equal(rows.length, 1);
    const [p] = rows;
    assert.deepEqual(
      { slug: p.slug, title: p.title, markdown: p.markdown, status: p.status, version: p.version },
      { slug: "mot-ngay-o-da-lat", title: "M\u1ed9t ng\u00e0y \u1edf \u0110\u00e0 L\u1ea1t", markdown: "First line.", status: "draft", version: 1 },
    );
    assert.equal(new URL(page.url()).pathname, `/admin/posts/${p.id}`);

    // 2. Another edit: an update, the version one up, still one post.
    await savedAfter(() => page.keyboard.type(" Second."));
    const [q] = w.query(`SELECT markdown, version, (SELECT count(*) FROM posts) AS n FROM posts WHERE id = ${p.id}`);
    assert.deepEqual(q, { markdown: "First line. Second.", version: 2, n: 1 });

    // 3. Another device saves first; the next edit here is stale and the writer is asked.
    const other = await api("PATCH", `/api/v1/posts/${p.id}`, { expected_version: 2, markdown: "From the other device." });
    assert.equal(other.status, 200);
    await page.keyboard.type(" Third.");
    const banner = page.getByRole("alert").filter({ hasText: tx("admin.editor.conflict.server_title") });
    await banner.waitFor({ timeout: 15_000 });
    const [r] = w.query(`SELECT markdown, version FROM posts WHERE id = ${p.id}`);
    assert.deepEqual(r, { markdown: "From the other device.", version: 3 }, "nothing overwritten before the writer picks");

    // 4. "Keep my draft": this text is saved over version 3.
    await savedAfter(() => banner.getByRole("button", { name: tx("admin.editor.conflict.keep_mine") }).click());
    const [s] = w.query(`SELECT markdown, version FROM posts WHERE id = ${p.id}`);
    assert.deepEqual(s, { markdown: "First line. Second. Third.", version: 4 });

    // 5. The network drops right after typing, before the local save: the editor is redrawn
    // (main.ts), and the keys typed before that are neither lost nor saved into the old key.
    await page.locator(".cm-content").click();
    await page.keyboard.press("End");
    await page.keyboard.type(" Fourth.");
    const before = problems.length;
    await context.setOffline(true);
    await page.locator(".empty, .ed").first().waitFor();
    await context.setOffline(false);
    await page.locator(".cm-content", { hasText: "Fourth." }).waitFor({ timeout: 15_000 });
    await status.filter({ hasText: new RegExp(`^${tx("admin.editor.saved")}$`) }).waitFor({ timeout: 15_000 });
    const [u] = w.query(`SELECT markdown, version, (SELECT count(*) FROM posts) AS n FROM posts WHERE id = ${p.id}`);
    assert.deepEqual(u, { markdown: "First line. Second. Third. Fourth.", version: 5, n: 1 });
    // Offline, fetching the post fails on purpose and says so in the log.
    problems.splice(before, problems.length - before, ...problems.slice(before).filter((x) => !/ERR_INTERNET_DISCONNECTED|no response/.test(x)));

    // 6. The editor is redrawn while a slow create is out: the new mount waits for it and
    // opens the post it made (index.ts, sync.ts `settled`); one post holds every typed key.
    await page.route("**/api/v1/posts", async (route) => {
      if (route.request().method() === "POST") await new Promise((ok) => setTimeout(ok, 4000));
      await route.continue();
    });
    await page.goto(at("/admin/posts/new"));
    await page.getByRole("toolbar", { name: tx("admin.editor.toolbar") }).waitFor();
    const createOut = page.waitForRequest((r) => r.method() === "POST" && r.url().endsWith("/api/v1/posts"), { timeout: 15_000 });
    await page.getByLabel(tx("admin.editor.title_label")).fill("Slow create");
    await page.locator(".cm-content").click();
    await page.keyboard.type("Alpha.");
    await createOut;
    await page.keyboard.type(" Beta.");
    // The redrawn editor opens the saved post: it reads it from the API (a new post's editor never does).
    const reopened = page.waitForRequest((r) => r.method() === "GET" && /\/api\/v1\/posts\/\d+$/.test(r.url()), { timeout: 15_000 });
    const before6 = problems.length;
    await context.setOffline(true);
    await context.setOffline(false);
    await reopened;
    await page.waitForURL(/\/admin\/posts\/\d+$/);
    await status.filter({ hasText: new RegExp(`^${tx("admin.editor.saved")}$`) }).waitFor({ timeout: 15_000 });
    const slow = w.query("SELECT id, markdown FROM posts WHERE title = 'Slow create'");
    assert.equal(slow.length, 1, "exactly one post");
    assert.equal(slow[0].markdown, "Alpha. Beta.");
    assert.equal(new URL(page.url()).pathname, `/admin/posts/${slow[0].id}`);
    problems.splice(before6, problems.length - before6, ...problems.slice(before6).filter((x) => !/ERR_INTERNET_DISCONNECTED|no response/.test(x)));

    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});
