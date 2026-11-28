// GET /api/v1/tags and /api/v1/series page by slug with a keyset cursor, like the post list.
// 101 rows at limit 100 is the case that needs the probe row: next_cursor must be set.
import assert from "node:assert/strict";
import { createHash, randomBytes } from "node:crypto";
import { after, before, test } from "node:test";

import { startWorker } from "./harness.mjs";

const DAY = 86_400_000;
const T = Date.now();
const session = randomBytes(32).toString("base64url");
const sha = (s) => createHash("sha256").update(s).digest("hex");
let w;

before(async () => {
  w = await startWorker({
    seedSql: [
      `INSERT INTO users (id, login, slug, display_name, role, password_hash, disabled_at, created_at, updated_at) VALUES (1, 'olga', 'olga', 'Olga', 'owner', NULL, NULL, ${T}, ${T});`,
      `INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES ('${sha(session)}', 1, ${T}, ${T + DAY});`,
    ].join("\n"),
  });
});
after(async () => {
  await w?.stop();
});

async function get(path) {
  const res = await fetch(w.url + path, { headers: { cookie: `__Host-plumblog_session=${session}` } });
  const text = await res.text();
  return { status: res.status, json: text ? JSON.parse(text) : null, text };
}
async function post(path, body) {
  const res = await fetch(w.url + path, {
    method: "POST",
    headers: { cookie: `__Host-plumblog_session=${session}`, origin: w.url, "content-type": "application/json" },
    body: JSON.stringify(body),
  });
  assert.equal(res.status, 201, await res.text());
}

for (const [route, key, mk] of [
  ["tags", "tags", (slug, i) => ({ slug, name: `Name ${1000 - i}` })],
  ["series", "series", (slug, i) => ({ slug, title: `Title ${1000 - i}` })],
]) {
  test(`${route}: empty list`, async () => {
    const r = await get(`/api/v1/${route}`);
    assert.equal(r.status, 200, r.text);
    assert.deepEqual(r.json, { [key]: [], next_cursor: null });
  });

  test(`${route}: 101 rows page by slug, 100 then 1, and the cursor is checked`, async () => {
    // Created newest-slug-first and with names that sort the other way: only the slug orders.
    for (let i = 100; i >= 0; i--) await post(`/api/v1/${route}`, mk(`p-${String(i).padStart(3, "0")}`, i));
    const want = Array.from({ length: 101 }, (_, i) => `p-${String(i).padStart(3, "0")}`);

    const a = await get(`/api/v1/${route}?limit=100`);
    assert.equal(a.status, 200, a.text);
    assert.equal(a.json[key].length, 100);
    assert.deepEqual(a.json[key].map((x) => x.slug), want.slice(0, 100));
    assert.equal(typeof a.json.next_cursor, "string", "101 rows at limit 100 has a next page");

    const b = await get(`/api/v1/${route}?limit=100&cursor=${a.json.next_cursor}`);
    assert.equal(b.status, 200, b.text);
    assert.deepEqual(b.json[key].map((x) => x.slug), [want[100]]);
    assert.equal(b.json.next_cursor, null);

    // Small pages walk the same order; the default page is 20 rows.
    const seen = [];
    let cursor = null;
    do {
      const r = await get(`/api/v1/${route}?limit=7${cursor ? `&cursor=${cursor}` : ""}`);
      seen.push(...r.json[key].map((x) => x.slug));
      cursor = r.json.next_cursor;
    } while (cursor);
    assert.deepEqual(seen, want);
    assert.equal((await get(`/api/v1/${route}`)).json[key].length, 20, "the default page is 20, as for posts");

    for (const q of ["cursor=!!", "cursor=Qm9n", "cursor=", "limit=0", "limit=101", "limit=x"]) {
      const r = await get(`/api/v1/${route}?${q}`);
      assert.equal(r.status, 400, `${q}: ${r.text}`);
      assert.equal(r.json.error, "input.invalid");
      assert.equal(r.json.field, q.split("=")[0]);
    }
  });
}
