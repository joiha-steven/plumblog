// `counts` on GET /api/v1/posts: posts per status for the admin's filter pills. First page
// only; follows kind and lang, nothing else; trash included; a read token gets it too.
import assert from "node:assert/strict";
import { createHash, randomBytes } from "node:crypto";
import { after, before, test } from "node:test";

import { startWorker } from "./harness.mjs";

const DAY = 86_400_000;
const T = Date.now();
const session = randomBytes(32).toString("base64url");
const writerSession = randomBytes(32).toString("base64url");
const readToken = `plb_${randomBytes(32).toString("base64url")}`;
const sha = (s) => createHash("sha256").update(s).digest("hex");
let w;

before(async () => {
  w = await startWorker({
    seedSql: [
      `INSERT INTO users (id, login, slug, display_name, role, password_hash, disabled_at, created_at, updated_at) VALUES (1, 'olga', 'olga', 'Olga', 'owner', NULL, NULL, ${T}, ${T}), (2, 'will', 'will', 'Will', 'writer', NULL, NULL, ${T}, ${T});`,
      `INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES ('${sha(session)}', 1, ${T}, ${T + DAY}), ('${sha(writerSession)}', 2, ${T}, ${T + DAY});`,
      `INSERT INTO api_tokens (id, user_id, name, token_hash, scope, created_at, expires_at, revoked_at) VALUES (11, 1, 'reader', '${sha(readToken)}', 'read', ${T}, NULL, NULL);`,
    ].join("\n"),
  });
});
after(async () => {
  await w?.stop();
});

async function api(method, path, body, headers = { cookie: `__Host-plumblog_session=${session}` }) {
  const h = { origin: w.url, ...headers };
  if (body !== undefined) h["content-type"] = "application/json";
  const res = await fetch(w.url + path, { method, headers: h, body: body === undefined ? undefined : JSON.stringify(body) });
  const text = await res.text();
  return { status: res.status, json: text ? JSON.parse(text) : null, text };
}
const counts = (d, s, p, t) => ({ draft: d, scheduled: s, published: p, trashed: t });

test("counts: first page only, per status, filtered by kind and lang and nothing else", async () => {
  const empty = await api("GET", "/api/v1/posts");
  assert.equal(empty.status, 200, empty.text);
  assert.deepEqual(empty.json.counts, counts(0, 0, 0, 0), "every key is present, 0 when none");

  // slug, kind, lang, status
  const tag = await api("POST", "/api/v1/tags", { slug: "t", name: "T" });
  const series = await api("POST", "/api/v1/series", { slug: "s", title: "S" });
  assert.equal(tag.status, 201, tag.text);
  assert.equal(series.status, 201, series.text);
  const rows = [
    ["a", "post", "en", "draft"],
    ["b", "post", "en", "draft"],
    ["c", "post", "vi", "draft"],
    ["d", "post", "en", "published"],
    ["e", "post", "vi", "published"],
    ["f", "page", "en", "published"],
    ["g", "post", "en", "scheduled"],
    ["h", "page", "vi", "scheduled"],
    ["i", "post", "vi", "published"],
  ];
  const ids = {};
  for (const [slug, kind, lang, status] of rows) {
    const body = { slug, kind, lang, status, title: slug, markdown: "x" };
    if (status !== "draft") body.published_at = status === "scheduled" ? T + DAY : T - DAY;
    if (slug === "a") body.tags = [tag.json.id];
    if (slug === "b") body.series_id = series.json.id;
    // "c" is the second author's; the rest are the owner's.
    const as = slug === "c" ? { cookie: `__Host-plumblog_session=${writerSession}` } : undefined;
    const r = await api("POST", "/api/v1/posts", body, as);
    assert.equal(r.status, 201, r.text);
    ids[slug] = r.json.id;
  }
  // A trashed post counts under trashed, not under the status it came from.
  const tr = await api("POST", `/api/v1/posts/${ids.i}/trash`);
  assert.equal(tr.status, 200, tr.text);

  const get = async (q, headers) => (await api("GET", `/api/v1/posts${q}`, undefined, headers)).json;
  assert.deepEqual((await get("")).counts, counts(3, 2, 3, 1));
  assert.deepEqual((await get("?kind=post")).counts, counts(3, 1, 2, 1));
  assert.deepEqual((await get("?lang=vi")).counts, counts(1, 1, 1, 1));
  assert.deepEqual((await get("?kind=page&lang=en")).counts, counts(0, 0, 1, 0));
  // Status, tag, series, author and order do not narrow the counts; the list does.
  const drafts = await get("?status=draft");
  assert.deepEqual(drafts.counts, counts(3, 2, 3, 1));
  assert.equal(drafts.posts.length, 3);
  const all = counts(3, 2, 3, 1);
  assert.deepEqual((await get("?status=trashed&order=published")).counts, all);
  for (const [q, n] of [[`?author=2`, 1], [`?tag=${tag.json.id}`, 1], [`?series=${series.json.id}`, 1]]) {
    const r = await get(q);
    assert.deepEqual(r.counts, all, `${q} does not narrow the counts`);
    assert.equal(r.posts.length, n, `${q} narrows the list`);
  }
  assert.deepEqual((await get("?kind=post&status=published")).counts, counts(3, 1, 2, 1));

  // A page fetched with a cursor has no counts.
  const first = await get("?limit=2");
  assert.ok(first.next_cursor);
  assert.deepEqual(first.counts, counts(3, 2, 3, 1));
  const next = await get(`?limit=2&cursor=${first.next_cursor}`);
  assert.equal(next.counts, null);
  assert.ok("counts" in next, "the key is there, as null");

  // A read token sees every status, so it gets the same counts.
  const asRead = await get("", { authorization: `Bearer ${readToken}` });
  assert.deepEqual(asRead.counts, counts(3, 2, 3, 1));
});
