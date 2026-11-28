// The posts and pages lists (web/admin/src/lists.ts) driven through the real functions with a
// fake Api, in a stand-in DOM: filters, paging, the short pages "All" has to fill, failures,
// the URL, and the strip of drafts kept on this device.
import assert from "node:assert/strict";
import { test } from "node:test";
import { build } from "esbuild";
import { Window } from "happy-dom";

const win = new Window({ url: "http://localhost/admin/" });
for (const k of ["document", "location", "history", "Node", "HTMLElement"]) {
  Object.defineProperty(globalThis, k, { value: win[k], configurable: true, writable: true });
}

const root = new URL("../../", import.meta.url);
const out = await build({ entryPoints: [new URL("web/admin/src/lists.ts", root).pathname], bundle: true, write: false, format: "esm", logLevel: "silent" });
const lists = await import(`data:text/javascript;base64,${Buffer.from(out.outputFiles[0].text).toString("base64")}`);
const drafts = await import(new URL("web/admin/src/editor/drafts.ts", root).href);

const wait = async () => {
  for (let i = 0; i < 8; i++) await new Promise((r) => setTimeout(r, 0));
};
const post = (id, status = "published", kind = "post") => ({
  id, kind, translation_group: id, lang: "en", slug: `p${id}`, status, trashed_from: null, published_at: 1, author_id: 1,
  title: `Post ${id}`, excerpt: "", word_count: 1, reading_minutes: 1, pinned: false, comments_enabled: true, cover_media_id: null,
  series_id: null, series_position: null, created_at: 1, updated_at: 1, version: 1, tags: [],
});
/** An Api whose `posts` answers from `pages` in order (a function gets the query); records queries. */
const fakeApi = (pages) => {
  const queries = [];
  return {
    queries,
    posts: async (q) => {
      queries.push(q);
      const n = queries.length - 1;
      const a = typeof pages === "function" ? pages(q, n) : pages[n];
      return a.ok === false ? a : { ok: true, value: a };
    },
  };
};
const page = (posts, next = null) => ({ posts, next_cursor: next });
const down = { ok: false, error: { code: "network", status: 0 } };
const newPost = () => document.createElement("a");

function mount(api, o = {}) {
  history.replaceState(null, "", o.url ?? "/admin/");
  document.body.replaceChildren();
  const el = lists.postsList(api, { user: 1, newPost, drafts: async () => [], ...o });
  document.body.append(el);
  return el;
}
const titles = (el) => [...el.querySelectorAll(".rows .row .row-title")].map((x) => x.textContent);
const button = (el, text) => [...el.querySelectorAll("button")].find((b) => b.textContent === text);

test("All hides trashed rows and keeps asking until 20 are visible", async () => {
  // Each page of 20 holds 8 trashed rows: two pages give 24 visible, a third is not asked for.
  const mk = (from) => page(Array.from({ length: 20 }, (_, i) => post(from + i, i < 8 ? "trashed" : "draft")), `c${from}`);
  const api = fakeApi([mk(100), mk(200), mk(300)]);
  const el = mount(api);
  await wait();
  assert.equal(api.queries.length, 2);
  assert.equal(api.queries[0].status, undefined, "All asks without a status");
  assert.equal(api.queries[1].cursor, "c100");
  assert.equal(titles(el).length, 24);
  assert.ok(!el.textContent.includes("Post 100"), "trashed rows are not shown");
  assert.ok(button(el, "admin.list.more"), "more pages remain, so Load more shows");
});

test("All stops after 5 extra requests when every page is mostly trash", async () => {
  const api = fakeApi((_, n) => page([post(n * 10, "draft"), ...Array.from({ length: 19 }, (__, i) => post(n * 10 + i + 1, "trashed"))], `c${n}`));
  const el = mount(api);
  await wait();
  assert.equal(api.queries.length, 6);
  assert.equal(titles(el).length, 6);
  assert.ok(button(el, "admin.list.more"));
});

test("a filter asks for its status once and is kept in the URL", async () => {
  const api = fakeApi((q) => page([post(q.status === "draft" ? 7 : 1, q.status ?? "published")]));
  const el = mount(api);
  await wait();
  button(el, "admin.status.draft").click();
  await wait();
  assert.equal(api.queries.at(-1).status, "draft");
  assert.equal(api.queries.length, 2);
  assert.equal(location.search, "?status=draft");
  assert.equal(button(el, "admin.status.draft").getAttribute("aria-pressed"), "true");
  assert.equal(button(el, "admin.list.all").getAttribute("aria-pressed"), "false");
  assert.deepEqual(titles(el), ["Post 7"]);
  button(el, "admin.list.all").click();
  await wait();
  assert.equal(location.search, "");
  assert.equal(api.queries.at(-1).status, undefined);
});

test("?status= in the URL picks the filter on load; anything else is All", async () => {
  const api = fakeApi(() => page([post(1, "scheduled")]));
  const el = mount(api, { url: "/admin/?status=scheduled" });
  await wait();
  assert.equal(api.queries[0].status, "scheduled");
  assert.equal(button(el, "admin.status.scheduled").getAttribute("aria-pressed"), "true");
  assert.equal(lists.filterOf("?status=nope"), "all");
  assert.equal(lists.filterOf("?status=trashed&x=1"), "trashed");
  assert.equal(lists.filterOf(""), "all");
});

test("Load more is busy while it loads, appends, focuses the first new row and hides on the last page", async () => {
  let release;
  const held = new Promise((r) => (release = r));
  const api = fakeApi(async () => undefined);
  api.posts = async (q) => {
    api.queries.push(q);
    if (api.queries.length === 1) return { ok: true, value: page(Array.from({ length: 20 }, (_, i) => post(i + 1)), "next") };
    await held;
    return { ok: true, value: page([post(21), post(22)]) };
  };
  const el = mount(api);
  await wait();
  assert.equal(titles(el).length, 20);
  const more = button(el, "admin.list.more");
  more.click();
  await wait();
  assert.ok(more.disabled && more.getAttribute("aria-busy") === "true" && more.textContent === "admin.list.loading_more");
  release();
  await wait();
  assert.equal(api.queries[1].cursor, "next");
  assert.deepEqual(titles(el).slice(-3), ["Post 20", "Post 21", "Post 22"]);
  assert.equal(button(el, "admin.list.more"), undefined, "no next cursor: no button");
  assert.equal(document.activeElement?.querySelector(".row-title")?.textContent, "Post 21");
});

test("a failed Load more keeps the rows and offers a retry that appends", async () => {
  const api = fakeApi([page(Array.from({ length: 20 }, (_, i) => post(i + 1)), "next"), down, page([post(21)])]);
  const el = mount(api);
  await wait();
  button(el, "admin.list.more").click();
  await wait();
  assert.equal(titles(el).length, 20, "loaded rows stay");
  assert.ok(el.querySelector("[role=alert]"), "the plain error shows");
  button(el, "admin.state.retry").click();
  await wait();
  assert.equal(api.queries[2].cursor, "next");
  assert.equal(titles(el).length, 21);
  assert.equal(el.querySelector("[role=alert]"), null);
});

test("a failed first load shows the error with a retry, and empty shows the empty state", async () => {
  const api = fakeApi([down, page([])]);
  const el = mount(api);
  await wait();
  assert.ok(el.querySelector("[role=alert]"));
  button(el, "admin.state.retry").click();
  await wait();
  assert.ok(el.textContent.includes("admin.posts.empty"));
  const filtered = mount(fakeApi([page([])]), { url: "/admin/?status=scheduled" });
  await wait();
  assert.ok(filtered.textContent.includes("admin.list.empty_scheduled_hint"));
});

test("the strip shows at most 3 kept drafts, only with All, and links to the editor", async () => {
  const entries = [
    { key: "1:new-aaaaaaaaaaaaaaaa", title: "", savedAt: Date.now() - 60000, newId: "aaaaaaaaaaaaaaaa" },
    { key: "1:post-7", title: "Seven", savedAt: Date.now() - 3600000, post: 7 },
    { key: "1:post-8", title: "Eight", savedAt: Date.now() - 7200000, post: 8 },
    { key: "1:post-9", title: "Nine", savedAt: Date.now() - 9000000, post: 9 },
  ];
  const el = mount(fakeApi(() => page([post(1)])), { drafts: async () => entries });
  await wait();
  const strip = el.querySelector(".device");
  const rows = [...strip.querySelectorAll(".row")];
  assert.equal(rows.length, 3);
  assert.deepEqual(rows.map((r) => r.querySelector(".row-title").textContent), ["admin.posts.untitled", "Seven", "Eight"]);
  assert.deepEqual(rows.map((r) => r.getAttribute("href")), ["/admin/posts/new?draft=aaaaaaaaaaaaaaaa", "/admin/posts/7", "/admin/posts/8"]);
  assert.equal(strip.hidden, false);
  button(el, "admin.status.draft").click();
  await wait();
  assert.equal(strip.hidden, true, "only with the All filter");
  const none = mount(fakeApi(() => page([post(1)])));
  await wait();
  assert.equal(none.querySelector(".device").childElementCount, 0, "nothing drawn without drafts");
});

test("kept drafts: only this user's, only with something written, only keys the editor makes", () => {
  const d = (title, body = "") => ({ title, body, base: null, rev: 1, savedAt: 5 });
  assert.deepEqual(drafts.toEntry(1, "1:post-7", d("T")), { key: "1:post-7", title: "T", savedAt: 5, post: 7 });
  assert.deepEqual(drafts.toEntry(1, "1:new-abcdefghijkl", d("", "words")), { key: "1:new-abcdefghijkl", title: "", savedAt: 5, newId: "abcdefghijkl" });
  assert.equal(drafts.toEntry(1, "2:post-7", d("T")), undefined, "another user's");
  assert.equal(drafts.toEntry(1, "12:post-7", d("T")), undefined, "user 12 is not user 1");
  assert.equal(drafts.toEntry(1, "1:post-7", d(" ", "\n")), undefined, "nothing written");
  assert.equal(drafts.toEntry(1, "1:new-shot-a", d("T")), undefined, "not an id the editor makes");
  assert.equal(drafts.toEntry(1, "1:post-7", { title: 1 }), undefined, "not a draft");
  assert.deepEqual(drafts.toEntry(1, "1:post-7~abcdefgh12345678", d("T")), { key: "1:post-7~abcdefgh12345678", title: "T", savedAt: 5, post: 7 }, "a tab's side copy names its post");
  assert.equal(drafts.toEntry(1, "1:post-7~", d("T")), undefined, "not a side key the editor makes");
});

test("kept drafts: a post's draft and its side copies from other tabs are one row, the newest", () => {
  const e = (key, savedAt, more) => ({ key, title: key, savedAt, ...more });
  const out = drafts.onePerPost([
    e("1:post-7", 5, { post: 7 }),
    e("1:post-7~aaaaaaaaaaaaaaaa", 9, { post: 7 }),
    e("1:new-abcdefghijkl~bbbbbbbbbbbbbbbb", 3, { newId: "abcdefghijkl" }),
    e("1:post-8", 7, { post: 8 }),
  ]);
  assert.deepEqual(out.map((x) => x.key), ["1:post-7~aaaaaaaaaaaaaaaa", "1:post-8", "1:new-abcdefghijkl~bbbbbbbbbbbbbbbb"]);
});

test("pages: kind=page, no filters, no New page button", async () => {
  const api = fakeApi(() => page([post(4, "published", "page"), post(5, "trashed", "page")]));
  history.replaceState(null, "", "/admin/pages");
  document.body.replaceChildren(lists.pagesList(api));
  await wait();
  assert.equal(api.queries[0].kind, "page");
  assert.equal(document.querySelector(".filters"), null);
  assert.deepEqual(titles(document.body), ["Post 4"]);
  const none = fakeApi(() => page([]));
  document.body.replaceChildren(lists.pagesList(none));
  await wait();
  assert.ok(document.body.textContent.includes("admin.pages.empty"));
  assert.equal(document.querySelector("a.btn"), null);
});

test("ago: minutes, hours, days, then the date", () => {
  const now = Date.UTC(2026, 10, 28, 12);
  assert.equal(lists.ago(now - 5 * 60000, now), "5 minutes ago");
  assert.equal(lists.ago(now - 3 * 3600000, now), "3 hours ago");
  assert.equal(lists.ago(now - 2 * 86400000, now), "2 days ago");
  assert.match(lists.ago(now - 90 * 86400000, now), /2026/);
});

test("a boot prefetch serves the list's first request once, and only when it asked the same", async () => {
  const api = fakeApi((q) => page([post(q.status === "draft" ? 7 : 1, q.status ?? "published")]));
  lists.prefetch(api, "post", "?status=draft");
  assert.equal(api.queries.length, 1, "the prefetch asks at once");
  const el = mount(api, { url: "/admin/?status=draft" });
  await wait();
  assert.equal(api.queries.length, 1, "the list used the prefetched page");
  assert.deepEqual(titles(el), ["Post 7"]);
  // Used once: the next load asks again.
  button(el, "admin.status.draft").click();
  await wait();
  assert.equal(api.queries.length, 2);
  // A prefetch for another filter is not used for this one.
  lists.prefetch(api, "post", "");
  mount(api, { url: "/admin/?status=draft" });
  await wait();
  assert.equal(api.queries.length, 4);
  assert.equal(api.queries[3].status, "draft");
});

/** A fake Api for the Trashed filter: `restore` answers each POST (and can be held), requests are recorded. */
const trashApi = (rows, restore) => {
  const api = fakeApi(() => page(rows));
  api.calls = [];
  api.call = async (method, path) => {
    api.calls.push(`${method} ${path}`);
    return restore(path);
  };
  return api;
};
const restoreButtons = (el) => [...el.querySelectorAll(".rows button")].filter((b) => b.textContent === "admin.list.restore");

test("Trashed rows have Restore; one click sends one request, removes the row and confirms", async () => {
  let release;
  const held = new Promise((r) => (release = r));
  const api = trashApi([post(1, "trashed"), post(2, "trashed")], async () => (await held, { ok: true, value: post(1, "draft") }));
  const el = mount(api, { url: "/admin/?status=trashed" });
  await wait();
  assert.equal(restoreButtons(el).length, 2);
  assert.ok(!el.querySelector(".row button"), "the button is beside the row link, not inside it");
  const [first] = restoreButtons(el);
  first.click();
  first.click();
  await wait();
  assert.equal(first.disabled, true, "off while the request runs");
  release();
  await wait();
  assert.deepEqual(api.calls, ["POST /api/v1/posts/1/restore"]);
  assert.deepEqual(titles(el), ["Post 2"]);
  assert.ok(document.querySelector(".toast")?.textContent.includes("admin.editor.pub.restored"));
  restoreButtons(el)[0].click();
  await wait();
  assert.ok(el.textContent.includes("admin.list.empty_status"), "the last restore leaves the empty state");
});

test("a failed Restore keeps the row, enables the button again and says so", async () => {
  document.querySelector(".toast")?.remove();
  const errors = [];
  const orig = console.error;
  console.error = (...a) => errors.push(a.join(" "));
  try {
    const api = trashApi([post(1, "trashed")], async () => ({ ok: false, error: { code: "post.not_found", status: 404 } }));
    const el = mount(api, { url: "/admin/?status=trashed" });
    await wait();
    const b = restoreButtons(el)[0];
    b.click();
    await wait();
    assert.deepEqual(titles(el), ["Post 1"]);
    assert.equal(b.disabled, false);
    assert.ok(document.querySelector(".toast")?.textContent.includes("admin.editor.pub.failed"));
    assert.ok(errors.some((e) => e.includes("post.not_found") && e.includes("404")), "status and code are logged");
    document.querySelector(".toast")?.remove();
    api.call = async () => ({ ok: false, error: { code: "network", status: 0 } });
    b.click();
    await wait();
    assert.ok(document.querySelector(".toast")?.textContent.includes("admin.state.network_title"), "offline reads as elsewhere in the list");
    assert.deepEqual(titles(el), ["Post 1"]);
  } finally {
    console.error = orig;
  }
});

test("other filters have no Restore", async () => {
  const el = mount(fakeApi(() => page([post(1, "draft")])), { url: "/admin/?status=draft" });
  await wait();
  assert.equal(restoreButtons(el).length, 0);
});
