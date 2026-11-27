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
    session = (await context.cookies()).find((c) => c.name === "__Host-plumblog_session")?.value;
    assert.ok(session, "signed in");

    // 1. A new post, published at once: one create, then the status change.
    await write("Publish me", "First version.");
    await button(tx("admin.editor.pub.publish")).click();
    assert.match(await page.locator(".pubpop").innerText(), /\/en\/publish-me/);
    await page.locator(".pubpop .btn.primary", { hasText: tx("admin.editor.pub.publish_now") }).click();
    await toast(tx("admin.editor.pub.published_toast"));
    // Read now: the toast leaves after a few seconds, and D1 queries are slow.
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
    await button(tx("admin.editor.pub.update")).click();
    await confirm().click();
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
