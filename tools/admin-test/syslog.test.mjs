// The System log screen (web/admin/src/syslog/): filters and their URL, the query they make,
// the mock's filtering and paging, labels for every kind and error code in both languages, and
// the screen itself driven through the real functions with a fake Api in a stand-in DOM.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { build } from "esbuild";
import { Window } from "happy-dom";
import * as gen from "../../web/admin/src/api/model.gen.ts";
import { checkShape } from "../shape-check.mjs";

const win = new Window({ url: "http://localhost/admin/settings/log" });
for (const k of ["document", "location", "history", "Node", "HTMLElement"]) {
  Object.defineProperty(globalThis, k, { value: win[k], configurable: true, writable: true });
}

const root = new URL("../../", import.meta.url);
const { SHAPES, ...rest } = gen;
const lists = Object.fromEntries(Object.entries(rest).filter(([, v]) => Array.isArray(v)));
const check = (name, value) => checkShape(SHAPES, lists, name, value);
const fixtures = JSON.parse(readFileSync(new URL("web/admin/fixtures/log.json", root), "utf8"));

async function bundle(file) {
  const out = await build({ entryPoints: [new URL(file, root).pathname], bundle: true, write: false, format: "esm", logLevel: "silent", loader: { ".txt": "text" }, define: { MOCK: "true" } });
  return import(`data:text/javascript;base64,${Buffer.from(out.outputFiles[0].text).toString("base64")}`);
}
const query = await bundle("web/admin/src/syslog/query.ts");
const mock = await bundle("web/admin/src/api/mock.ts");
const screen = await bundle("web/admin/src/syslog/screen.ts");

const ok = (r) => (assert.ok(r.ok, JSON.stringify(r)), r.value);
const api = (qs = "") => mock.mockApi(new URLSearchParams(qs));
const wait = async () => {
  for (let i = 0; i < 12; i++) await new Promise((r) => setTimeout(r, 20));
};

// --- locale text ---

/** Flat `section.key = "text"` reader, enough for the admin keys. */
const locale = (lang) => {
  const out = {};
  let section = "";
  for (const line of readFileSync(new URL(`locales/${lang}.toml`, root), "utf8").split("\n")) {
    const s = /^\[(.+)\]$/.exec(line);
    if (s) section = s[1];
    const kv = /^(\w+) = "((?:[^"\\]|\\.)*)"$/.exec(line);
    if (kv) out[`${section}.${kv[1]}`] = kv[2];
  }
  return out;
};

test("every log kind and error code has a plain label in English and Vietnamese", () => {
  for (const lang of ["en", "vi"]) {
    const text = locale(lang);
    for (const kind of gen.LOG_KINDS) assert.ok(text[`admin.syslog.kind.${kind.replaceAll(".", "_")}`], `${lang}: kind ${kind}`);
    for (const code of gen.ERROR_CODES) assert.ok(text[`admin.syslog.err.${code.replaceAll(".", "_")}`], `${lang}: error ${code}`);
    assert.ok(text["admin.syslog.redacted"], `${lang}: redacted`);
    for (const g of query.KIND_GROUPS) assert.ok(text[`admin.syslog.group_${g.id}`], `${lang}: group ${g.id}`);
    for (const r of query.RANGES) assert.ok(text[`admin.syslog.range_${r}`], `${lang}: range ${r}`);
    for (const l of query.LEVELS) assert.ok(text[`admin.syslog.level_${l}`], `${lang}: level ${l}`);
  }
});

test("the type select holds every kind exactly once", () => {
  const all = query.KIND_GROUPS.flatMap((g) => g.kinds);
  assert.deepEqual([...all].sort(), [...gen.LOG_KINDS].sort());
});

// --- filters and the URL ---

test("filters read from and write to the URL, and anything unknown means no filter", () => {
  assert.deepEqual(query.filtersOf(""), query.NO_FILTERS);
  const f = query.filtersOf("?level=error&kind=job.run&range=7d&q=%20hello%20");
  assert.deepEqual(f, { level: "error", kind: "job.run", range: "7d", q: "hello" });
  assert.equal(query.searchOf(f), "?level=error&kind=job.run&range=7d&q=hello");
  assert.equal(query.searchOf(query.NO_FILTERS), "");
  assert.deepEqual(query.filtersOf("?level=fatal&kind=nope&range=1y"), query.NO_FILTERS);
  assert.equal(query.filtersOf(`?q=${"x".repeat(150)}`).q.length, 100);
  for (const s of ["", "?level=warning", "?kind=error&range=24h", "?q=a%26b%3Dc"]) assert.equal(query.searchOf(query.filtersOf(s)), s);
  assert.ok(!query.isFiltered(query.NO_FILTERS) && query.isFiltered({ ...query.NO_FILTERS, q: "x" }));
});

test("filters make the API query: range is a window ending now, level all is no level", () => {
  const now = 1_800_000_000_000;
  assert.deepEqual(query.queryOf(query.NO_FILTERS, now), { limit: 50 });
  const q = query.queryOf({ level: "warning", kind: "auth.sign_in_failed", range: "24h", q: "lan" }, now, "abc");
  assert.deepEqual(q, { limit: 50, level: "warning", kind: "auth.sign_in_failed", from: now - 86_400_000, q: "lan", cursor: "abc" });
  assert.equal(query.queryOf({ ...query.NO_FILTERS, range: "7d" }, now).from, now - 7 * 86_400_000);
  assert.equal(query.queryOf({ ...query.NO_FILTERS, range: "30d" }, now).from, now - 30 * 86_400_000);
  assert.ok(!("to" in q), "the window ends now: no upper bound");
});

// --- fixtures and the mock ---

test("fixtures: every entry matches SHAPES.log_entry and the set covers the cases the screen shows", () => {
  fixtures.forEach((e, i) => assert.deepEqual(check("log_entry", e), [], `fixture ${i}`));
  assert.ok(fixtures.length >= 30);
  assert.equal(new Set(fixtures.map((e) => e.id)).size, fixtures.length, "unique ids");
  for (const level of gen.LOG_LEVELS) assert.ok(fixtures.some((e) => e.level === level), level);
  assert.ok(fixtures.some((e) => e.fields.some((f) => f.value === "{redacted}")), "a redacted value");
  assert.ok(fixtures.some((e) => e.actor_id === null) && fixtures.some((e) => e.actor_id !== null && e.actor_name === null), "system and deleted user");
  assert.ok(fixtures.some((e) => e.error_code !== null), "an error code");
  assert.ok(fixtures.some((e) => e.kind === "job.run" && e.fields.some((f) => f.key === "deleted")), "a retention run");
  assert.ok(fixtures.some((e) => e.subject.length > 150), "a very long subject");
  assert.ok(fixtures.some((e) => /[\u00c0-\u1ef9]/.test(e.subject)), "Vietnamese text");
});

test("mock: a page answers in the real shape, newest first, 50 at most", async () => {
  const page = ok(await api().logList({}));
  assert.deepEqual(check("log_page", page), []);
  assert.equal(page.next_cursor, null);
  assert.equal(page.entries.length, fixtures.length);
  for (let i = 1; i < page.entries.length; i++) assert.ok(page.entries[i - 1].at >= page.entries[i].at);
  assert.ok(Date.now() - page.entries[0].at < 3_600_000, "the newest entry is recent");
});

test("mock: each filter narrows the way the server does", async () => {
  const a = api();
  const entries = async (q) => ok(await a.logList(q)).entries;
  const warn = await entries({ level: "warning" });
  assert.ok(warn.length > 0 && warn.every((e) => e.level !== "info") && warn.some((e) => e.level === "error"), "level is a minimum");
  assert.ok((await entries({ level: "error" })).every((e) => e.level === "error"));
  assert.equal((await entries({ level: "info" })).length, fixtures.length, "info is no filter");
  assert.ok((await entries({ kind: "job.run" })).every((e) => e.kind === "job.run"));
  assert.ok((await entries({ code: "webhook.delivery_failed" })).every((e) => e.error_code === "webhook.delivery_failed"));
  assert.ok((await entries({ actor: 1 })).every((e) => e.actor_id === 1));
  const day = await entries({ from: Date.now() - 86_400_000 });
  assert.ok(day.length > 0 && day.length < fixtures.length);
  const all = await entries({});
  const cut = all[5].at;
  assert.ok((await entries({ to: cut })).every((e) => e.at < cut), "to is exclusive");
  assert.ok((await entries({ from: cut })).some((e) => e.at === cut), "from is inclusive");
  assert.ok((await entries({ q: "WEBHOOK" })).length > 0, "text, any case");
  assert.ok((await entries({ q: "backup script" })).length > 0, "text in a field");
  const one = all[3];
  assert.deepEqual((await entries({ q: one.request_id })).map((e) => e.id), [one.id], "an exact request id");
  assert.deepEqual(await entries({ q: "no such text anywhere" }), []);
});

test("mock: the cursor walks every entry once; bad parameters are refused", async () => {
  const a = api();
  const seen = [];
  let cursor;
  for (let i = 0; i < 20; i++) {
    const page = ok(await a.logList({ limit: 7, ...(cursor ? { cursor } : {}) }));
    assert.deepEqual(check("log_page", page), []);
    seen.push(...page.entries.map((e) => e.id));
    cursor = page.next_cursor;
    if (cursor === null) break;
  }
  assert.equal(cursor, null);
  assert.equal(new Set(seen).size, seen.length, "no duplicates");
  assert.equal(seen.length, fixtures.length);
  for (const [q, field] of [[{ q: "" }, "q"], [{ q: "x".repeat(101) }, "q"], [{ limit: 0 }, "limit"], [{ limit: 101 }, "limit"], [{ cursor: "!!" }, "cursor"]]) {
    const r = await a.logList(q);
    assert.ok(!r.ok && r.error.code === "input.invalid" && r.error.field === field, JSON.stringify(q));
  }
});

test("mock: ?mock= states and a writer", async () => {
  assert.deepEqual(ok(await api("mock=empty").logList({})), { entries: [], next_cursor: null });
  const bad = await api("mock=error").logList({});
  assert.ok(!bad.ok && bad.error.code === "internal");
  const off = await api("mock=offline").logList({});
  assert.ok(!off.ok && off.error.code === "network");
  assert.deepEqual(ok(await api("log=empty").logList({})), { entries: [], next_cursor: null });
  const logOff = await api("log=offline").logList({});
  assert.ok(!logOff.ok && logOff.error.code === "network");
  const logBad = await api("log=error").logList({});
  assert.ok(!logBad.ok && logBad.error.code === "internal");
  const writer = await api("role=writer").logList({});
  assert.ok(!writer.ok && writer.error.code === "auth.forbidden" && writer.error.status === 403);
});

// --- the screen ---

const entry = (id, at, extra = {}) => ({ id, at, request_id: `req-${id}`, kind: "auth.sign_in", level: "info", actor_id: 1, actor_name: "Lan", error_code: null, subject: `Entry ${id}`, fields: [], ...extra });
const fakeApi = (answers) => {
  const queries = [];
  return {
    queries,
    logList: async (q) => {
      queries.push(q);
      const a = typeof answers === "function" ? answers(q, queries.length - 1) : answers[queries.length - 1];
      return a.ok === false ? a : { ok: true, value: a };
    },
  };
};
const down = { ok: false, error: { code: "network", status: 0 } };

function mount(a, url = "/admin/settings/log") {
  history.replaceState(null, "", url);
  document.body.replaceChildren();
  const find = document.createElement("div");
  const body = document.createElement("div");
  document.body.append(find, body);
  screen.mountSyslog({ find, body, api: a });
  return { find, body };
}
const button = (el, text) => [...el.querySelectorAll("button")].find((b) => b.textContent === text);

test("screen: filters from the URL go to the API; rows group under day headings, newest first", async () => {
  const now = Date.now();
  const a = fakeApi([{ entries: [entry(3, now), entry(2, now - 60_000), entry(1, now - 3 * 86_400_000)], next_cursor: null }]);
  const { body } = mount(a, "/admin/settings/log?level=warning&kind=job.run&range=24h&q=lan");
  await wait();
  assert.equal(a.queries.length, 1);
  assert.equal(a.queries[0].level, "warning");
  assert.equal(a.queries[0].kind, "job.run");
  assert.equal(a.queries[0].q, "lan");
  assert.ok(Math.abs(a.queries[0].from - (now - 86_400_000)) < 5000);
  assert.equal(body.querySelectorAll(".log-day").length, 2, "today and three days ago");
  assert.deepEqual([...body.querySelectorAll(".log-subject")].map((x) => x.textContent), ["Entry 3", "Entry 2", "Entry 1"]);
  assert.equal(body.querySelector("select").value, "job.run");
  assert.equal([...body.querySelectorAll(".seg button")].find((b) => b.getAttribute("aria-pressed") === "true").textContent, "admin.syslog.level_warning");
});

test("screen: a row opens in place with its request id; the marker for a removed value shows as words", async () => {
  const a = fakeApi([{ entries: [entry(1, Date.now(), { fields: [{ key: "token", value: "{redacted}" }, { key: "name", value: "ci" }], actor_id: 9, actor_name: null })], next_cursor: null }]);
  const { body } = mount(a);
  await wait();
  const row = body.querySelector(".log-btn");
  const region = body.querySelector(".log-detail");
  assert.equal(row.getAttribute("aria-expanded"), "false");
  assert.ok(region.hidden);
  assert.equal(row.getAttribute("aria-controls"), region.id);
  assert.ok(body.querySelector(".log-who").textContent === "admin.syslog.who_deleted", "a deleted user");
  row.click();
  assert.equal(row.getAttribute("aria-expanded"), "true");
  assert.ok(!region.hidden);
  assert.ok(region.textContent.includes("req-1"));
  assert.ok(region.textContent.includes("admin.syslog.redacted") && !region.textContent.includes("{redacted}"));
  assert.ok(button(region, "admin.syslog.copy_ai") && button(region, "admin.syslog.copy"));
  row.click();
  assert.equal(row.getAttribute("aria-expanded"), "false");
  assert.ok(region.hidden);
});

test("screen: Load more keeps the rows and asks with the cursor", async () => {
  const now = Date.now();
  const a = fakeApi([{ entries: [entry(2, now)], next_cursor: "c1" }, { entries: [entry(1, now - 1000)], next_cursor: null }]);
  const { body } = mount(a);
  await wait();
  button(body, "admin.list.more").click();
  await wait();
  assert.equal(a.queries[1].cursor, "c1");
  assert.equal(body.querySelectorAll(".log-btn").length, 2);
  assert.equal(body.querySelectorAll(".log-day").length, 1, "the same day stays one group");
  assert.ok(!button(body, "admin.list.more"), "no cursor, no button");
});

test("screen: empty log, no results with Clear filters, and failures with retry", async () => {
  const empty = mount(fakeApi([{ entries: [], next_cursor: null }]));
  await wait();
  assert.ok(empty.body.textContent.includes("admin.syslog.empty_title"));

  const a = fakeApi((q) => (q.level === "error" ? { entries: [], next_cursor: null } : { entries: [entry(1, Date.now())], next_cursor: null }));
  const { body } = mount(a, "/admin/settings/log?level=error");
  await wait();
  assert.ok(body.textContent.includes("admin.syslog.none_title"));
  button(body, "admin.syslog.clear").click();
  await wait();
  assert.equal(location.search, "");
  assert.equal(body.querySelectorAll(".log-btn").length, 1, "cleared: the entry shows");
  assert.equal(a.queries.at(-1).level, undefined);

  const bad = fakeApi([down, { entries: [entry(1, Date.now())], next_cursor: null }]);
  const failed = mount(bad);
  await wait();
  assert.ok(failed.body.querySelector("[role=alert]"), "the network failure state");
  button(failed.body, "admin.state.retry").click();
  await wait();
  assert.equal(failed.body.querySelectorAll(".log-btn").length, 1);
});

test("screen: picking a level or a type asks again and moves the URL", async () => {
  const a = fakeApi(() => ({ entries: [], next_cursor: null }));
  const { body } = mount(a);
  await wait();
  button(body, "admin.syslog.level_error").click();
  await wait();
  assert.equal(location.search, "?level=error");
  assert.equal(a.queries.at(-1).level, "error");
  const select = body.querySelectorAll("select")[1];
  select.value = "7d";
  select.dispatchEvent(new win.Event("change"));
  await wait();
  assert.equal(location.search, "?level=error&range=7d");
  assert.ok(a.queries.at(-1).from > 0);
});

test("screen: every page of a list asks with the same clock", async () => {
  const now = Date.now();
  const a = fakeApi([{ entries: [entry(2, now)], next_cursor: "c1" }, { entries: [entry(1, now - 1000)], next_cursor: null }]);
  const { body } = mount(a, "/admin/settings/log?range=24h");
  await wait();
  await new Promise((r) => setTimeout(r, 30));
  button(body, "admin.list.more").click();
  await wait();
  assert.equal(a.queries.length, 2);
  assert.equal(a.queries[1].from, a.queries[0].from);
});

test("screen: Clear filters moves focus to the search field", async () => {
  const a = fakeApi((q) => (q.level === "error" ? { entries: [], next_cursor: null } : { entries: [entry(1, Date.now())], next_cursor: null }));
  const { find, body } = mount(a, "/admin/settings/log?level=error");
  await wait();
  button(body, "admin.syslog.clear").click();
  await wait();
  assert.equal(document.activeElement, find.querySelector("input"));
});

test("screen: a filter change takes the typed text with it and cancels the pending search", async () => {
  const a = fakeApi(() => ({ entries: [], next_cursor: null }));
  const { find, body } = mount(a);
  await wait();
  const input = find.querySelector("input");
  input.value = "lan";
  input.dispatchEvent(new win.Event("input"));
  const kind = body.querySelectorAll("select")[0];
  kind.value = "job.run";
  kind.dispatchEvent(new win.Event("change"));
  const entries = history.length;
  await new Promise((r) => setTimeout(r, 450));
  assert.equal(location.search, "?kind=job.run&q=lan");
  assert.equal(history.length, entries, "the late timer adds and replaces nothing");
  assert.equal(a.queries.length, 2);
  assert.equal(a.queries.at(-1).q, "lan");
});
