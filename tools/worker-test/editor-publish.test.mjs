// Publishing from the editor (web/admin/src/editor/publish.ts) in the system Chrome against the
// real Worker and a real local D1: publish a new post, edit and Update, schedule, unpublish,
// a taken slug, and trash. The public page checks (the post answering at /<lang>/<slug>, and
// not after unpublishing) arrive with #76: the reader does not read D1 yet, so D1 is the record.
// UI text is read from locales/en.toml, never hard-coded here.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { after, before, test } from "node:test";
import { chromium } from "playwright-core";
import { startWorker } from "./harness.mjs";

const sq = (s) => `'${String(s).replaceAll("'", "''")}'`;
const T = Date.now();
const SETUP_CODE = "setup-code-publish-0123456789";
const OWNER = { login: "Wren", slug: "wren", name: "Wren Owner", password: "correct horse battery" };
let session;
/** The signed-in browser's cookies, for the later tests' own contexts. */
let cookies = [];

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
const at = (path = "") => `${w.url.replace("127.0.0.1", "localhost")}${path}`;

before(async () => {
  const hash = createHash("sha256").update(SETUP_CODE).digest("hex");
  w = await startWorker({
    seedSql: (url) => `INSERT INTO settings (key, value_json, updated_at) VALUES
        ('site_id', ${sq(JSON.stringify("site-publish-1"))}, ${T}),
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

async function api(method, path, body) {
  const headers = { origin: at(), cookie: `__Host-plumblog_session=${session}`, "content-type": "application/json" };
  const res = await fetch(w.url + path, { method, headers, body: JSON.stringify(body) });
  return { status: res.status, json: await res.json() };
}

test("publish, update, schedule, unpublish and trash from the editor", async () => {
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, locale: "en-US", timezoneId: "UTC" });
  await context.addInitScript(() => {
    document.addEventListener("securitypolicyviolation", (e) => console.error(`CSPVIOLATION ${e.violatedDirective} ${e.blockedURI}`));
  });
  const page = await context.newPage();
  const problems = [];
  page.on("pageerror", (e) => problems.push(`page error: ${e}`));
  page.on("console", (m) => {
    const text = m.text();
    // The taken slug below answers 409 on purpose; Chrome logs it by itself.
    if (m.type() === "error" && !/^Failed to load resource: the server responded with a status of 409/.test(text)) problems.push(`console: ${text}`);
    if (/Content Security Policy|Refused to/.test(text)) problems.push(`csp: ${text}`);
  });
  const button = (name) => page.getByRole("button", { name, exact: true });
  const confirm = () => page.locator(".pubpop .btn.primary");
  const toast = (text) => page.locator(".toast", { hasText: text }).waitFor();
  const write = async (title, body) => {
    await page.goto(at("/admin/posts/new"));
    await page.getByRole("toolbar", { name: tx("admin.editor.toolbar") }).waitFor();
    await page.getByLabel(tx("admin.editor.title_label")).fill(title);
    await page.locator(".cm-content").click();
    await page.keyboard.type(body);
  };
  const row = (id) => w.query(`SELECT slug, title, markdown, status, published_at, version FROM posts WHERE id = ${id}`)[0];
  try {
    await page.goto(at(`/admin/setup?code=${SETUP_CODE}`));
    await page.getByLabel(tx("admin.auth.login")).fill(OWNER.login);
    await page.getByLabel(tx("admin.auth.slug")).fill(OWNER.slug);
    await page.getByLabel(tx("admin.auth.display_name")).fill(OWNER.name);
    await page.getByLabel(tx("admin.auth.password"), { exact: true }).fill(OWNER.password);
    await page.getByLabel(tx("admin.auth.password_repeat")).fill(OWNER.password);
    await page.getByRole("button", { name: tx("admin.auth.create") }).click();
    await page.waitForURL(/\/admin\/$/);
    cookies = await context.cookies();
    session = cookies.find((c) => c.name === "__Host-plumblog_session")?.value;
    assert.ok(session, "signed in");

    // 1. A new post, published at once: one create, then the status change.
    await write("Publish me", "First version.");
    await button(tx("admin.editor.pub.publish")).click();
    assert.match(await page.locator(".pubpop").innerText(), /\/en\/publish-me/);
    await page.locator(".pubpop .btn.primary", { hasText: tx("admin.editor.pub.publish_now") }).click();
    await toast(tx("admin.editor.pub.published_toast"));
    // Read now: the toast leaves after a few seconds.
    const viewHref = await page.locator(".toast a").getAttribute("href");
    await button(tx("admin.editor.pub.update")).waitFor();
    const all = w.query("SELECT id FROM posts");
    assert.equal(all.length, 1, "one post");
    const id = all[0].id;
    const live = row(id);
    assert.equal(live.status, "published");
    assert.equal(live.slug, "publish-me");
    assert.equal(live.markdown, "First version.");
    assert.ok(live.published_at > 0, "published now");
    assert.equal(viewHref, "/en/publish-me");
    assert.equal(await page.locator(".crumb .st").innerText(), tx("admin.status.published"));

    // 2. Editing a live post stays on this device until Update.
    await page.locator(".cm-content").click();
    await page.keyboard.press("Control+End");
    await page.keyboard.type(" Second version.");
    await page.locator(".ed-actions .saved, .phone-status").first().waitFor();
    await page.getByText(tx("admin.editor.pub.unpublished_changes")).first().waitFor();
    await page.waitForTimeout(3600); // longer than the autosave delay of a draft
    assert.equal(row(id).markdown, "First version.", "not sent before Update");
    // The publish toast above may still be up, so it does not prove this save ended: wait for its answer.
    const saved1 = page.waitForResponse((r) => /\/api\/v1\/posts\/\d+$/.test(r.url()) && r.request().method() === "PATCH");
    await button(tx("admin.editor.pub.update")).click();
    await confirm().click();
    assert.equal((await saved1).status(), 200);
    await toast(tx("admin.editor.pub.published_toast"));
    const updated = row(id);
    assert.equal(updated.markdown, "First version. Second version.");
    assert.equal(updated.status, "published");
    assert.equal(updated.published_at, live.published_at, "an update keeps the publish time");
    assert.ok(updated.version > live.version);

    // 3. Unpublish from the settings panel: back to a draft with no time.
    await button(tx("admin.editor.pub.settings")).click();
    await page.getByRole("button", { name: tx("admin.editor.pub.unpublish") }).click();
    await page.locator(".confirm .btn.danger").click();
    await toast(tx("admin.editor.pub.unpublished"));
    const down = row(id);
    assert.equal(down.status, "draft");
    assert.equal(down.published_at, null);
    await button(tx("admin.editor.pub.publish")).waitFor();

    // 4. A second post, scheduled for tomorrow.
    await write("Later post", "Comes tomorrow.");
    await button(tx("admin.editor.pub.settings")).click();
    await page.getByLabel(tx("admin.editor.pub.when_later")).check();
    const tomorrow = new Date(Date.now() + 86_400_000 - (Date.now() % 60_000));
    const value = tomorrow.toISOString().slice(0, 16);
    await page.getByLabel(tx("admin.editor.pub.when_at")).fill(value);
    await button(tx("admin.editor.pub.schedule")).click();
    await confirm().click();
    await page.locator(".toast", { hasText: "Scheduled for" }).waitFor();
    const later = w.query("SELECT id, status, published_at FROM posts WHERE title = 'Later post'")[0];
    assert.equal(later.status, "scheduled");
    assert.equal(later.published_at, Date.parse(`${value}:00Z`));

    // 5. A taken slug: the message, the slug field open, nothing published.
    await write("Third post", "Needs another link.");
    await button(tx("admin.editor.pub.settings")).click();
    await page.locator("[data-slug]").fill("publish-me");
    await page.locator("[data-slug]").blur();
    await button(tx("admin.editor.pub.publish")).click();
    await confirm().click();
    await page.locator(".panel .err", { hasText: tx("admin.editor.pub.slug_conflict") }).waitFor();
    assert.equal(w.query("SELECT status FROM posts WHERE title = 'Third post'")[0].status, "draft");
    await page.locator("[data-slug]").fill("third-post");
    await page.locator("[data-slug]").blur();
    await button(tx("admin.editor.pub.publish")).click();
    await confirm().click();
    await toast(tx("admin.editor.pub.published_toast"));
    assert.equal(w.query("SELECT slug, status FROM posts WHERE title = 'Third post'")[0].slug, "third-post");

    // 6. Trash from the panel: to the posts list, the post in the trash.
    await page.getByRole("button", { name: tx("admin.editor.pub.trash") }).click();
    await page.locator(".confirm .btn.danger").click();
    await page.waitForURL(/\/admin\/$/);
    await toast(tx("admin.editor.pub.trashed"));
    assert.equal(w.query("SELECT status FROM posts WHERE title = 'Third post'")[0].status, "trashed");
    assert.equal((await api("GET", "/api/v1/posts/" + id)).json.status, "draft");

    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

// Autosave and publish together: a draft autosaves; once published nothing autosaves, edits go
// only by Update; a change from elsewhere is a choice, and Keep mine still never autosaves.
test("a draft autosaves; published, only Update sends; after a 409 and Keep mine, Update sends my text", async () => {
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, locale: "en-US", timezoneId: "UTC" });
  await context.addCookies(cookies);
  const page = await context.newPage();
  const problems = [];
  const sent = [];
  page.on("pageerror", (e) => problems.push(`page error: ${e}`));
  page.on("console", (m) => {
    // The stale Update below answers 409 on purpose; Chrome logs it by itself.
    if (m.type() === "error" && !/status of 409/.test(m.text())) problems.push(`console: ${m.text()}`);
  });
  page.on("request", (r) => /\/api\/v1\/posts/.test(r.url()) && r.method() !== "GET" && sent.push({ method: r.method(), body: r.postDataJSON() }));
  const button = (name) => page.getByRole("button", { name, exact: true });
  const confirm = () => page.locator(".pubpop .btn.primary");
  const toast = (text) => page.locator(".toast", { hasText: text }).waitFor();
  // Whole text: "Saved" must not match "Saved on this device".
  const line = (key) => page.locator(".ed-actions .saved", { hasText: new RegExp(`^${tx(key).replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}$`) }).waitFor();
  const patches = () => sent.filter((r) => r.method === "PATCH");
  const typeAtEnd = async (text) => {
    await page.locator(".cm-content").click();
    await page.keyboard.press("Control+End");
    await page.keyboard.type(text);
  };
  try {
    // 1. A draft autosaves: one create.
    await page.goto(at("/admin/posts/new"));
    await page.getByRole("toolbar", { name: tx("admin.editor.toolbar") }).waitFor();
    await page.getByLabel(tx("admin.editor.title_label")).fill("Round trip");
    await page.locator(".cm-content").click();
    await page.keyboard.type("Draft text.");
    await page.waitForURL(/\/admin\/posts\/\d+$/, { timeout: 10_000 });
    await line("admin.editor.saved");
    const id = Number(new URL(page.url()).pathname.split("/").pop());
    const row = () => w.query(`SELECT markdown, status, version FROM posts WHERE id = ${id}`)[0];
    assert.equal(row().markdown, "Draft text.");
    assert.deepEqual(sent.map((r) => r.method), ["POST"]);
    // A post created by autosave shows Trash in the panel (review item 6).
    await button(tx("admin.editor.pub.settings")).click();
    await page.locator('[data-act="trash"]').waitFor();
    await button(tx("admin.editor.pub.settings")).click();

    // 2. Publish it.
    await button(tx("admin.editor.pub.publish")).click();
    await confirm().click();
    await toast(tx("admin.editor.pub.published_toast"));
    assert.equal(row().status, "published");
    assert.equal(patches().length, 1);

    // 3. Edit it: nothing is sent, and the status line says it is not live.
    await typeAtEnd(" Edited.");
    await line("admin.editor.pub.unpublished_changes");
    await page.waitForTimeout(3600); // past a draft's autosave delay
    assert.equal(patches().length, 1, "a live post never autosaves");

    // 4. Update: one PATCH with the text and the status.
    // The publish toast of step 2 may still be up, so it does not prove this save ended: wait for its answer.
    const saved = page.waitForResponse((r) => /\/api\/v1\/posts\/\d+$/.test(r.url()) && r.request().method() === "PATCH");
    await button(tx("admin.editor.pub.update")).click();
    await confirm().click();
    assert.equal((await saved).status(), 200);
    await toast(tx("admin.editor.pub.published_toast"));
    assert.equal(patches().length, 2);
    assert.equal(patches()[1].body.markdown, "Draft text. Edited.");
    assert.equal(patches()[1].body.status, "published");
    assert.equal(row().markdown, "Draft text. Edited.");
    await line("admin.editor.saved");

    // 5. Another tab changes it; this one edits and presses Update: a 409, then the choice.
    const theirs = await api("PATCH", `/api/v1/posts/${id}`, { expected_version: row().version, markdown: "Theirs." });
    assert.equal(theirs.status, 200);
    await typeAtEnd(" Mine.");
    await button(tx("admin.editor.pub.update")).click();
    await confirm().click();
    const banner = page.getByRole("alert").filter({ hasText: tx("admin.editor.conflict.server_title") });
    await banner.waitFor();
    assert.equal(patches().length, 3, "the stale Update, nothing else");

    // 6. Keep mine: still nothing autosaves; Update sends my text and it stays published.
    await banner.getByRole("button", { name: tx("admin.editor.conflict.keep_mine") }).click();
    await page.waitForTimeout(3600);
    assert.equal(patches().length, 3, "Keep mine does not autosave a live post");
    const kept = page.waitForResponse((r) => /\/api\/v1\/posts\/\d+$/.test(r.url()) && r.request().method() === "PATCH");
    await button(tx("admin.editor.pub.update")).click();
    await confirm().click();
    assert.equal((await kept).status(), 200);
    await toast(tx("admin.editor.pub.published_toast"));
    assert.equal(patches().length, 4);
    const end = row();
    assert.equal(end.markdown, "Draft text. Edited. Mine.");
    assert.equal(end.status, "published");
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

// Publish then Unpublish while the sync chunk is still loading: one sync only, so one autosave.
test("publish and unpublish before the sync code arrives start one autosave, not two", async () => {
  const made = await api("POST", "/api/v1/posts", { lang: "en", slug: "slow-sync", title: "Slow sync", markdown: "Start." });
  assert.equal(made.status, 201, JSON.stringify(made.json));
  const id = made.json.id;
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, locale: "en-US", serviceWorkers: "block" });
  await context.addCookies(cookies);
  let release;
  const held = new Promise((ok) => (release = ok));
  await context.route("**/chunks/sync-*.js", async (r) => {
    await held;
    await r.continue();
  });
  const page = await context.newPage();
  const patches = [];
  page.on("request", (r) => r.method() === "PATCH" && r.url().endsWith(`/api/v1/posts/${id}`) && patches.push(r.postDataJSON()));
  const button = (name) => page.getByRole("button", { name, exact: true });
  const toast = (text) => page.locator(".toast", { hasText: text }).waitFor();
  try {
    await page.goto(at(`/admin/posts/${id}`));
    await page.getByRole("toolbar", { name: tx("admin.editor.toolbar") }).waitFor();
    await button(tx("admin.editor.pub.publish")).click();
    await page.locator(".pubpop .btn.primary").click();
    await toast(tx("admin.editor.pub.published_toast"));
    await button(tx("admin.editor.pub.settings")).click();
    await page.getByRole("button", { name: tx("admin.editor.pub.unpublish") }).click();
    await page.locator(".confirm .btn.danger").click();
    await toast(tx("admin.editor.pub.unpublished"));
    // Typed before the sync code arrives: every sync that starts finds text to send.
    await page.locator(".cm-content").click();
    await page.keyboard.press("Control+End");
    await page.keyboard.type(" Typed.");
    await page.waitForTimeout(1000); // past the 700 ms local save
    const loaded = page.waitForResponse((r) => /\/chunks\/sync-/.test(r.url()));
    release();
    await loaded;
    await page.waitForTimeout(5000); // past the autosave delay
    const autosaves = patches.filter((b) => b.status === undefined);
    assert.equal(autosaves.length, 1, JSON.stringify(patches));
    assert.equal(w.query(`SELECT markdown FROM posts WHERE id = ${id}`)[0].markdown, "Start. Typed.");
    assert.equal(await page.getByRole("alert").filter({ hasText: tx("admin.editor.conflict.server_title") }).count(), 0, "no false conflict");
  } finally {
    await context.close();
  }
});

// PR #91 review, items 1 and 3: a trashed post opens in a trashed state; Restore brings it back.
test("a trashed post opens trashed: nothing autosaves, Publish is off, Restore brings it back and saving resumes", async () => {
  const made = await api("POST", "/api/v1/posts", { lang: "en", slug: "in-the-trash", title: "In the trash", markdown: "Start." });
  assert.equal(made.status, 201, JSON.stringify(made.json));
  const id = made.json.id;
  assert.equal((await api("POST", `/api/v1/posts/${id}/trash`)).status, 200);
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, locale: "en-US" });
  await context.addCookies(cookies);
  const page = await context.newPage();
  const problems = [];
  const sent = [];
  page.on("pageerror", (e) => problems.push(`page error: ${e}`));
  page.on("console", (m) => m.type() === "error" && problems.push(`console: ${m.text()}`));
  page.on("request", (r) => /\/api\/v1\/posts/.test(r.url()) && r.method() !== "GET" && sent.push(`${r.method()} ${new URL(r.url()).pathname}`));
  const button = (name) => page.getByRole("button", { name, exact: true });
  try {
    await page.goto(at(`/admin/posts/${id}`));
    await page.getByRole("toolbar", { name: tx("admin.editor.toolbar") }).waitFor();
    assert.equal(await page.locator(".crumb .st").innerText(), tx("admin.status.trashed"));
    await page.getByText(tx("admin.editor.pub.trashed_banner")).waitFor();
    await page.locator(".cm-content").click();
    await page.keyboard.press("Control+End");
    await page.keyboard.type(" Edited.");
    await page.waitForTimeout(4000); // past a draft's autosave delay
    assert.deepEqual(sent, [], "nothing is sent for a trashed post");
    assert.equal(await button(tx("admin.editor.pub.publish")).isDisabled(), true);
    await button(tx("admin.editor.pub.settings")).click();
    assert.equal(await page.locator('[data-act="trash"], [data-act="unpublish"]').count(), 0, "no Trash for a trashed post");
    await button(tx("admin.editor.pub.settings")).click();
    await button(tx("admin.editor.pub.restore")).click();
    await page.locator(".toast", { hasText: tx("admin.editor.pub.restored") }).waitFor();
    assert.equal(await page.locator(".crumb .st").innerText(), tx("admin.status.draft"));
    // Restored as a draft: the edit made while trashed autosaves now.
    await page.locator(".ed-actions .saved", { hasText: new RegExp(`^${tx("admin.editor.saved")}$`) }).waitFor({ timeout: 10_000 });
    const row = w.query(`SELECT status, markdown FROM posts WHERE id = ${id}`)[0];
    assert.deepEqual(row, { status: "draft", markdown: "Start. Edited." });
    assert.deepEqual(sent, [`POST /api/v1/posts/${id}/restore`, `PATCH /api/v1/posts/${id}`]);
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});
