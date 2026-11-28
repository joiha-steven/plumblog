// Retries after a lost answer (issue #86), over HTTP on real D1: a create repeated with its
// `client_id` returns the post the first one made (per user, also when two race), and a
// PATCH from a stale version that the stored post already equals is a 200, not a 409.
import assert from "node:assert/strict";
import { createHash, randomBytes, randomUUID } from "node:crypto";
import { after, before, test } from "node:test";

import { startWorker } from "./harness.mjs";

const DAY = 86_400_000;
const T = Date.now();
const owner = randomBytes(32).toString("base64url");
const writer = randomBytes(32).toString("base64url");
const sha = (s) => createHash("sha256").update(s).digest("hex");
const AS = { owner: { cookie: `__Host-plumblog_session=${owner}` }, writer: { cookie: `__Host-plumblog_session=${writer}` } };
let w;

before(async () => {
  w = await startWorker({
    seedSql: [
      `INSERT INTO users (id, login, slug, display_name, role, password_hash, disabled_at, created_at, updated_at) VALUES (1, 'olga', 'olga', 'Olga', 'owner', NULL, NULL, ${T}, ${T}), (2, 'will', 'will', 'Will', 'writer', NULL, NULL, ${T}, ${T});`,
      `INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES ('${sha(owner)}', 1, ${T}, ${T + DAY}), ('${sha(writer)}', 2, ${T}, ${T + DAY});`,
    ].join("\n"),
  });
});
after(async () => {
  await w?.stop();
});

async function api(method, path, body, as = AS.writer) {
  const h = { origin: w.url, ...as };
  if (body !== undefined) h["content-type"] = "application/json";
  const res = await fetch(w.url + path, { method, headers: h, body: body === undefined ? undefined : JSON.stringify(body) });
  const text = await res.text();
  return { status: res.status, json: text ? JSON.parse(text) : null, text };
}
const newPost = (slug, extra = {}) => ({ lang: "en", slug, title: `Title ${slug}`, markdown: `Body of ${slug}.`, ...extra });
const count = (sql) => w.query(sql)[0].n;
const creates = () => count("SELECT COUNT(*) AS n FROM system_log WHERE kind = 'content.changed' AND detail LIKE '%action\tpost.create%'");

test("a create repeated with its client_id returns the first post and makes nothing", async () => {
  const key = randomUUID();
  const first = await api("POST", "/api/v1/posts", newPost("retry", { client_id: key }));
  assert.equal(first.status, 201, first.text);
  const logged = creates();
  // The answer was lost; the client retries the same body, then one edited meanwhile.
  const again = await api("POST", "/api/v1/posts", newPost("retry", { client_id: key }));
  assert.equal(again.status, 201, again.text);
  assert.deepEqual(again.json, first.json, "the same answer as the first create");
  const edited = await api("POST", "/api/v1/posts", newPost("retry-2", { client_id: key, title: "Edited", markdown: "$$ x" }));
  assert.equal(edited.status, 201, edited.text);
  assert.equal(edited.json.id, first.json.id);
  assert.equal(edited.json.title, first.json.title, "the stored post, not the new body");
  assert.equal(count(`SELECT COUNT(*) AS n FROM posts WHERE slug LIKE 'retry%'`), 1);
  assert.equal(creates(), logged, "a replay logs no second create");

  // Another user's same key is their own request.
  const theirs = await api("POST", "/api/v1/posts", newPost("theirs", { client_id: key }), AS.owner);
  assert.equal(theirs.status, 201, theirs.text);
  assert.notEqual(theirs.json.id, first.json.id);
  assert.equal(theirs.json.author_id, 1);
  // The writer's own key still answers the writer's post.
  const mine = await api("POST", "/api/v1/posts", newPost("retry", { client_id: key }));
  assert.equal(mine.json.id, first.json.id);

  // Without a key, a second create is a second post (the slug is taken).
  assert.equal((await api("POST", "/api/v1/posts", newPost("plain"))).status, 201);
  const dup = await api("POST", "/api/v1/posts", newPost("plain"));
  assert.equal(dup.status, 409, dup.text);
  assert.equal(dup.json.field, "slug");
});

test("client_id is checked: 1-64 of A-Z a-z 0-9 - _", async () => {
  for (const bad of ["", "a".repeat(65), "has space", "slash/no", "\u00e9t\u00e9"]) {
    const r = await api("POST", "/api/v1/posts", newPost(`bad-${bad.length}`, { client_id: bad }));
    assert.equal(r.status, 400, `${JSON.stringify(bad)}: ${r.text}`);
    assert.deepEqual([r.json.error, r.json.field], ["input.invalid", "client_id"]);
  }
  assert.equal((await api("POST", "/api/v1/posts", newPost("long-key", { client_id: "A_z-9".repeat(12) + "abcd" }))).status, 201);
  assert.equal(count(`SELECT COUNT(*) AS n FROM posts WHERE slug LIKE 'bad-%'`), 0);
});

test("two creates with one client_id at once make one post", async () => {
  for (const [a, b] of [["race", "race"], ["left", "right"]]) {
    const key = randomUUID();
    const logged = creates();
    const both = await Promise.all([a, b].map((slug) => api("POST", "/api/v1/posts", newPost(slug, { client_id: key }))));
    assert.deepEqual(both.map((r) => r.status), [201, 201], both.map((r) => r.text).join(" | "));
    assert.equal(both[0].json.id, both[1].json.id);
    assert.equal(count(`SELECT COUNT(*) AS n FROM posts WHERE client_id = '${key}'`), 1);
    assert.equal(creates(), logged + 1, "only the insert that won logs post.create");
  }
});

test("a stale PATCH the stored post already equals is a 200; another writer's change is a 409", async () => {
  const c = await api("POST", "/api/v1/posts", newPost("patched"));
  const id = c.json.id;
  const body = { expected_version: 1, title: "Saved once", markdown: "Saved *once*.", tags: [] };
  const u = await api("PATCH", `/api/v1/posts/${id}`, body);
  assert.equal(u.status, 200, u.text);
  const revisions = count(`SELECT COUNT(*) AS n FROM revisions WHERE post_id = ${id}`);
  // The answer was lost: the same PATCH again, from version 1.
  const again = await api("PATCH", `/api/v1/posts/${id}`, body);
  assert.equal(again.status, 200, again.text);
  assert.deepEqual({ ...again.json, warnings: [] }, { ...u.json, warnings: [] }, "the stored post, unchanged");
  assert.equal(again.json.version, 2);
  assert.equal(count(`SELECT COUNT(*) AS n FROM revisions WHERE post_id = ${id}`), revisions, "nothing written");
  // A subset that matches is a match too (fields not sent are the stored ones).
  assert.equal((await api("PATCH", `/api/v1/posts/${id}`, { expected_version: 1, title: "Saved once" })).status, 200);

  for (const other of [{ title: "Someone else" }, { markdown: "Other text." }, { pinned: true }, { slug: "moved" }]) {
    const r = await api("PATCH", `/api/v1/posts/${id}`, { ...body, ...other });
    assert.equal(r.status, 409, `${JSON.stringify(other)}: ${r.text}`);
    assert.deepEqual(r.json, { error: "store.conflict", field: "expected_version", version: 2 });
  }
  const now = await api("GET", `/api/v1/posts/${id}`);
  assert.equal(now.json.version, 2);
  assert.equal(now.json.title, "Saved once");
});
