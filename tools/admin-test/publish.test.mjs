// The publish flow (web/admin/src/editor/publish.ts) through the real modules with a fake Api,
// a Map behind IndexedDB and a stand-in DOM: the button label per status, one update per
// publish with the right fields, a past time refused before sending, unpublish, trash, the
// version and slug conflicts, and a new post published after a single create.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { build } from "esbuild";
import { Window } from "happy-dom";

const win = new Window({ url: "http://localhost/admin/posts/new?draft=abcdefghijkl" });
for (const k of ["document", "location", "history", "Node", "HTMLElement", "addEventListener", "removeEventListener"]) {
  const v = typeof win[k] === "function" && !/^[A-Z]/.test(k) ? win[k].bind(win) : win[k];
  Object.defineProperty(globalThis, k, { value: v, configurable: true, writable: true });
}

Object.defineProperty(globalThis, "window", { value: win, configurable: true, writable: true });

// UI text from locales/en.toml, as "section.key".
const en = {};
{
  let section = "";
  for (const line of readFileSync(new URL("../../locales/en.toml", import.meta.url), "utf8").split("\n")) {
    const s = /^\[(.+)\]$/.exec(line);
    if (s) section = s[1];
    const kv = /^(\w+) = "((?:[^"\\]|\\.)*)"$/.exec(line);
    if (kv) en[`${section}.${kv[1]}`] = kv[2];
  }
}
const tx = (k) => {
  assert.ok(en[k], `locale key ${k}`);
  return en[k];
};

const root = new URL("../../", import.meta.url);
const src = (f) => new URL(`web/admin/src/${f}`, root).pathname;
const out = await build({
  stdin: {
    contents: `export * from ${JSON.stringify(src("editor/publish.ts"))};
      export * from ${JSON.stringify(src("editor/pub.ts"))};
      export { startSync } from ${JSON.stringify(src("editor/sync.ts"))};
      export { startState } from ${JSON.stringify(src("editor/drafts.ts"))};
      export { initI18n } from ${JSON.stringify(src("i18n.ts"))};`,
    resolveDir: new URL("tools/", root).pathname,
    loader: "ts",
  },
  bundle: true,
  write: false,
  format: "esm",
  logLevel: "silent",
  nodePaths: [new URL("tools/node_modules", root).pathname],
});
const m = await import(`data:text/javascript;base64,${Buffer.from(out.outputFiles[0].text).toString("base64")}`);
m.initI18n({ lang: "en", langs: ["en"], site: "", nonce: "" }, { messages: en, fallback: en });

// A Map behind the slice of IndexedDB drafts.ts uses (as in sync.test.mjs).
const rows = new Map();
{
  const request = (fn) => {
    const r = {};
    queueMicrotask(() => {
      r.result = fn();
      r.onsuccess?.();
    });
    return r;
  };
  const store = {
    get: (k) => request(() => rows.get(k)),
    put: (v, k) => request(() => (rows.set(k, structuredClone(v)), k)),
    delete: (k) => request(() => void rows.delete(k)),
  };
  const db = {
    objectStoreNames: { contains: () => true },
    close() {},
    transaction() {
      const tx = { objectStore: () => store };
      setTimeout(() => tx.oncomplete?.(), 0);
      return tx;
    },
  };
  globalThis.indexedDB = { open: () => request(() => db) };
}

const until = async (f, ms = 1500) => {
  const end = Date.now() + ms;
  while (!f()) {
    if (Date.now() > end) assert.fail("timed out");
    await new Promise((r) => setTimeout(r, 5));
  }
};
const quiet = (ms = 60) => new Promise((r) => setTimeout(r, ms));
const post = (id, o = {}) => ({
  id, kind: "post", translation_group: id, lang: "en", slug: "hello", status: "draft", trashed_from: null, published_at: null, author_id: 1,
  title: "Hello", excerpt: "", word_count: 1, reading_minutes: 1, pinned: false, comments_enabled: true, cover_media_id: null, series_id: null,
  series_position: null, created_at: 1, updated_at: 100, version: 3, tags: [], markdown: "Body", html: "", media_ids: [], warnings: [], ...o,
});
const conflict = (field, version) => ({ ok: false, error: { code: "store.conflict", status: 409, field, ...(version ? { version } : {}) } });

/** A fake Api; `update` decides each answer. */
function fakeApi({ update, get, call, create } = {}) {
  const calls = [];
  return {
    calls,
    async createPost(input) {
      calls.push(["create", input]);
      return create ? create(input) : { ok: true, value: post(7, { slug: input.slug, title: input.title, markdown: input.markdown, version: 1 }) };
    },
    async updatePost(id, input) {
      calls.push(["update", id, input]);
      if (update) return update(id, input, calls);
      const { expected_version: v, ...rest } = input;
      const at = rest.status === "published" ? (rest.published_at ?? 5000) : (rest.published_at ?? null);
      return { ok: true, value: post(id, { ...rest, published_at: at, version: v + 1, updated_at: 200 + v }) };
    },
    async post(id) {
      calls.push(["get", id]);
      return get(id);
    },
    async call(method, path) {
      calls.push(["call", method, path]);
      return call ? call() : { ok: true, value: post(7, { status: "trashed" }) };
    },
  };
}

let n = 0;
/** The editor side of the flow around `p` (a server post, or none for a new one). */
function setup(api, p, text = ["Hello", "Body"]) {
  rows.clear();
  const url = p ? `/admin/posts/${p.id}` : `/admin/posts/new?draft=abcdefghij${(++n).toString(36)}`;
  history.replaceState(null, "", url);
  document.body.replaceChildren();
  const key = p ? `1:post-${p.id}` : `1:new-${new URLSearchParams(location.search).get("draft")}`;
  const { st } = m.startState(key, undefined, p);
  const e = { text, chosen: [], applied: 0, relabeled: 0, lived: [], busy: [], online: true, sync: undefined };
  const pub = m.newPub(p);
  const gear = document.createElement("button");
  gear.setAttribute("data-panel-btn", "");
  const rootEl = document.createElement("div");
  rootEl.className = "ed";
  const body = document.createElement("div");
  rootEl.append(gear, body);
  document.body.append(rootEl);
  const anchor = document.createElement("button");
  rootEl.append(anchor);
  e.pub = pub;
  e.st = st;
  e.anchor = anchor;
  e.body = body;
  e.host = {
    api, root: rootEl, body, pub, st, lang: p?.lang ?? "en", ui: {},
    text: () => e.text,
    sync: () => e.sync,
    offline: () => !e.online,
    wide: () => true,
    busy: (on) => e.busy.push(on),
    // As index.ts: a post that is not a draft never autosaves.
    applied: () => {
      e.applied++;
      if (pub.status !== "draft") stopSync();
    },
    relabel: () => e.relabeled++,
    trashed: () => e.trashed === true,
    live: (s) => {
      e.lived.push(s);
      m.fromPost?.(pub, s);
      if (s.status !== "draft") stopSync();
    },
    choose: (s) => e.chosen.push(s),
  };
  const stopSync = () => {
    e.sync?.stop();
    e.sync = undefined;
  };
  e.sync = m.startSync({
    api, user: 1, lang: "en", st, id: p?.id, draft: pub.status === "draft", offline: () => !e.online, alive: () => true,
    text: () => e.text, say: () => undefined, choose: (s) => e.chosen.push(s), live: (s) => e.host.live(s),
    made: (s) => (m.created ?? m.fromPost)?.(pub, s), note: document.createElement("div"),
  }, 5);
  return e;
}
const popup = () => document.querySelector(".pop");
const clickIn = (sel) => document.querySelector(sel).click();
const updates = (api) => api.calls.filter((c) => c[0] === "update");
const toastText = () => document.querySelector(".toast")?.textContent;

test("the button says Publish for a draft, Update for a live post, Schedule for a scheduled one or when later is picked", () => {
  assert.equal(m.label(m.newPub(undefined)), tx("admin.editor.pub.publish"));
  assert.equal(m.label(m.newPub(post(1, { status: "published" }))), tx("admin.editor.pub.update"));
  assert.equal(m.label(m.newPub(post(1, { status: "scheduled", published_at: 9e12 }))), tx("admin.editor.pub.schedule"));
  const later = m.newPub(post(1));
  later.later = true;
  assert.equal(m.label(later), tx("admin.editor.pub.schedule"));
  const back = m.newPub(post(1, { status: "scheduled", published_at: 9e12 }));
  back.later = false;
  assert.equal(m.label(back), tx("admin.editor.pub.publish"), "a scheduled post switched to now publishes");
});

test("slugPlan follows the title for a draft named after it, keeps what the server has otherwise", () => {
  const draft = { ...m.newPub(post(1, { slug: "hello" })) };
  assert.deepEqual(m.slugPlan(draft, "Hello"), { slug: "hello", send: false });
  assert.deepEqual(m.slugPlan(draft, "Hello again"), { slug: "hello-again", send: true });
  assert.deepEqual(m.slugPlan({ ...draft, slug: "hello-2" }, "Hello"), { slug: "hello-2", send: false }, "a numbered variant stays");
  assert.deepEqual(m.slugPlan({ ...draft, typed: "My Link" }, "Hello"), { slug: "my-link", send: true });
  assert.deepEqual(m.slugPlan({ ...draft, typed: "hello" }, "Hello"), { slug: "hello", send: false });
  const live = m.newPub(post(1, { status: "published", slug: "old-name" }));
  assert.deepEqual(m.slugPlan(live, "Totally new title"), { slug: "old-name", send: false }, "a live link never follows the title");
  assert.deepEqual(m.slugPlan(m.newPub(undefined), "Hi there"), { slug: "hi-there", send: false });
  const custom = m.newPub(post(1, { slug: "custom" }));
  assert.deepEqual(m.slugPlan(custom, "Hello"), { slug: "custom", send: false }, "a slug that is not the title's stays");
});

// Review item 1: autosave moves the server title, so the slug must not be compared with it.
test("a draft retitled and autosaved still gets the new title's slug; the address shown is the slug sent", async () => {
  const api = fakeApi();
  const e = setup(api, post(7));
  e.text = ["Hello again", "Body"];
  e.sync.changed();
  await until(() => updates(api).length === 1 && e.st.server[0] === "Hello again");
  m.publishClick(e.host, e.anchor);
  assert.match(popup().textContent, /\/en\/hello-again/);
  // Retitled again while the confirm is open: what was shown is what is sent.
  e.text = ["Third title", "Body"];
  popup().querySelector(".btn.primary").click();
  await until(() => e.applied === 1);
  const sent = updates(api)[1][2];
  assert.equal(sent.slug, "hello-again");
  assert.equal(sent.title, "Third title");
});

// Review item 2.
test("a scheduled post can be rescheduled before the settings panel was ever drawn", async () => {
  const api = fakeApi();
  const at = Math.ceil((Date.now() + 2 * 86_400_000) / 60_000) * 60_000;
  const e = setup(api, post(7, { status: "scheduled", published_at: at }));
  e.text = ["Hello", "Edited"];
  m.publishClick(e.host, e.anchor);
  assert.notEqual(toastText(), tx("admin.editor.pub.time_missing"));
  assert.ok(popup(), "the confirm opens");
  popup().querySelector(".btn.primary").click();
  await until(() => e.applied === 1);
  assert.deepEqual(updates(api)[0][2], { expected_version: 3, title: "Hello", markdown: "Edited", status: "scheduled", published_at: at });
});

// Review item 3: picking now/later only relabels the button; the status line is not redrawn.
test("choosing later in the panel relabels the button and nothing else", () => {
  const e = setup(fakeApi(), post(7));
  m.openPanel(e.host);
  const radios = document.querySelectorAll('input[name="pub-when"]');
  radios[1].dispatchEvent(new win.Event("change"));
  assert.equal(e.pub.later, true);
  assert.equal(e.relabeled, 1);
  assert.equal(e.applied, 0);
});

// Review item 4: no autosave goes out while the publish update is in flight.
test("autosave waits while a publish request is out, so no second update carries the same version", async () => {
  let release;
  const gate = new Promise((r) => (release = r));
  const api = fakeApi({
    update: async (id, input) => {
      if (input.status) await gate;
      const { expected_version: v, ...rest } = input;
      return { ok: true, value: post(id, { ...rest, published_at: rest.status === "published" ? 5000 : null, version: v + 1 }) };
    },
  });
  const e = setup(api, post(7));
  e.text = ["Hello", "Body edited"];
  m.publishClick(e.host, e.anchor);
  popup().querySelector(".btn.primary").click();
  await until(() => updates(api).length === 1);
  e.text = ["Hello", "Typed during publish"];
  e.sync?.changed();
  await quiet(60);
  assert.equal(updates(api).length, 1, "no autosave during the publish request");
  release();
  await until(() => e.applied === 1);
  await quiet(60);
  assert.equal(updates(api).length, 1, "a published post never autosaves after");
});

test("a save answered after the sync stopped is not acted on", async () => {
  let release;
  const gate = new Promise((r) => (release = r));
  const api = fakeApi({ update: async () => (await gate, conflict("expected_version", 9)), get: () => ({ ok: true, value: post(7, { markdown: "Else", version: 9 }) }) });
  const e = setup(api, post(7));
  e.text = ["Hello", "Mine"];
  e.sync.changed();
  await until(() => updates(api).length === 1);
  e.sync.stop();
  const warn = console.warn;
  console.warn = () => undefined;
  try {
    release();
    await quiet(60);
  } finally {
    console.warn = warn;
  }
  assert.deepEqual(api.calls.map((c) => c[0]), ["update"], "no read, no choice after stop");
  assert.equal(e.chosen.length, 0);
});

// Review item 5: another tab published it; Keep mine must not autosave over the live post.
test("a 409 from a post published elsewhere adopts its status; Keep mine then autosaves nothing", async () => {
  const server = post(7, { status: "published", published_at: 1000, markdown: "Theirs", version: 5, updated_at: 500 });
  const api = fakeApi({ update: (id, input) => (input.status ? conflict("expected_version", 5) : assert.fail("autosave sent over a live post")), get: () => ({ ok: true, value: server }) });
  const e = setup(api, post(7));
  e.text = ["Hello", "Mine"];
  const warn = console.warn;
  console.warn = () => undefined;
  try {
    m.publishClick(e.host, e.anchor);
    popup().querySelector(".btn.primary").click();
    await until(() => e.chosen.length === 1);
    assert.equal(e.pub.status, "published", "the new status is adopted");
    // Keep mine, as index.ts serverChoice does it.
    Object.assign(e.st, { hold: false, base: server.updated_at, version: server.version, server: [server.title, server.markdown] });
    e.sync?.changed();
    await quiet(60);
  } finally {
    console.warn = warn;
  }
  assert.deepEqual(api.calls.map((c) => c[0]), ["update", "get"]);
  assert.equal(m.label(e.pub), tx("admin.editor.pub.update"));
});

// Review item 6: a post created by autosave shows Trash without a publish or a reload.
test("a post created by autosave knows its id: the panel shows Trash", async () => {
  const api = fakeApi();
  const e = setup(api, undefined, ["Hello", "Body"]);
  e.sync.changed();
  await until(() => api.calls.length === 1 && location.pathname === "/admin/posts/7");
  assert.equal(e.pub.id, 7);
  m.openPanel(e.host);
  assert.ok(document.querySelector('[data-act="trash"]'), "Trash shows");
});

test("publishing a draft sends one update: text, status, version; no time, no slug when unchanged", async () => {
  const api = fakeApi();
  const e = setup(api, post(7));
  e.text = ["Hello", "Body edited"];
  m.publishClick(e.host, e.anchor);
  assert.match(popup().textContent, new RegExp(tx("admin.editor.pub.q_publish")));
  assert.match(popup().textContent, /\/en\/hello/);
  popup().querySelector(".btn.primary").click();
  await until(() => e.applied === 1);
  assert.deepEqual(api.calls, [["update", 7, { expected_version: 3, title: "Hello", markdown: "Body edited", status: "published" }]]);
  assert.equal(e.pub.status, "published");
  assert.equal(e.st.version, 4);
  assert.equal(toastText(), `${tx("admin.editor.pub.published_toast")}${tx("admin.editor.pub.view_post")}`);
  assert.equal(document.querySelector(".toast a").getAttribute("href"), "/en/hello");
  assert.deepEqual(e.busy, [true, false]);
});

test("the confirm shows address, when and length; Cancel sends nothing", async () => {
  const api = fakeApi();
  const e = setup(api, post(7));
  m.publishClick(e.host, e.anchor);
  const text = popup().textContent;
  for (const k of ["address", "when", "length", "now"]) assert.ok(text.includes(tx(`admin.editor.pub.${k}`)), k);
  popup().querySelector(".btn.quiet").click();
  await quiet();
  assert.equal(api.calls.length, 0);
});

test("a changed slug is sent; Update on a live post sends status published and no time", async () => {
  const api = fakeApi();
  const e = setup(api, post(7, { status: "published", published_at: 1000 }));
  e.pub.typed = "new-link";
  e.text = ["Hello", "Fixed typo"];
  m.publishClick(e.host, e.anchor);
  assert.match(popup().textContent, new RegExp(tx("admin.editor.pub.q_update")));
  popup().querySelector(".btn.primary").click();
  await until(() => e.applied === 1);
  assert.deepEqual(api.calls, [["update", 7, { expected_version: 3, title: "Hello", markdown: "Fixed typo", status: "published", slug: "new-link" }]]);
});

test("scheduling refuses a past time before sending anything", async () => {
  const api = fakeApi();
  const e = setup(api, post(7));
  e.pub.later = true;
  e.pub.at = m.localValue(Date.now() - 3_600_000);
  m.publishClick(e.host, e.anchor);
  assert.equal(popup(), null, "no confirm");
  assert.equal(toastText(), tx("admin.editor.pub.time_past"));
  await quiet();
  assert.equal(api.calls.length, 0);
  e.pub.at = "";
  m.publishClick(e.host, e.anchor);
  assert.equal(toastText(), tx("admin.editor.pub.time_missing"));
  assert.equal(api.calls.length, 0);
});

test("scheduling sends status scheduled and the chosen time", async () => {
  const api = fakeApi();
  const e = setup(api, post(7));
  e.pub.later = true;
  e.pub.at = m.localValue(Date.now() + 86_400_000);
  const at = new Date(e.pub.at).getTime();
  m.publishClick(e.host, e.anchor);
  assert.match(popup().textContent, new RegExp(tx("admin.editor.pub.q_schedule")));
  popup().querySelector(".btn.primary").click();
  await until(() => e.applied === 1);
  assert.deepEqual(api.calls, [["update", 7, { expected_version: 3, title: "Hello", markdown: "Body", status: "scheduled", published_at: at }]]);
  assert.match(toastText(), /Scheduled for/);
  assert.equal(e.pub.later, true);
});

test("a scheduled post switched to now sends the current time, so it does not keep its future one", async () => {
  const api = fakeApi();
  const e = setup(api, post(7, { status: "scheduled", published_at: Date.now() + 9e8 }));
  e.pub.later = false;
  const before = Date.now();
  m.publishClick(e.host, e.anchor);
  popup().querySelector(".btn.primary").click();
  await until(() => e.applied === 1);
  const input = api.calls[0][2];
  assert.equal(input.status, "published");
  assert.ok(input.published_at >= before && input.published_at <= Date.now());
});

test("unpublish asks first, then sends status draft and published_at null", async () => {
  const api = fakeApi();
  const e = setup(api, post(7, { status: "published", published_at: 1000 }));
  m.openPanel(e.host);
  document.querySelector('[data-act="unpublish"]').click();
  await quiet();
  assert.equal(api.calls.length, 0, "not before the confirm");
  clickIn(".confirm .btn.danger");
  await until(() => e.applied === 1);
  assert.deepEqual(api.calls, [["update", 7, { expected_version: 3, status: "draft", published_at: null }]]);
  assert.equal(e.pub.status, "draft");
  assert.equal(toastText(), tx("admin.editor.pub.unpublished"));
});

test("trash asks first, then calls the trash route", async () => {
  const api = fakeApi();
  const e = setup(api, post(7));
  m.openPanel(e.host);
  document.querySelector('[data-act="trash"]').click();
  assert.equal(api.calls.length, 0);
  clickIn(".confirm .btn.danger");
  await until(() => api.calls.length === 1);
  assert.deepEqual(api.calls, [["call", "POST", "/api/v1/posts/7/trash"]]);
  await until(() => toastText() === tx("admin.editor.pub.trashed"));
  assert.equal(location.pathname, "/admin/");
});

test("a stale version opens the choice and sends nothing more", async () => {
  const server = post(7, { markdown: "From elsewhere", version: 5 });
  const api = fakeApi({ update: () => conflict("expected_version", 5), get: () => ({ ok: true, value: server }) });
  const e = setup(api, post(7));
  e.text = ["Hello", "Mine"];
  m.publishClick(e.host, e.anchor);
  popup().querySelector(".btn.primary").click();
  await until(() => e.chosen.length === 1);
  assert.equal(e.chosen[0], server);
  await quiet(80);
  assert.deepEqual(api.calls.map((c) => c[0]), ["update", "get"]);
  assert.equal(e.st.hold, true, "held until the writer picks");
  assert.equal(e.pub.status, "draft");
});

test("a taken slug says so and opens the slug field", async () => {
  const api = fakeApi({ update: () => conflict("slug") });
  const e = setup(api, post(7));
  e.pub.typed = "taken";
  m.publishClick(e.host, e.anchor);
  popup().querySelector(".btn.primary").click();
  await until(() => toastText() === tx("admin.editor.pub.slug_conflict"));
  const panel = document.querySelector(".panel");
  assert.ok(panel, "panel open");
  assert.equal(panel.querySelector(".err").textContent, tx("admin.editor.pub.slug_conflict"));
  assert.equal(document.activeElement, panel.querySelector("[data-slug]"));
  assert.equal(e.pub.status, "draft");
});

test("a taken slug with the panel already open shows the message there", async () => {
  const api = fakeApi({ update: () => conflict("slug") });
  const e = setup(api, post(7));
  m.openPanel(e.host);
  const field = document.querySelector("[data-slug]");
  field.value = "taken";
  field.dispatchEvent(new win.Event("input"));
  field.dispatchEvent(new win.Event("change"));
  m.publishClick(e.host, e.anchor);
  popup().querySelector(".btn.primary").click();
  await until(() => document.querySelector(".panel .err").textContent === tx("admin.editor.pub.slug_conflict"));
  assert.equal(document.querySelector("[data-slug]").value, "taken", "what the writer typed stays");
  assert.equal(document.activeElement, document.querySelector("[data-slug]"));
});

test("another failure says so plainly and keeps the text", async () => {
  const api = fakeApi({ update: () => ({ ok: false, error: { code: "internal", status: 500 } }) });
  const e = setup(api, post(7));
  const log = console.error;
  const logged = [];
  console.error = (...a) => logged.push(a.join(" "));
  try {
    m.publishClick(e.host, e.anchor);
    popup().querySelector(".btn.primary").click();
    await until(() => toastText() === tx("admin.editor.pub.failed"));
  } finally {
    console.error = log;
  }
  assert.match(logged.join("\n"), /the update failed: internal \(500\)/);
  assert.equal(e.applied, 0);
});

test("offline: the button says so and sends nothing", async () => {
  const api = fakeApi();
  const e = setup(api, post(7));
  e.online = false;
  m.publishClick(e.host, e.anchor);
  assert.match(popup().textContent, new RegExp(tx("admin.editor.pub.offline_title")));
  assert.equal(popup().querySelector(".btn"), null);
  assert.equal(api.calls.length, 0);
});

test("a new post publishes after a single create, then one update", async () => {
  const api = fakeApi();
  const e = setup(api, undefined, ["Hello", "Body"]);
  m.publishClick(e.host, e.anchor);
  assert.match(popup().textContent, /\/en\/hello/);
  popup().querySelector(".btn.primary").click();
  await until(() => e.applied === 1);
  assert.deepEqual(api.calls.map((c) => c[0]), ["create", "update"]);
  assert.equal(api.calls[1][1], 7);
  assert.deepEqual(api.calls[1][2], { expected_version: 1, title: "Hello", markdown: "Body", status: "published" });
  assert.equal(e.pub.id, 7);
  assert.equal(location.pathname, "/admin/posts/7");
  await quiet(80);
  assert.equal(api.calls.length, 2, "nothing else is sent");
});

// Found in the merge: the create's answer (a draft) must not undo "later" and its time.
test("a new post scheduled before it exists is created once, then scheduled", async () => {
  const api = fakeApi();
  const e = setup(api, undefined, ["Later", "Body"]);
  e.pub.later = true;
  e.pub.at = m.localValue(Date.now() + 86_400_000);
  const at = new Date(e.pub.at).getTime();
  m.publishClick(e.host, e.anchor);
  popup().querySelector(".btn.primary").click();
  await until(() => e.applied === 1);
  assert.deepEqual(api.calls.map((c) => c[0]), ["create", "update"]);
  assert.deepEqual(api.calls[1][2], { expected_version: 1, title: "Later", markdown: "Body", status: "scheduled", published_at: at });
});

test("a failed create publishes nothing, and says why in the log", async () => {
  const api = fakeApi({ create: () => ({ ok: false, error: { code: "internal", status: 500 } }) });
  const e = setup(api, undefined);
  const log = console.error;
  const logged = [];
  console.error = (...a) => logged.push(a.join(" "));
  try {
    m.publishClick(e.host, e.anchor);
    popup().querySelector(".btn.primary").click();
    await until(() => toastText() === tx("admin.editor.pub.failed"));
  } finally {
    console.error = log;
  }
  assert.deepEqual(api.calls.map((c) => c[0]), ["create"]);
  // Review item 7: the toast alone left nothing for the next person to find.
  assert.match(logged.join("\n"), /nothing to publish yet/);
});

// PR #91 review, item 4: no autosave PATCH races the trash.
test("Trash holds autosave: a save waiting to go is not sent beside the trash request", async () => {
  let release;
  const gate = new Promise((r) => (release = r));
  const api = fakeApi({ call: async () => (await gate, { ok: true, value: post(7, { status: "trashed" }) }) });
  const e = setup(api, post(7));
  e.text = ["Hello", "Typed just before"];
  e.sync.changed(); // autosave due in 5 ms
  m.openPanel(e.host);
  document.querySelector('[data-act="trash"]').click();
  clickIn(".confirm .btn.danger");
  await quiet(60);
  release();
  await until(() => toastText() === tx("admin.editor.pub.trashed"));
  await quiet(60);
  assert.deepEqual(api.calls.map((c) => c[0]), ["call"], "no PATCH beside the trash");
});

// Item 5: Unpublish and Trash wait while an update is out.
test("the panel's Unpublish and Trash are off while a publish step is out", async () => {
  let release;
  const gate = new Promise((r) => (release = r));
  const api = fakeApi({
    update: async (id, input) => {
      await gate;
      return { ok: true, value: post(id, { ...input, status: "published", published_at: 5000, version: input.expected_version + 1 }) };
    },
  });
  const e = setup(api, post(7, { status: "published", published_at: 1000 }));
  m.openPanel(e.host);
  m.publishClick(e.host, e.anchor);
  popup().querySelector(".btn.primary").click();
  await until(() => updates(api).length === 1);
  for (const act of ["unpublish", "trash"]) assert.equal(document.querySelector(`[data-act="${act}"]`).disabled, true, `${act} off while busy`);
  release();
  await until(() => e.applied === 1);
  for (const act of ["unpublish", "trash"]) assert.equal(document.querySelector(`[data-act="${act}"]`).disabled, false, `${act} back on`);
});

// Item 6: the server's time refusal is the time message, with the time field open.
test("a published_at refusal says the time is past and opens the time field", async () => {
  const api = fakeApi({ update: () => ({ ok: false, error: { code: "validation.invalid", status: 422, field: "published_at" } }) });
  const e = setup(api, post(7));
  e.pub.later = true;
  e.pub.at = m.localValue(Date.now() + 86_400_000);
  const warn = console.warn;
  console.warn = () => undefined;
  try {
    m.publishClick(e.host, e.anchor);
    popup().querySelector(".btn.primary").click();
    await until(() => toastText() === tx("admin.editor.pub.time_past"));
  } finally {
    console.warn = warn;
  }
  assert.ok(document.querySelector(".panel"), "panel open");
  assert.equal(document.activeElement, document.querySelector("[data-at]"));
});

// Item 7: an Unpublish stopped by a text choice says so; it is not dropped silently.
test("Unpublish that meets a 409 with other text shows the choice and says to press it again", async () => {
  const server = post(7, { status: "published", published_at: 1000, markdown: "Theirs", version: 5 });
  const api = fakeApi({ update: () => conflict("expected_version", 5), get: () => ({ ok: true, value: server }) });
  const e = setup(api, post(7, { status: "published", published_at: 1000 }));
  e.text = ["Hello", "Mine"];
  m.openPanel(e.host);
  document.querySelector('[data-act="unpublish"]').click();
  clickIn(".confirm .btn.danger");
  await until(() => e.chosen.length === 1);
  const said = tx("admin.editor.pub.pick_then_again").replace("{button}", tx("admin.editor.pub.unpublish"));
  assert.equal(toastText(), said);
  assert.equal(e.pub.status, "published", "nothing unpublished yet");
});

// Items 1 and 3: a trashed post offers only Restore; Restore brings it back.
test("a trashed post: the panel has no Trash; Restore calls the restore route and adopts the post", async () => {
  const restored = post(7, { status: "draft", version: 4, updated_at: 300 });
  const api = fakeApi({ call: () => ({ ok: true, value: restored }) });
  const e = setup(api, post(7, { status: "trashed", version: 3 }));
  e.trashed = true;
  m.openPanel(e.host);
  assert.equal(document.querySelector('[data-act="trash"]'), null);
  assert.equal(typeof m.restore, "function");
  await m.restore(e.host);
  assert.deepEqual(api.calls, [["call", "POST", "/api/v1/posts/7/restore"]]);
  assert.deepEqual(e.lived, [restored]);
  assert.equal(e.st.version, 4, "based on the trashed copy: the restored version is the base");
  assert.equal(toastText(), tx("admin.editor.pub.restored"));
});
