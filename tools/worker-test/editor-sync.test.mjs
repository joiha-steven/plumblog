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
/** The signed-in browser's cookies, for the later tests' own contexts. */
let cookies = [];
const DAY = 86_400_000;

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
    cookies = await context.cookies();
    session = cookies.find((c) => c.name === "__Host-plumblog_session")?.value;
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

/** A signed-in page on `path` with the editor drawn; `sent` lists every posts API request it makes. */
async function editorPage(path, context) {
  if (!context) {
    context = await browser.newContext({ viewport: { width: 1440, height: 900 }, locale: "en-US" });
    await context.addCookies(cookies);
  }
  const page = await context.newPage();
  const sent = [];
  const errors = [];
  page.on("request", (r) => /\/api\/v1\/posts/.test(r.url()) && sent.push(`${r.method()} ${new URL(r.url()).pathname}`));
  page.on("pageerror", (e) => errors.push(`page error: ${e}`));
  await page.goto(at(path));
  await page.getByRole("toolbar", { name: tx("admin.editor.toolbar") }).waitFor();
  return { context, page, sent, errors };
}

/** Every draft this browser keeps (drafts.ts), by key. */
const stored = (page) =>
  page.evaluate(
    () =>
      new Promise((ok, no) => {
        const r = indexedDB.open("plumblog-admin");
        r.onerror = () => no(r.error);
        r.onsuccess = () => {
          const db = r.result;
          const t = db.transaction("drafts");
          const keys = t.objectStore("drafts").getAllKeys();
          const vals = t.objectStore("drafts").getAll();
          t.oncomplete = () => {
            db.close();
            ok(Object.fromEntries(keys.result.map((k, i) => [k, vals.result[i]])));
          };
        };
      }),
  );

/** A draft post made through the API, as another device would. */
async function newDraft(slug, markdown) {
  const made = await api("POST", "/api/v1/posts", { lang: "en", slug, title: slug, markdown });
  assert.equal(made.status, 201, JSON.stringify(made.json));
  return made.json;
}

const uid = () => w.query("SELECT id FROM users WHERE login = 'Wren'")[0].id;
const typeAtEnd = async (page, text) => {
  await page.locator(".cm-content").click();
  await page.keyboard.press("ControlOrMeta+End");
  await page.keyboard.type(text);
};

test("a draft published or scheduled elsewhere is never changed by autosave, even after Keep my draft", async () => {
  for (const status of ["published", "scheduled"]) {
    const p = await newDraft(`live-${status}`, "Original.");
    const { context, page, sent, errors } = await editorPage(`/admin/posts/${p.id}`);
    try {
      const change = { expected_version: p.version, status, markdown: `The ${status} text.` };
      if (status === "scheduled") change.published_at = Date.now() + DAY;
      const live = await api("PATCH", `/api/v1/posts/${p.id}`, change);
      assert.equal(live.status, 200, JSON.stringify(live.json));

      await typeAtEnd(page, " Mine.");
      const banner = page.getByRole("alert").filter({ hasText: tx("admin.editor.conflict.server_title") });
      await banner.waitFor({ timeout: 15_000 });
      const before = sent.length;
      await banner.getByRole("button", { name: tx("admin.editor.conflict.keep_mine") }).click();
      await typeAtEnd(page, " More.");
      // Autosave runs 3 s after the last local save: wait well past it.
      await new Promise((ok) => setTimeout(ok, 5000));

      assert.deepEqual(sent.slice(before).filter((r) => r.startsWith("PATCH")), [], `${status}: nothing sent after Keep my draft`);
      const [row] = w.query(`SELECT status, markdown FROM posts WHERE id = ${p.id}`);
      assert.deepEqual(row, { status, markdown: `The ${status} text.` }, "the live post is unchanged");
      assert.equal(await page.locator(".crumb .st").textContent(), tx(`admin.status.${status}`), "the status chip shows it");
      assert.equal(await page.locator(".ed-actions .saved").textContent(), tx("admin.editor.saved_here"));
      const kept = (await stored(page))[`${uid()}:post-${p.id}`];
      assert.equal(kept?.body, "Original. Mine. More.", "the writer's text stays on this device");
      assert.deepEqual(errors, []);
    } finally {
      await context.close();
    }
  }
});

test("'Use the saved version' after a 409 shows the server text and sends nothing", async () => {
  const p = await newDraft("use-saved", "Base.");
  const { context, page, sent } = await editorPage(`/admin/posts/${p.id}`);
  try {
    assert.equal((await api("PATCH", `/api/v1/posts/${p.id}`, { expected_version: p.version, markdown: "Theirs." })).status, 200);
    await typeAtEnd(page, " Mine.");
    const banner = page.getByRole("alert").filter({ hasText: tx("admin.editor.conflict.server_title") });
    await banner.waitFor({ timeout: 15_000 });
    const before = sent.length;
    await banner.getByRole("button", { name: tx("admin.editor.conflict.use_saved") }).click();
    await page.locator(".cm-content", { hasText: /^Theirs\.$/ }).waitFor();
    await page.locator(".ed-actions .saved", { hasText: new RegExp(`^${tx("admin.editor.saved")}$`) }).waitFor({ timeout: 15_000 });
    assert.deepEqual(sent.slice(before).filter((r) => r.startsWith("PATCH")), []);
    assert.deepEqual(w.query(`SELECT markdown, version FROM posts WHERE id = ${p.id}`)[0], { markdown: "Theirs.", version: p.version + 1 });
    assert.equal((await stored(page))[`${uid()}:post-${p.id}`], undefined, "the local draft goes once the server holds it");
  } finally {
    await context.close();
  }
});

/** Two or more pages on one draft, with server saves cut off so the drafts meet on this device. */
async function localOnly() {
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, locale: "en-US" });
  await context.addCookies(cookies);
  await context.route("**/api/v1/posts/*", (r) => (r.request().method() === "PATCH" ? r.abort() : r.continue()));
  return context;
}
const sidesOf = (all, key) => Object.keys(all).filter((k) => k.startsWith(`${key}~`));
const tabBanner = (page) => page.getByRole("alert").filter({ hasText: tx("admin.editor.conflict.tab_title") });
/** Pages A and B on one draft; B is paused by A's save and keeps typing under the banner. */
async function pausedPair(slug) {
  const p = await newDraft(slug, "Start.");
  const context = await localOnly();
  const key = `${uid()}:post-${p.id}`;
  const a = await editorPage(`/admin/posts/${p.id}`, context);
  const b = await editorPage(`/admin/posts/${p.id}`, context);
  await typeAtEnd(a.page, " A.");
  await a.page.locator(".ed-actions .saved", { hasText: tx("admin.editor.saved_here") }).waitFor();
  await typeAtEnd(b.page, " B.");
  await tabBanner(b.page).waitFor();
  await typeAtEnd(b.page, " Under the banner.");
  // Past the 700 ms local save.
  await new Promise((ok) => setTimeout(ok, 1500));
  return { p, context, key, a, b };
}

test("typing in a tab paused by another tab's save is kept on this device and offered back", async () => {
  const { p, context, key, b } = await pausedPair("two-tabs");
  try {
    const all = await stored(b.page);
    assert.equal(all[key].body, "Start. A.", "the other tab's draft is not overwritten");
    assert.equal(sidesOf(all, key).length, 1, "one side copy");
    assert.equal(all[sidesOf(all, key)[0]].body, "Start. B. Under the banner.");

    // The paused tab closes; opening the post again offers its text, never a silent pick.
    await b.page.close();
    const c = await editorPage(`/admin/posts/${p.id}`, context);
    await tabBanner(c.page).waitFor();
    assert.equal(await c.page.locator(".cm-content").textContent(), "Start. A.", "opens with the draft; the copy is only offered");
    await tabBanner(c.page).getByRole("button", { name: tx("admin.editor.conflict.load_other") }).click();
    await c.page.locator(".cm-content", { hasText: /^Start\. B\. Under the banner\.$/ }).waitFor();
    await new Promise((ok) => setTimeout(ok, 500));
    const after = await stored(c.page);
    assert.equal(after[key].body, "Start. B. Under the banner.", "the pick is in the draft");
    assert.deepEqual(sidesOf(after, key), [], "the side copy is gone after the pick");
  } finally {
    await context.close();
  }
});

test("a page that opens while another is paused never writes the paused page's copy", async () => {
  const { p, context, key, b } = await pausedPair("three-tabs");
  try {
    const [bKey] = sidesOf(await stored(b.page), key);
    // C opens: B's copy is offered; C types, then B types more and closes at once.
    const c = await editorPage(`/admin/posts/${p.id}`, context);
    await tabBanner(c.page).waitFor();
    await typeAtEnd(c.page, " C.");
    await new Promise((ok) => setTimeout(ok, 1500));
    await typeAtEnd(b.page, " B last.");
    await b.page.close({ runBeforeUnload: true });
    await typeAtEnd(c.page, " C again.");
    await new Promise((ok) => setTimeout(ok, 1500));

    const all = await stored(c.page);
    assert.equal(all[bKey]?.body, "Start. B. Under the banner. B last.", "B's copy holds B's last keys");
    const cKeys = sidesOf(all, key).filter((k) => k !== bKey);
    assert.equal(cKeys.length, 1, "C writes a copy of its own");
    assert.equal(all[cKeys[0]].body, "Start. A. C. C again.");
    assert.equal(all[key].body, "Start. A.", "the draft waits for the pick");

    // "Load that version" reads B's copy as it is now, then both copies go.
    await tabBanner(c.page).getByRole("button", { name: tx("admin.editor.conflict.load_other") }).click();
    await c.page.locator(".cm-content", { hasText: /^Start\. B\. Under the banner\. B last\.$/ }).waitFor();
    await new Promise((ok) => setTimeout(ok, 500));
    const after = await stored(c.page);
    assert.equal(after[key].body, "Start. B. Under the banner. B last.");
    assert.deepEqual(sidesOf(after, key), []);
  } finally {
    await context.close();
  }
});

test("a copy another page left is offered, never written over a newer draft; one equal to the draft just goes", async () => {
  const p = await newDraft("left-copy", "Start.");
  const context = await localOnly();
  const key = `${uid()}:post-${p.id}`;
  try {
    // The state a closed tab can leave: its copy (old text) and a draft written since, which
    // went back to rev 0 when the server held it, then up again (the copy's rev means nothing).
    const first = await editorPage(`/admin/posts/${p.id}`, context);
    await first.page.evaluate(
      ([key]) =>
        new Promise((ok) => {
          const r = indexedDB.open("plumblog-admin");
          r.onsuccess = () => {
            const t = r.result.transaction("drafts", "readwrite");
            const s = t.objectStore("drafts");
            const d = (body, rev, savedAt) => ({ title: "left-copy", body, base: null, rev, savedAt });
            // Based on version 0: the server copy (version 1) moved on, so that choice waits too.
            s.put({ ...d("Newer text.", 5, 3), version: 0 }, key);
            s.put({ ...d("Old text.", 1, 1), after: 5 }, `${key}~aaaaaaaaaaaaaaaa`);
            s.put(d("Newer text.", 2, 2), `${key}~bbbbbbbbbbbbbbbb`);
            t.oncomplete = () => (r.result.close(), ok());
          };
        }),
      [key],
    );
    await first.page.close();
    const c = await editorPage(`/admin/posts/${p.id}`, context);
    await tabBanner(c.page).waitFor();
    assert.equal(await c.page.locator(".cm-content").textContent(), "Newer text.");
    const all = await stored(c.page);
    assert.equal(all[key].body, "Newer text.", "the newer draft is not overwritten");
    assert.deepEqual(sidesOf(all, key), [`${key}~aaaaaaaaaaaaaaaa`], "the copy equal to the draft is gone, the other kept");
    await tabBanner(c.page).getByRole("button", { name: tx("admin.editor.conflict.keep_this") }).click();
    await new Promise((ok) => setTimeout(ok, 500));
    const after = await stored(c.page);
    assert.equal(after[key].body, "Newer text.");
    assert.deepEqual(sidesOf(after, key), [], "kept this: the offered copy goes");
    await c.page.getByRole("alert").filter({ hasText: tx("admin.editor.conflict.server_title") }).waitFor();
  } finally {
    await context.close();
  }
});

test("keys typed just before the tab is hidden or closed are kept", async () => {
  const p = await newDraft("hide-close", "Start.");
  const key = `${uid()}:post-${p.id}`;
  const context = await localOnly();
  const { page } = await editorPage(`/admin/posts/${p.id}`, context);
  try {
    // Hidden (switching apps on a phone): saved at once, not 700 ms later; no copy left.
    await typeAtEnd(page, " Hidden.");
    await page.evaluate(() => {
      Object.defineProperty(document, "visibilityState", { value: "hidden", configurable: true });
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await new Promise((ok) => setTimeout(ok, 300));
    const hidden = await stored(page);
    assert.equal(hidden[key]?.body, "Start. Hidden.");
    assert.deepEqual(sidesOf(hidden, key), [], "the copy goes once the text is in the draft");
    await page.evaluate(() => {
      Object.defineProperty(document, "visibilityState", { value: "visible", configurable: true });
    });

    // Closed straight after typing: reopened, the last keys are offered, never lost.
    await typeAtEnd(page, " Closed.");
    await page.close({ runBeforeUnload: true });
    const again = await editorPage(`/admin/posts/${p.id}`, context);
    const all = await stored(again.page);
    if (all[key]?.body !== "Start. Hidden. Closed.") {
      await tabBanner(again.page).getByRole("button", { name: tx("admin.editor.conflict.load_other") }).click();
      await again.page.locator(".cm-content", { hasText: /^Start\. Hidden\. Closed\.$/ }).waitFor();
      await new Promise((ok) => setTimeout(ok, 500));
    }
    const after = await stored(again.page);
    assert.equal(after[key]?.body, "Start. Hidden. Closed.");
    assert.deepEqual(sidesOf(after, key), []);
  } finally {
    await context.close();
  }
});

// Last in this file: it ends the shared session.
test("an expired session saves the last keys on this device before going to sign in", async () => {
  const p = await newDraft("expired", "Start.");
  const key = `${uid()}:post-${p.id}`;
  const { context, page } = await editorPage(`/admin/posts/${p.id}`);
  try {
    let release;
    const gate = new Promise((ok) => (release = ok));
    await page.route("**/api/v1/posts/*", async (r) => {
      if (r.request().method() === "PATCH") await gate;
      await r.continue();
    });
    const patch = page.waitForRequest((r) => r.method() === "PATCH", { timeout: 15_000 });
    await typeAtEnd(page, " Before.");
    await patch;
    w.query("DELETE FROM sessions");
    // The save is out; the writer keeps typing, then the 401 lands within the local debounce.
    await page.keyboard.type(" After.");
    release();
    await page.waitForURL(/\/admin\/signin\?next=/, { timeout: 15_000 });
    assert.equal((await stored(page))[key]?.body, "Start. Before. After.");
  } finally {
    await context.close();
  }
});
