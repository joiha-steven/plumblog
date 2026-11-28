// Paged lists at the largest page size: with one row more than a full page stored,
// limit=100 must say another page exists (the probe row must not be clamped away).
import assert from "node:assert/strict";
import { createHash, randomBytes } from "node:crypto";
import { after, before, test } from "node:test";

import { startWorker } from "./harness.mjs";

const T = Date.now();
const readToken = `plb_${randomBytes(32).toString("base64url")}`;
const sha = (s) => createHash("sha256").update(s).digest("hex");
const ROWS = 101;
let w;

before(async () => {
  const n = `WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < ${ROWS})`;
  w = await startWorker({
    seedSql: [
      `INSERT INTO users (id, login, slug, display_name, role, password_hash, disabled_at, created_at, updated_at) VALUES (1, 'olga', 'olga', 'Olga', 'owner', NULL, NULL, ${T}, ${T});`,
      `INSERT INTO api_tokens (id, user_id, name, token_hash, scope, created_at, expires_at, revoked_at) VALUES (11, 1, 'reader', '${sha(readToken)}', 'read', ${T}, NULL, NULL);`,
      `${n} INSERT INTO posts (kind, translation_group, lang, slug, status, published_at, author_id, title, markdown, html, created_at, updated_at) SELECT 'post', i, 'en', 's' || i, 'published', i * 1000, 1, 't', 'm', 'h', i, i FROM n;`,
      `${n} INSERT INTO media (r2_key, mime, bytes, width, height, created_at) SELECT 'k' || i, 'image/png', 10, 4, 3, i FROM n;`,
    ].join("\n"),
  });
});
after(async () => {
  await w?.stop();
});

async function get(path) {
  const res = await fetch(w.url + path, { headers: { authorization: `Bearer ${readToken}` } });
  assert.equal(res.status, 200, path);
  return res.json();
}

for (const [name, path, key] of [
  ["posts", "/api/v1/posts", "posts"],
  ["media", "/api/v1/media", "media"],
]) {
  test(`${name}: limit=100 with 101 rows has a next page of one`, async () => {
    const first = await get(`${path}?limit=100`);
    assert.equal(first[key].length, 100);
    assert.ok(first.next_cursor, "next_cursor is set");
    const next = await get(`${path}?limit=100&cursor=${first.next_cursor}`);
    assert.equal(next[key].length, 1);
    assert.equal(next.next_cursor, null);
  });
}
