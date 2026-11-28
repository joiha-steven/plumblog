// Saving drafts to the server (web/admin/src/editor/sync.ts), driven through the real module
// with a fake Api, a Map behind IndexedDB and a stand-in DOM: one create per new post, the
// draft key and URL moving, versions, the conflict choice, published posts, offline, slugs,
// warnings and when the local draft is dropped.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { build } from "esbuild";
import { Window } from "happy-dom";
import { RENDER_WARNINGS } from "../../web/admin/src/api/model.gen.ts";

const win = new Window({ url: "http://localhost/admin/posts/new?draft=abcdefghijkl" });
for (const k of ["document", "location", "history", "Node", "HTMLElement", "addEventListener", "removeEventListener"]) {
  const v = typeof win[k] === "function" && !/^[A-Z]/.test(k) ? win[k].bind(win) : win[k];
  Object.defineProperty(globalThis, k, { value: v, configurable: true, writable: true });
}

const root = new URL("../../", import.meta.url);
const out = await build({ entryPoints: [new URL("web/admin/src/editor/sync.ts", root).pathname], bundle: true, write: false, format: "esm", logLevel: "silent" });
const { startState } = await import(new URL("web/admin/src/editor/drafts.ts", root).href);
const { settled, slugify, startSync } = await import(`data:text/javascript;base64,${Buffer.from(out.outputFiles[0].text).toString("base64")}`);
const outClient = await build({ entryPoints: [new URL("web/admin/src/api/client.ts", root).pathname], bundle: true, write: false, format: "esm", logLevel: "silent" });
const client = await import(`data:text/javascript;base64,${Buffer.from(outClient.outputFiles[0].text).toString("base64")}`);

// A Map behind the slice of IndexedDB drafts.ts uses (as in auth.test.mjs), plus single-key delete.
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

const DELAY = 5;
const settle = async (ms = 40) => {
  for (let i = 0; i < 4; i++) await new Promise((r) => setTimeout(r, ms / 4));
};
/** The keys holding drafts; a created post's new key keeps only a note of where it went (drafts.ts moveDraft). */
const draftKeys = () => [...rows].filter(([, v]) => typeof v.movedTo !== "string").map(([k]) => k);
const draft = (title, body, rev, extra = {}) => ({ title, body, base: null, rev, savedAt: 1, ...extra });
const post = (id, title, markdown, version = 1, warnings = []) => ({
  id, kind: "post", translation_group: id, lang: "en", slug: "s", status: "draft", trashed_from: null, published_at: null, author_id: 1,
  title, excerpt: "", word_count: 1, reading_minutes: 1, pinned: false, comments_enabled: true, cover_media_id: null, series_id: null,
  series_position: null, created_at: 1, updated_at: 100 + version, version, tags: [], markdown, html: "", media_ids: [], warnings,
});
const conflict = (field, version) => ({ ok: false, error: { code: "store.conflict", status: 409, field, ...(version ? { version } : {}) } });

/** A fake Api: `create`/`update`/`get` decide each answer (sync or async); every call is recorded. */
function fakeApi({ create, update, get } = {}) {
  const calls = [];
  let next = 7;
  return {
    calls,
    async createPost(input) {
      calls.push(["create", input]);
      return create ? create(input, calls) : { ok: true, value: post(next++, input.title, input.markdown) };
    },
    async updatePost(id, input) {
      calls.push(["update", id, input]);
      return update ? update(id, input, calls) : { ok: true, value: post(id, input.title, input.markdown, input.expected_version + 1) };
    },
    async post(id) {
      calls.push(["get", id]);
      return get(id);
    },
  };
}

/** The editor side: its text, its state, what it showed. `save()` acts like index.ts's local save. */
function editor(api, o = {}) {
  const e = {
    title: o.title ?? "",
    body: o.body ?? "",
    said: [],
    chosen: [],
    lived: [],
    made: [],
    online: true,
    alive: true,
    note: document.createElement("div"),
  };
  e.st = o.st ?? {
    key: o.key ?? newKey(),
    base: o.post?.updated_at ?? null,
    version: o.post?.version,
    rev: rows.get(o.key ?? newKey())?.rev ?? 0,
    paused: false,
    hold: false,
    server: o.post && [o.post.title, o.post.markdown],
    q: Promise.resolve(),
  };
  e.save = async (title = e.title, body = e.body) => {
    e.title = title;
    e.body = body;
    const job = e.st.q.then(async () => {
      const cur = rows.get(e.st.key)?.rev ?? 0;
      assert.equal(cur, e.st.rev, "a local save never conflicts with this tab's own sync");
      rows.set(e.st.key, draft(title, body, cur + 1, { base: e.st.base, version: e.st.version }));
      e.st.rev = cur + 1;
    });
    e.st.q = job;
    await job;
    e.sync.changed();
  };
  e.sync = startSync(
    {
      api, user: 1, lang: "vi", st: e.st, id: o.post?.id, draft: (o.post?.status ?? "draft") === "draft",
      offline: () => !e.online, alive: () => e.alive, text: () => [e.title, e.body],
      say: (s) => e.said.push(s), choose: (p) => e.chosen.push(p), live: (p) => e.lived.push(p), made: (p) => e.made.push(p), note: e.note,
    },
    DELAY,
    o.slow,
  );
  return e;
}

// Each test is a new post of its own: creates are remembered per draft key for the page's life.
let drafts = 0;
const newKey = () => `1:new-${new URLSearchParams(location.search).get("draft")}`;
const reset = (url = `/admin/posts/new?draft=abcdefghijk${(++drafts).toString(36)}`) => {
  rows.clear();
  history.replaceState(null, "", url);
  document.body.replaceChildren();
};

test("slugify folds Vietnamese and other marks, keeps a-z 0-9 and inner hyphens, caps the length", () => {
  assert.equal(slugify("M\u1ed9t tu\u1ea7n \u1edf \u0110\u00e0 L\u1ea1t"), "mot-tuan-o-da-lat");
  assert.equal(slugify("\u0110\u1eb6NG \u0111\u01b0\u1eddng ph\u1ed1"), "dang-duong-pho");
  assert.equal(slugify("Cr\u00e8me br\u00fbl\u00e9e, \u00d1and\u00fa!"), "creme-brulee-nandu");
  assert.equal(slugify("  C++ & Rust 2.0  "), "c-rust-2-0");
  assert.equal(slugify("---a---b---"), "a-b");
  assert.equal(slugify(""), "untitled");
  assert.equal(slugify("!!! ???"), "untitled");
  assert.equal(slugify("\u65e5\u672c\u8a9e"), "untitled");
  const long = slugify(`${"word ".repeat(19)}abcdefghij`);
  assert.ok(long.length <= 100 && !long.endsWith("-"), long);
  assert.equal(slugify("a".repeat(150)).length, 100);
  assert.equal(slugify("a".repeat(150), 2), `${"a".repeat(98)}-2`);
  assert.equal(slugify("", 3), "untitled-3");
});

test("a new post is created once, even with edits during a slow create; then updated with its version", async () => {
  reset();
  let release;
  const api = fakeApi({ create: (input) => new Promise((r) => (release = () => r({ ok: true, value: post(7, input.title, input.markdown, 1) }))) });
  const e = editor(api);
  await e.save("Hello", "first");
  await settle();
  assert.equal(api.calls.length, 1);
  for (const b of ["second", "third", "fourth"]) {
    await e.save("Hello", b);
    await settle();
  }
  assert.equal(api.calls.length, 1, "no second create while the first is out");
  release();
  await settle();
  assert.deepEqual(
    api.calls.map((c) => c[0]),
    ["create", "update"],
  );
  assert.deepEqual(api.calls[0][1], { lang: "vi", slug: "hello", title: "Hello", markdown: "first" });
  assert.deepEqual(api.calls[1].slice(1), [7, { expected_version: 1, title: "Hello", markdown: "fourth" }]);
  assert.equal(e.st.version, 2);
});

test("a create with no answer for a while says so, keeps saying so over local saves, and stops once it settles", async () => {
  reset();
  let release;
  const api = fakeApi({ create: (input) => new Promise((r) => (release = () => r({ ok: true, value: post(7, input.title, input.markdown, 1) }))) });
  const e = editor(api, { slow: 150 });
  const warn = console.warn;
  console.warn = () => undefined;
  try {
    await e.save("Hello", "first");
    await settle();
    assert.ok(!e.said.includes("admin.editor.sync.slow"), "not before the bound");
    await new Promise((r) => setTimeout(r, 200));
    assert.equal(e.said.at(-1), "admin.editor.sync.slow");
    await e.save("Hello", "second");
    assert.equal(e.said.at(-1), "admin.editor.sync.slow", "a local save meanwhile does not hide it");
    release();
    await settle();
  } finally {
    console.warn = warn;
  }
  assert.equal(e.said.at(-1), "admin.editor.saved");
  assert.ok(e.said.lastIndexOf("admin.editor.sync.slow") < e.said.lastIndexOf("admin.editor.saved"));
});

test("on create the URL and the draft key move to the post; the draft stays while the text differs", async () => {
  reset();
  let release;
  const api = fakeApi({ create: (input) => new Promise((r) => (release = () => r({ ok: true, value: post(7, input.title, input.markdown, 1) }))) });
  const e = editor(api);
  await e.save("T", "sent");
  await settle();
  e.body = "typed meanwhile"; // not saved locally yet: the text differs from what was sent
  release();
  await settle();
  assert.equal(location.pathname + location.search, "/admin/posts/7");
  assert.equal(e.st.key, "1:post-7");
  assert.deepEqual(draftKeys(), ["1:post-7"]);
  const note = [...rows].find(([k]) => k.startsWith("1:new-"))?.[1];
  assert.equal(note?.movedTo, "1:post-7", "the new key says where it went");
  assert.ok(Math.abs(note.at - Date.now()) < 5000, "and when, so it can be swept");
  assert.equal(rows.get("1:post-7").body, "sent");
  assert.equal(rows.get("1:post-7").version, 1);
  assert.equal(rows.get("1:post-7").base, 101);
});

test("the local draft is dropped once the server holds the exact text, not if a save came meanwhile", async () => {
  reset("/admin/posts/3");
  const p = post(3, "T", "old", 4);
  rows.set("1:post-3", draft("T", "x", 2));
  const e = editor(fakeApi(), { key: "1:post-3", post: p, title: "T", body: "x" });
  await e.save("T", "new");
  await settle();
  assert.equal(rows.has("1:post-3"), false, "server has the text: draft dropped");
  assert.equal(e.st.rev, 0);
  assert.ok(e.said.includes("admin.editor.saved"));

  // A local save lands while the update is out: same text, newer rev, so the draft stays.
  reset("/admin/posts/3");
  let release;
  const api = fakeApi({ update: (id, input) => new Promise((r) => (release = () => r({ ok: true, value: post(id, input.title, input.markdown, 5) }))) });
  rows.set("1:post-3", draft("T", "x", 1));
  const f = editor(api, { key: "1:post-3", post: p, title: "T", body: "x" });
  await f.save("T", "y");
  await settle();
  const job = (f.st.q = f.st.q.then(() => {
    rows.set("1:post-3", draft("T", "y", 3));
    f.st.rev = 3;
  }));
  await job;
  release();
  await settle();
  assert.equal(rows.get("1:post-3")?.rev, 3, "a draft saved after the request started is kept");
});

test("every update sends expected_version; a 409 asks, sends nothing until the writer picks", async () => {
  reset("/admin/posts/3");
  const p = post(3, "T", "base", 4);
  const theirs = post(3, "T", "theirs", 6);
  let stale = true;
  const api = fakeApi({
    update: (id, input) => (stale ? conflict("expected_version", 6) : { ok: true, value: post(id, input.title, input.markdown, input.expected_version + 1) }),
    get: () => ({ ok: true, value: theirs }),
  });
  const e = editor(api, { key: "1:post-3", post: p, title: "T", body: "base" });
  await e.save("T", "mine");
  await settle();
  assert.deepEqual(api.calls[0], ["update", 3, { expected_version: 4, title: "T", markdown: "mine" }]);
  assert.deepEqual(api.calls[1], ["get", 3]);
  assert.equal(e.chosen.length, 1);
  assert.equal(e.chosen[0].version, 6);
  assert.equal(e.said.at(-1), "admin.editor.not_saved");
  await e.save("T", "mine, more");
  await settle();
  assert.equal(api.calls.length, 2, "nothing sent while the choice is open");
  assert.equal(rows.get("1:post-3").body, "mine, more", "the local draft keeps every change");
  // The writer's pick goes through index.ts serverChoice: tools/worker-test/editor-sync.test.mjs.
});

test("typing while a save is out: its answer does not say Saved, even when the next send cannot go", async () => {
  reset("/admin/posts/3");
  let release;
  const api = fakeApi({
    update: (id, input) => new Promise((r) => (release = () => r({ ok: true, value: post(id, input.title, input.markdown, input.expected_version + 1) }))),
  });
  const e = editor(api, { key: "1:post-3", post: post(3, "T", "base", 4), title: "T", body: "base" });
  await e.save("T", "sent");
  await settle();
  assert.equal(api.calls.length, 1);
  await e.save("T", "sent, then more");
  e.online = false; // the follow-up send exits quietly
  release();
  await settle();
  assert.equal(api.calls.length, 1);
  assert.equal(e.said.at(-1), "admin.editor.saved_here", "the newer text is only on this device");
});

test("a 409 whose server copy is still a draft: after Keep mine the draft autosaves again", async () => {
  reset("/admin/posts/3");
  const theirs = post(3, "T", "theirs", 6);
  let stale = true;
  const api = fakeApi({
    update: (id, input) => (stale ? conflict("expected_version", 6) : { ok: true, value: post(id, input.title, input.markdown, input.expected_version + 1) }),
    get: () => ({ ok: true, value: theirs }),
  });
  const e = editor(api, { key: "1:post-3", post: post(3, "T", "base", 4), title: "T", body: "base" });
  await e.save("T", "mine");
  await settle();
  assert.equal(e.chosen.length, 1);
  assert.deepEqual(e.lived, [], "a draft copy does not stop the sync");
  // Keep mine, as index.ts serverChoice does it.
  stale = false;
  Object.assign(e.st, { hold: false, base: theirs.updated_at, version: theirs.version, server: [theirs.title, theirs.markdown] });
  await e.save("T", "mine, kept");
  await settle();
  assert.deepEqual(api.calls.at(-1), ["update", 3, { expected_version: 6, title: "T", markdown: "mine, kept" }]);
});

test("a 409 whose server copy is published, scheduled or trashed stops sending for good", async () => {
  for (const status of ["published", "scheduled", "trashed"]) {
    reset("/admin/posts/3");
    const theirs = { ...post(3, "T", "live text", 6), status };
    const api = fakeApi({ update: () => conflict("expected_version", 6), get: () => ({ ok: true, value: theirs }) });
    const e = editor(api, { key: "1:post-3", post: post(3, "T", "base", 4), title: "T", body: "base" });
    await e.save("T", "mine");
    await settle();
    assert.deepEqual(api.calls.map((c) => c[0]), ["update", "get"], status);
    assert.deepEqual(e.lived.map((p) => p.status), [status], "the editor is told, once");
    assert.equal(e.chosen.length, 1, "the writer still picks which text stays here");
    assert.equal(e.said.at(-1), "admin.editor.saved_here");
    // Whatever the pick sets (index.ts adopt clears hold and takes the version), nothing is sent.
    Object.assign(e.st, { hold: false, base: theirs.updated_at, version: theirs.version, server: [theirs.title, theirs.markdown] });
    await e.save("T", "mine, kept");
    await settle();
    assert.equal(api.calls.length, 2, `${status}: never sent after it went live`);
    assert.equal(rows.get("1:post-3").body, "mine, kept", "kept on this device");
  }
});

test("a request with no answer settles as network within its bound, and the next change saves", async () => {
  reset("/admin/posts/3");
  const real = globalThis.fetch;
  const sent = [];
  globalThis.fetch = (path, init) => {
    sent.push([init.method, path]);
    // The first PATCH hangs until aborted; later ones answer.
    if (sent.length === 1)
      return new Promise((_, no) => init.signal.addEventListener("abort", () => no(new DOMException("aborted", "AbortError"))));
    const body = JSON.parse(init.body);
    return Promise.resolve(new Response(JSON.stringify(post(3, body.title, body.markdown, body.expected_version + 1)), { status: 200 }));
  };
  const errors = [];
  const logErr = console.error;
  console.error = (...a) => errors.push(a.join(" "));
  try {
    const api = client.httpApi(30);
    // A create is never cut short: a slow one that lands would be sent again as a second post.
    const create = api.createPost({ lang: "en", slug: "s", title: "T", markdown: "m" });
    const raced = await Promise.race([create, new Promise((ok) => setTimeout(() => ok("pending"), 200))]);
    assert.equal(raced, "pending", "a create has no bound");
    sent.length = 0;
    const hung = await Promise.race([api.updatePost(3, { expected_version: 4 }), new Promise((ok) => setTimeout(() => ok("still hung"), 1000))]);
    assert.deepEqual(hung, { ok: false, error: { code: "network", status: 0 } });
    assert.match(errors.join("\n"), /no answer within 30 ms/);

    sent.length = 0;
    const e = editor(api, { key: "1:post-3", post: post(3, "T", "base", 4), title: "T", body: "base" });
    await e.save("T", "one");
    await settle(200);
    assert.equal(e.said.at(-1), "admin.editor.not_saved", "a hung save is a failed save");
    assert.equal(rows.get("1:post-3").body, "one", "the draft stays");
    await e.save("T", "two");
    await settle(200);
    assert.deepEqual(sent, [["PATCH", "/api/v1/posts/3"], ["PATCH", "/api/v1/posts/3"]], "busy cleared: the next change is sent");
    assert.equal(e.said.at(-1), "admin.editor.saved");
  } finally {
    globalThis.fetch = real;
    console.error = logErr;
  }
});

test("an expired session waits for the editor's local save, at most its bound", async () => {
  let done = false;
  client.session.flush = () => new Promise((ok) => setTimeout(() => ok((done = true)), 20));
  await client.flushLocal(1000);
  assert.equal(done, true, "waited for the flush");
  client.session.flush = () => new Promise(() => undefined);
  const t0 = Date.now();
  await client.flushLocal(50);
  assert.ok(Date.now() - t0 < 500, "a stuck flush never keeps the writer from signing in");
  client.session.flush = () => Promise.reject(new Error("boom"));
  const logErr = console.error;
  console.error = () => undefined;
  await client.flushLocal(50);
  console.error = logErr;
  client.session.flush = undefined;
});

test("a 409 whose server copy already has this text needs no choice", async () => {
  reset("/admin/posts/3");
  const api = fakeApi({ update: () => conflict("expected_version", 9), get: () => ({ ok: true, value: post(3, "T", "same", 9) }) });
  const e = editor(api, { key: "1:post-3", post: post(3, "T", "base", 4), title: "T", body: "base" });
  await e.save("T", "same");
  await settle();
  assert.equal(e.chosen.length, 0);
  assert.equal(e.st.version, 9);
  assert.equal(rows.has("1:post-3"), false);
});

test("published and scheduled posts are never sent", async () => {
  for (const status of ["published", "scheduled", "trashed"]) {
    reset("/admin/posts/3");
    const api = fakeApi();
    const e = editor(api, { key: "1:post-3", post: { ...post(3, "T", "live", 2), status }, title: "T", body: "live" });
    await e.save("T", "edited");
    await settle();
    assert.equal(api.calls.length, 0, status);
    assert.equal(rows.get("1:post-3").body, "edited", "kept on this device");
    assert.deepEqual(e.said, []);
  }
});

test("offline sends nothing; back online, the next change sends", async () => {
  reset();
  const api = fakeApi();
  const e = editor(api);
  e.online = false;
  await e.save("T", "b");
  await settle();
  assert.equal(api.calls.length, 0);
  assert.deepEqual(e.said, []);
  e.online = true;
  await e.save("T", "b2");
  await settle();
  assert.equal(api.calls.length, 1);
});

test("an empty new post stays on this device", async () => {
  reset();
  const api = fakeApi();
  const e = editor(api);
  await e.save(" ", "\n");
  await settle();
  assert.equal(api.calls.length, 0);
});

test("a taken slug retries with -2, -3; after 5 tries a plain message", async () => {
  reset();
  const taken = new Set(["hello", "hello-2"]);
  const api = fakeApi({ create: (input) => (taken.has(input.slug) ? conflict("slug") : { ok: true, value: post(8, input.title, input.markdown) }) });
  const e = editor(api);
  await e.save("Hello", "b");
  await settle();
  assert.deepEqual(
    api.calls.map((c) => c[1].slug),
    ["hello", "hello-2", "hello-3"],
  );
  assert.equal(location.pathname, "/admin/posts/8");

  reset();
  const all = fakeApi({ create: () => conflict("slug") });
  const f = editor(all);
  await f.save("Hello", "b");
  await settle();
  assert.equal(all.calls.length, 5);
  assert.equal(document.querySelector(".toast")?.textContent, "admin.editor.sync.slug_taken");
  assert.equal(f.said.at(-1), "admin.editor.not_saved");
  assert.equal(rows.get(newKey()).body, "b", "the local draft is kept");
});

test("a failed save keeps the local draft, says so once, and retries on the next change", async () => {
  reset();
  let down = true;
  const api = fakeApi({ create: (input) => (down ? { ok: false, error: { code: "network", status: 0 } } : { ok: true, value: post(9, input.title, input.markdown) }) });
  const e = editor(api);
  await e.save("T", "a");
  await settle();
  assert.equal(e.said.at(-1), "admin.editor.not_saved");
  assert.equal(document.querySelector(".toast")?.textContent, "admin.editor.sync.failed");
  document.querySelector(".toast")?.remove();
  await e.save("T", "ab");
  await settle();
  assert.equal(document.querySelector(".toast"), null, "one toast per run of failures");
  assert.equal(rows.get(newKey()).body, "ab");
  down = false;
  await e.save("T", "abc");
  await settle();
  assert.equal(api.calls.length, 3);
  assert.equal(e.said.at(-1), "admin.editor.saved");
  assert.equal(draftKeys().length, 0);
});

test("warnings show under the title, then clear on a save without them", async () => {
  reset("/admin/posts/3");
  let warn = ["markdown.math_invalid"];
  const api = fakeApi({ update: (id, input) => ({ ok: true, value: post(id, input.title, input.markdown, input.expected_version + 1, warn) }) });
  const e = editor(api, { key: "1:post-3", post: post(3, "T", "", 1), title: "T" });
  await e.save("T", "$\\frac{$");
  await settle();
  assert.equal(e.note.textContent, "admin.editor.warning.markdown.math_invalid");
  assert.equal(e.note.querySelector("p")?.className, "hint");
  warn = [];
  await e.save("T", "$x$");
  await settle();
  assert.equal(e.note.textContent, "");
});

test("every render warning the server can send has English text", () => {
  const en = readFileSync(new URL("locales/en.toml", root), "utf8");
  const keys = new Set();
  let section = "";
  for (const line of en.split("\n")) {
    const s = /^\[([^\]]+)\]\s*$/.exec(line);
    if (s) section = s[1];
    const k = /^([a-z0-9_]+)\s*=/.exec(line);
    if (k) keys.add(`${section}.${k[1]}`);
  }
  const src = readFileSync(new URL("web/admin/src/editor/sync.ts", root), "utf8");
  for (const w of RENDER_WARNINGS) {
    assert.ok(keys.has(`admin.editor.warning.${w}`), `locales/en.toml has no admin.editor.warning.${w}`);
    assert.match(src, new RegExp(`// i18n:.* admin\\.editor\\.warning\\.${w.replaceAll(".", "\\.")}\\b`), `sync.ts i18n comment lists ${w}`);
  }
});

test("a moved server copy keeps asking after an edit and a reload; nothing is sent", async () => {
  for (const kept of [{ version: 3, base: 103 }, { base: 103 }]) {
    reset("/admin/posts/3");
    rows.set("1:post-3", draft("T", "mine", 4, kept));
    const theirs = post(3, "T", "theirs", 5);
    const api = fakeApi();
    const first = startState("1:post-3", rows.get("1:post-3"), theirs);
    assert.equal(first.serverMoved, true);
    const e = editor(api, { st: first.st, key: "1:post-3", post: theirs, title: "T", body: "mine" });
    await e.save("T", "mine, one more key");
    await settle();
    assert.equal(rows.get("1:post-3").version, kept.version, "the draft keeps the version it started from");
    const again = startState("1:post-3", rows.get("1:post-3"), theirs);
    assert.equal(again.serverMoved, true, "the reload still asks");
    const f = editor(api, { st: again.st, key: "1:post-3", post: theirs, title: "T", body: "mine, one more key" });
    await f.save();
    await settle();
    assert.deepEqual(api.calls, [], "nothing sent before the writer picks");
  }
});

test("a remount during a slow create waits for it: one create, then an update", async () => {
  reset();
  let release;
  const api = fakeApi({ create: (input) => new Promise((r) => (release = () => r({ ok: true, value: post(7, input.title, input.markdown, 1) }))) });
  const a = editor(api);
  await a.save("T", "one");
  await settle();
  assert.equal(api.calls.length, 1);
  a.alive = false; // online/offline redrew the editor
  const b = editor(api, { title: "T", body: "one" });
  await b.save("T", "two");
  await settle();
  assert.equal(api.calls.length, 1, "no second create while the first is out");
  release();
  await settle();
  assert.deepEqual(api.calls.map((c) => c[0]), ["create", "update"]);
  assert.deepEqual(api.calls[1].slice(1), [7, { expected_version: 1, title: "T", markdown: "two" }]);
  assert.equal(b.st.key, "1:post-7");
  assert.equal(draftKeys().length, 0, "server holds the text: no draft left");
});

test("what the server confirmed is its answer: a normalised text keeps the draft", async () => {
  reset("/admin/posts/3");
  rows.set("1:post-3", draft("T", "x", 1));
  const api = fakeApi({ update: (id, input) => ({ ok: true, value: post(id, input.title, input.markdown.trim(), input.expected_version + 1) }) });
  const e = editor(api, { key: "1:post-3", post: post(3, "T", "x", 1), title: "T", body: "x" });
  await e.save("T", "body  ");
  await settle();
  assert.deepEqual(e.st.server, ["T", "body"]);
  assert.equal(rows.get("1:post-3")?.body, "body  ", "the draft stays: the server holds other text");
});

test("a mount waits at most the bound for a hung create, then holds: nothing sent until it settles, then adopts it", async () => {
  reset();
  let release;
  const api = fakeApi({ create: (input) => new Promise((r) => (release = () => r({ ok: true, value: post(7, input.title, input.markdown, 1) }))) });
  const a = editor(api);
  await a.save("T", "one");
  await settle();
  a.alive = false; // redrawn while the create hangs
  const key = newKey();
  const t0 = Date.now();
  assert.equal(await settled(key, 1, 30), undefined, "the mount goes on after the bound");
  assert.ok(Date.now() - t0 < 500);
  const b = editor(api, { title: "T", body: "one" });
  await b.save("T", "two");
  await settle();
  assert.equal(api.calls.length, 1, "no second create, no update while the first is out");
  assert.ok(!b.said.includes("admin.editor.saved") && !b.said.includes("admin.editor.not_saved"));
  release();
  await settle();
  assert.equal(b.st.key, "1:post-7", "adopted: key moved");
  assert.equal(location.pathname, "/admin/posts/7", "adopted: URL fixed");
  assert.deepEqual(api.calls.map((c) => c[0]), ["create", "update"]);
  assert.deepEqual(api.calls[1].slice(1), [7, { expected_version: 1, title: "T", markdown: "two" }]);
});

test("a create that settles within the bound: the mount gets the post and its draft is moved", async () => {
  reset();
  let release;
  const api = fakeApi({ create: (input) => new Promise((r) => (release = () => r({ ok: true, value: post(8, input.title, input.markdown, 1) }))) });
  const a = editor(api);
  await a.save("T", "one");
  await settle();
  a.alive = false;
  const key = newKey();
  const wait = settled(key, 1, 5000);
  release();
  const p = await wait;
  assert.equal(p?.id, 8);
  assert.deepEqual(draftKeys(), ["1:post-8"]);
  assert.equal(rows.get("1:post-8").version, 1);
});
