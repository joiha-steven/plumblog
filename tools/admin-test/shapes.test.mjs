// The shape checker, and that the admin's fixtures and mock answer to the generated SHAPES
// (the same table worker-test holds the real API to).
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { build } from "esbuild";
import * as gen from "../../web/admin/src/api/model.gen.ts";
import { checkShape } from "../shape-check.mjs";

const root = new URL("../../", import.meta.url);
const { SHAPES, ...rest } = gen;
const lists = Object.fromEntries(Object.entries(rest).filter(([, v]) => Array.isArray(v)));
const check = (name, value) => checkShape(SHAPES, lists, name, value);
const fixtures = JSON.parse(readFileSync(new URL("web/admin/fixtures/posts.json", root), "utf8"));

async function bundle(file) {
  const out = await build({
    entryPoints: [new URL(file, root).pathname],
    bundle: true,
    write: false,
    format: "esm",
    logLevel: "silent",
    loader: { ".txt": "text" },
  });
  return import(`data:text/javascript;base64,${Buffer.from(out.outputFiles[0].text).toString("base64")}`);
}

// --- the checker ---

const toy = { inner: { n: "integer" }, outer: { s: "string|null", l: "integer[]", c: "code:COLORS", i: "shape:inner", is: "shape:inner[]", b: "boolean", f: "number", j: "json" } };
const codes = { COLORS: ["red", "blue"] };
const good = () => ({ s: null, l: [1, 2], c: "red", i: { n: 1 }, is: [{ n: 2 }], b: true, f: 0.25, j: { light: "#fff" } });
const toyCheck = (v) => checkShape(toy, codes, "outer", v);

test("shape checker: a valid value passes", () => {
  assert.deepEqual(toyCheck(good()), []);
  assert.deepEqual(toyCheck({ ...good(), s: "x", l: [] , is: [] }), []);
  assert.deepEqual(toyCheck({ ...good(), f: 3, j: false }), []);
});

test("shape checker: each kind of problem is found", () => {
  const cases = [
    ["missing key", (v) => (delete v.b, v), /b: missing key/],
    ["extra key", (v) => ({ ...v, z: 1 }), /z: extra key/],
    ["key order", (v) => ({ b: v.b, s: v.s, l: v.l, c: v.c, i: v.i, is: v.is, f: v.f, j: v.j }), /key order differs/],
    ["number expected", (v) => ({ ...v, f: "1" }), /f: expected number/],
    ["json is not null", (v) => ({ ...v, j: null }), /j: expected a value, got null/],
    ["wrong type", (v) => ({ ...v, b: "yes" }), /b: expected boolean/],
    ["float is not integer", (v) => ({ ...v, l: [1.5] }), /l\[0\]: expected integer/],
    ["null where not allowed", (v) => ({ ...v, b: null }), /b: expected boolean, got null/],
    ["code not in list", (v) => ({ ...v, c: "green" }), /c: "green" is not in COLORS/],
    ["list expected", (v) => ({ ...v, l: 3 }), /l: expected a list/],
    ["nested shape", (v) => ({ ...v, i: { n: "1" } }), /outer\.i\.n: expected integer/],
    ["nested list item", (v) => ({ ...v, is: [{ n: 1 }, { n: null }] }), /outer\.is\[1\]\.n: expected integer/],
  ];
  for (const [what, mutate, re] of cases) {
    const problems = toyCheck(mutate(good()));
    assert.ok(problems.some((p) => re.test(p)), `${what}: ${problems.join("; ")}`);
  }
  assert.ok(checkShape(toy, codes, "outer", [1])[0].includes("expected an object"));
  assert.ok(checkShape(toy, codes, "nope", {})[0].includes("unknown shape"));
});

// --- fixtures ---

test("fixtures: every post matches SHAPES.post, ids 1..N", () => {
  fixtures.forEach((p, i) => {
    assert.deepEqual(check("post", p), [], `fixture ${i + 1}`);
    assert.equal(p.id, i + 1);
  });
  assert.ok(fixtures.some((p) => p.kind === "page") && fixtures.some((p) => p.status === "trashed"));
});

// --- the mock ---

const mock = await bundle("web/admin/src/api/mock.ts");
const api = (qs = "") => mock.mockApi(new URLSearchParams(qs));
const ok = (r) => (assert.ok(r.ok, JSON.stringify(r)), r.value);

test("mock: list, get and create answer in the real shapes", async () => {
  const a = api();
  const page = ok(await a.posts({}));
  assert.deepEqual(check("post_page", page), []);
  // A first page holds at most 20 (the screens' page size); the fixtures need a second one.
  assert.ok(fixtures.length > 20 && page.posts.length === 20 && page.next_cursor !== null, "the list pages");
  assert.deepEqual(check("post", ok(await a.post(1))), []);
  assert.ok(ok(await a.post(1)).markdown.length > 500, "the long post's body comes from post-1.txt");
  const made = ok(await a.createPost({ lang: "en", slug: "new-one", title: "New", markdown: "Hello $$ x" }));
  assert.deepEqual(check("post", made), []);
  assert.equal(made.version, 1);
  assert.deepEqual(made.warnings, ["markdown.math_invalid"]);
  const missing = await a.post(999);
  assert.ok(!missing.ok && missing.error.code === "post.not_found" && missing.error.status === 404);
});

test("mock: filters, and paging walks every post once", async () => {
  const a = api();
  const kinds = ok(await a.posts({ kind: "page" })).posts;
  assert.ok(kinds.length > 0 && kinds.every((p) => p.kind === "page"));
  assert.ok(ok(await a.posts({ status: "trashed" })).posts.every((p) => p.status === "trashed"));
  assert.ok(ok(await a.posts({ lang: "en" })).posts.every((p) => p.lang === "en"));
  const seen = [];
  let cursor;
  for (let i = 0; i < 20; i++) {
    const page = ok(await a.posts({ limit: 2, ...(cursor ? { cursor } : {}) }));
    assert.deepEqual(check("post_page", page), []);
    seen.push(...page.posts.map((p) => p.id));
    cursor = page.next_cursor;
    if (cursor === null) break;
  }
  assert.equal(cursor, null);
  assert.equal(new Set(seen).size, seen.length, "no duplicates");
  assert.deepEqual([...seen].sort((x, y) => x - y), fixtures.map((p) => p.id));
});

test("mock: a stale update is a conflict with the current version; a fresh one saves", async () => {
  const a = api();
  const stale = await a.updatePost(2, { expected_version: 1, title: "X" });
  assert.ok(!stale.ok);
  assert.equal(stale.error.code, "store.conflict");
  assert.equal(stale.error.status, 409);
  assert.equal(stale.error.field, "expected_version");
  assert.equal(stale.error.version, fixtures[1].version);
  assert.deepEqual(check("conflict", { error: stale.error.code, field: stale.error.field, version: stale.error.version }), []);
  const saved = ok(await a.updatePost(2, { expected_version: fixtures[1].version, title: "Y" }));
  assert.deepEqual(check("post", saved), []);
  assert.equal(saved.title, "Y");
  assert.equal(saved.version, fixtures[1].version + 1);
  const pub = ok(await a.updatePost(3, { expected_version: fixtures[2].version, status: "published", published_at: null }));
  assert.ok(pub.published_at > 0);
  const taken = await a.updatePost(1, { expected_version: 7, slug: "about", lang: "en" });
  assert.ok(!taken.ok && taken.error.field === "slug" && taken.error.version === undefined);
});

test("mock: ?conflict=1 makes the first update conflict only", async () => {
  const a = api("conflict=1");
  const first = await a.updatePost(2, { expected_version: fixtures[1].version, title: "X" });
  assert.ok(!first.ok && first.error.field === "expected_version" && first.error.version === fixtures[1].version + 1);
  ok(await a.updatePost(2, { expected_version: first.error.version, title: "X" }));
});

// --- people and invitations ---

const people = JSON.parse(readFileSync(new URL("web/admin/fixtures/users.json", root), "utf8"));
const call = (a, method, path, body) => a.call(method, path, body);

test("fixtures: every person matches SHAPES.user, ids 1..N, with owners, writers and a disabled one", () => {
  people.users.forEach((u, i) => {
    assert.deepEqual(check("user", u), [], `user ${i + 1}`);
    assert.equal(u.id, i + 1);
  });
  assert.ok(people.users.some((u) => u.role === "owner") && people.users.some((u) => u.disabled_at !== null));
  assert.ok(people.invites.some((i) => i.expires_in_days < 0), "one invitation has expired");
});

test("mock: people and invitations answer in the real shapes; paging is by id", async () => {
  const a = api();
  assert.deepEqual(check("user_page", ok(await call(a, "GET", "/api/v1/users"))), []);
  assert.deepEqual(check("invite_list", ok(await call(a, "GET", "/api/v1/invites"))), []);
  const first = ok(await call(a, "GET", "/api/v1/users?limit=3"));
  assert.equal(first.users.length, 3);
  assert.equal(first.next_cursor, "3");
  const rest = ok(await call(a, "GET", `/api/v1/users?cursor=${first.next_cursor}&limit=50`));
  assert.equal(rest.next_cursor, null);
  assert.equal(first.users.length + rest.users.length, people.users.length);
  const made = ok(await call(a, "POST", "/api/v1/invites", { role: "writer", expires_at: Date.now() + 86_400_000 }));
  assert.deepEqual(check("invite_created", made), []);
  assert.equal((await call(a, "POST", "/api/v1/invites", { role: "root" })).ok, false);
  assert.equal(ok(await call(a, "DELETE", `/api/v1/invites/${made.id}`)), undefined);
  const gone = await call(a, "DELETE", `/api/v1/invites/${made.id}`);
  assert.ok(!gone.ok && gone.error.status === 404);
});

test("mock: role, disable and delete follow the server's rules", async () => {
  const a = api();
  assert.deepEqual(check("user", ok(await call(a, "PUT", "/api/v1/users/3/role", { role: "owner" }))), []);
  assert.equal(ok(await call(a, "PUT", "/api/v1/users/3/disabled", { disabled: true })).disabled_at > 0, true);
  assert.equal(ok(await call(a, "PUT", "/api/v1/users/3/disabled", { disabled: false })).disabled_at, null);
  // Two active owners can become one; the last cannot go.
  ok(await call(a, "PUT", "/api/v1/users/3/role", { role: "writer" }));
  ok(await call(a, "PUT", "/api/v1/users/2/role", { role: "writer" }));
  const last = await call(a, "PUT", "/api/v1/users/1/role", { role: "writer" });
  assert.ok(!last.ok && last.error.code === "user.last_owner" && last.error.status === 409);
  assert.equal((await call(a, "DELETE", "/api/v1/users/1")).error.code, "user.last_owner");
  assert.equal((await call(a, "DELETE", "/api/v1/users/1")).error.code, "user.last_owner");
  assert.equal(ok(await call(a, "DELETE", "/api/v1/users/4")), undefined);
  assert.equal((await call(a, "DELETE", "/api/v1/users/4")).error.status, 404);
  // Demoting an owner drops their invitations.
  const b = api();
  ok(await call(b, "PUT", "/api/v1/users/2/role", { role: "writer" }));
  assert.ok(!ok(await call(b, "GET", "/api/v1/invites")).invites.some((i) => i.created_by === 2));
});

test("mock: ?users= forces each refusal, ?mock=empty leaves only you, a writer gets 403", async () => {
  const code = async (qs, method, path, body) => {
    const r = await call(api(qs), method, path, body);
    return r.ok ? "ok" : `${r.error.code} ${r.error.status}`;
  };
  assert.equal(await code("users=last_owner", "PUT", "/api/v1/users/2/role", { role: "writer" }), "user.last_owner 409");
  assert.equal(await code("users=posts", "DELETE", "/api/v1/users/2"), "store.conflict 409");
  assert.equal(await code("users=gone", "PUT", "/api/v1/users/2/disabled", { disabled: true }), "user.not_found 404");
  assert.equal(await code("users=forbidden", "DELETE", "/api/v1/users/2"), "auth.forbidden 403");
  assert.equal(await code("users=error", "GET", "/api/v1/users"), "internal 500");
  assert.equal(await code("invites=error", "GET", "/api/v1/invites"), "internal 500");
  assert.equal(await code("role=writer", "GET", "/api/v1/users"), "auth.forbidden 403");
  const empty = api("mock=empty");
  assert.equal(ok(await call(empty, "GET", "/api/v1/users")).users.length, 1);
  assert.deepEqual(ok(await call(empty, "GET", "/api/v1/invites")).invites, []);
});

// --- Appearance and Plugins fixtures ---

test("fixtures: theme and plugin declarations match SHAPES.option_declarations and cover every field type", async () => {
  const theme = JSON.parse(readFileSync(new URL("web/admin/fixtures/theme-declarations.json", root), "utf8"));
  const plugins = JSON.parse(readFileSync(new URL("web/admin/fixtures/plugins.json", root), "utf8"));
  assert.deepEqual(check("theme_list", { themes: theme.themes }), []);
  const all = [theme.declarations, ...Object.values(plugins.declarations)].flatMap((d) => [d.en, d.vi]);
  for (const d of all) assert.deepEqual(check("option_declarations", d), [], d.name);
  const types = new Set(all.flatMap((d) => d.options.map((o) => o.type)));
  assert.deepEqual([...types].sort(), [...gen.OPTION_TYPES].sort(), "every field type appears in the fixtures");
  // Same keys in both languages, so a screen never differs by language.
  for (const d of [theme.declarations, ...Object.values(plugins.declarations)]) {
    assert.deepEqual(d.en.options.map((o) => o.key), d.vi.options.map((o) => o.key));
  }
  assert.deepEqual(Object.keys(plugins.declarations).sort(), plugins.plugins.map((p) => p.name).sort());
});

test("mock: themes, declarations in the asked language, options and plugin switches", async () => {
  const a = (q = "") => api(q);
  const list = ok(await a().call("GET", "/api/v1/themes"));
  assert.deepEqual(check("theme_list", list), []);
  const vi = ok(await a().call("GET", "/api/v1/themes/minimal/declarations?lang=vi"));
  assert.deepEqual(check("option_declarations", vi), []);
  assert.equal(vi.lang, "vi");
  const en = ok(await a().call("GET", "/api/v1/themes/minimal/declarations?lang=en"));
  assert.equal(en.lang, "en");
  const mock = a();
  assert.equal(ok(await mock.call("GET", "/api/v1/themes/minimal/options")).options.excerpt_words, 40);
  ok(await mock.call("PUT", "/api/v1/themes/minimal/options/excerpt_words", { value: 10 }));
  assert.equal(ok(await mock.call("GET", "/api/v1/themes/minimal/options")).options.excerpt_words, 10);
  ok(await mock.call("DELETE", "/api/v1/themes/minimal/options"));
  assert.deepEqual(ok(await mock.call("GET", "/api/v1/themes/minimal/options")).options, {});
  const plugins = ok(await mock.call("GET", "/api/v1/plugins")).plugins;
  assert.equal(plugins.length, 2);
  ok(await mock.call("PUT", "/api/v1/plugins/reading-progress", { enabled: true }));
  const now = ok(await mock.call("GET", "/api/v1/plugins")).plugins.find((p) => p.name === "reading-progress");
  assert.deepEqual([now.enabled, now.disabled_reason], [true, null]);
  assert.deepEqual(check("option_declarations", ok(await mock.call("GET", "/api/v1/plugins/newsletter/declarations?lang=en"))), []);
  // The forced states.
  const bad = await a("options=unavailable").call("GET", "/api/v1/themes/minimal/declarations?lang=en");
  assert.ok(!bad.ok && bad.error.code === "theme.unavailable" && bad.error.status === 409);
  assert.ok(ok(await a("options=unavailable").call("GET", "/api/v1/themes")).themes.some((t) => !t.available));
  assert.deepEqual(ok(await a("mock=empty").call("GET", "/api/v1/plugins")).plugins, []);
  assert.deepEqual(ok(await a("options=bare").call("GET", "/api/v1/themes/minimal/declarations?lang=en")).options, []);
  const refused = await a("options=invalid").call("PUT", "/api/v1/themes/minimal/options/excerpt_words", { value: 10 });
  assert.ok(!refused.ok && refused.error.code === "input.invalid");
  const writer = await a("role=writer").call("PUT", "/api/v1/plugins/newsletter", { enabled: false });
  assert.ok(!writer.ok && writer.error.code === "auth.forbidden");
});
