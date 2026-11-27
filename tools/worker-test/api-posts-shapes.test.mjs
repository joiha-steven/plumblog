// The posts API answers in the shapes generated from crates/model (`SHAPES` in
// web/admin/src/api/model.gen.ts), and the admin's fixtures answer to the same table, so the
// mock and the real Worker cannot drift apart.
import assert from "node:assert/strict";
import { createHash, randomBytes } from "node:crypto";
import { readFileSync } from "node:fs";
import { after, before, test } from "node:test";

import * as gen from "../../web/admin/src/api/model.gen.ts";
import { checkShape } from "../shape-check.mjs";
import { startWorker } from "./harness.mjs";

const { SHAPES, ...rest } = gen;
const lists = Object.fromEntries(Object.entries(rest).filter(([, v]) => Array.isArray(v)));
const check = (name, value) => checkShape(SHAPES, lists, name, value);
const fixtures = JSON.parse(readFileSync(new URL("../../web/admin/fixtures/posts.json", import.meta.url), "utf8"));

const T = Date.now();
const session = randomBytes(32).toString("base64url");
let w;

before(async () => {
  const hash = createHash("sha256").update(session).digest("hex");
  w = await startWorker({
    seedSql: [
      `INSERT INTO users (id, login, slug, display_name, role, password_hash, disabled_at, created_at, updated_at) VALUES (1, 'olga', 'olga', 'Olga', 'owner', NULL, NULL, ${T}, ${T});`,
      `INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES ('${hash}', 1, ${T}, ${T + 86_400_000});`,
    ].join("\n"),
  });
});
after(async () => {
  await w?.stop();
});

async function api(method, path, body) {
  const headers = { origin: w.url, cookie: `__Host-plumblog_session=${session}` };
  if (body !== undefined) headers["content-type"] = "application/json";
  const res = await fetch(w.url + path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) });
  const text = await res.text();
  return { status: res.status, json: text ? JSON.parse(text) : null, text };
}
const shaped = (name, r, status) => {
  assert.equal(r.status, status, r.text);
  assert.deepEqual(check(name, r.json), [], r.text);
  return r.json;
};

test("fixtures answer to SHAPES.post, as the real API does", () => {
  fixtures.forEach((p, i) => assert.deepEqual(check("post", p), [], `fixture ${i + 1}`));
});

test("create, get, list (paged), update, trash: every answer matches its shape", async () => {
  const made = shaped("post", await api("POST", "/api/v1/posts", { lang: "en", slug: "shape-a", title: "A", markdown: "Bad $\\notacommand$ here." }), 201);
  assert.deepEqual(made.warnings, ["markdown.math_invalid"], "a non-empty warnings list is checked too");
  for (const slug of ["shape-b", "shape-c"]) shaped("post", await api("POST", "/api/v1/posts", { lang: "en", slug, title: slug, markdown: "x" }), 201);

  const got = shaped("post", await api("GET", `/api/v1/posts/${made.id}`), 200);
  // The fixtures stand in for these answers: same keys, same order.
  assert.deepEqual(Object.keys(got), Object.keys(fixtures[0]));

  const seen = [];
  let cursor = null;
  for (let i = 0; i < 10; i++) {
    const page = shaped("post_page", await api("GET", `/api/v1/posts?limit=1${cursor ? `&cursor=${encodeURIComponent(cursor)}` : ""}`), 200);
    assert.ok(page.posts.length <= 1);
    seen.push(...page.posts.map((p) => p.id));
    cursor = page.next_cursor;
    if (cursor === null) break;
  }
  assert.equal(cursor, null);
  assert.equal(new Set(seen).size, seen.length);
  assert.ok(seen.includes(made.id) && seen.length >= 3);

  const upd = shaped("post_saved", await api("PATCH", `/api/v1/posts/${made.id}`, { expected_version: made.version, title: "A2" }), 200);
  assert.equal(upd.version, made.version + 1);
  assert.deepEqual(check("post", upd), []);

  const stale = await api("PATCH", `/api/v1/posts/${made.id}`, { expected_version: made.version, title: "A3" });
  assert.equal(stale.status, 409, stale.text);
  assert.deepEqual(check("conflict", stale.json), [], stale.text);
  assert.deepEqual(stale.json, { error: "store.conflict", field: "expected_version", version: upd.version });

  const taken = await api("POST", "/api/v1/posts", { lang: "en", slug: "shape-b", title: "dup", markdown: "x" });
  assert.equal(taken.status, 409, taken.text);
  assert.equal(taken.json.field, "slug");
  assert.equal(taken.json.version, undefined);

  const trashed = shaped("post_saved", await api("POST", `/api/v1/posts/${made.id}/trash`), 200);
  assert.equal(trashed.status, "trashed");
});
