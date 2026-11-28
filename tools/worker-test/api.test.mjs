// /api/v1 over HTTP against the real Worker (issue #10): the route table (reference), posts,
// tags, series and me, through a session and through Bearer tokens; every permission
// boundary, optimistic versions, body caps and hostile JSON.
import assert from "node:assert/strict";
import { createHash, randomBytes } from "node:crypto";
import { after, before, describe, test } from "node:test";

import { startWorker } from "./harness.mjs";

const COOKIE = "__Host-plumblog_session";
const DAY = 86_400_000;
const T = Date.now();
const MAX_BODY = 8 * 1024;
const MAX_POST_BODY = 160 * 1024;
const sha256hex = (s) => createHash("sha256").update(s).digest("hex");
const b64u = (buf) => Buffer.from(buf).toString("base64url");
const sq = (s) => `'${String(s).replaceAll("'", "''")}'`;
const newSecret = () => `plb_${b64u(randomBytes(32))}`;

// Every route the table must hold; the reference must list exactly these.
const ROUTES = [
  "GET /api/v1/reference",
  "GET /api/v1/me",
  "GET /api/v1/posts",
  "GET /api/v1/posts/{id}",
  "POST /api/v1/posts",
  "PATCH /api/v1/posts/{id}",
  "POST /api/v1/posts/{id}/trash",
  "POST /api/v1/posts/{id}/restore",
  "DELETE /api/v1/posts/{id}",
  "GET /api/v1/tags",
  "GET /api/v1/tags/{id}",
  "POST /api/v1/tags",
  "PATCH /api/v1/tags/{id}",
  "DELETE /api/v1/tags/{id}",
  "GET /api/v1/series",
  "GET /api/v1/series/{id}",
  "POST /api/v1/series",
  "PATCH /api/v1/series/{id}",
  "DELETE /api/v1/series/{id}",
  "GET /api/v1/media",
  "GET /api/v1/media/{id}",
  "POST /api/v1/media",
  "PATCH /api/v1/media/{id}",
  "DELETE /api/v1/media/{id}",
  "GET /api/v1/media/{id}/original",
  "POST /api/v1/media/{id}/variants",
  "GET /api/v1/comments",
  "GET /api/v1/users",
  "GET /api/v1/redirects",
  "PUT /api/v1/redirects",
  "DELETE /api/v1/redirects",
  "GET /api/v1/settings",
  "PUT /api/v1/settings/{key}",
  "DELETE /api/v1/settings/{key}",
  "GET /api/v1/themes/{name}/options",
  "PUT /api/v1/themes/{name}/options/{key}",
  "DELETE /api/v1/themes/{name}/options",
  "GET /api/v1/plugins",
  "PUT /api/v1/plugins/{name}",
  "GET /api/v1/plugins/{name}/options",
  "PUT /api/v1/plugins/{name}/options/{key}",
  "DELETE /api/v1/plugins/{name}/options",
];

/** One API call. `as` is {cookie} or {bearer}; `origin: null` omits the header. */
async function api(w, method, path, { body, as = {}, origin } = {}) {
  const h = {};
  if (origin !== null) h.origin = origin ?? w.url;
  if (as.cookie) h.cookie = `${COOKIE}=${as.cookie}`;
  if (as.bearer) h.authorization = `Bearer ${as.bearer}`;
  let payload;
  if (body !== undefined) {
    h["content-type"] = "application/json";
    payload = typeof body === "string" ? body : JSON.stringify(body);
  }
  const res = await fetch(w.url + path, { method, headers: h, body: payload });
  const text = await res.text();
  return { status: res.status, text, json: text ? JSON.parse(text) : null };
}

function assertError(r, status, code, field) {
  assert.equal(r.status, status, r.text);
  assert.deepEqual(r.json, field === undefined ? { error: code } : { error: code, field });
}

// Users: 1 owner, 2 writer. Sessions for both; tokens of each scope.
const S = { owner: b64u(randomBytes(32)), writer: b64u(randomBytes(32)) };
const TOK = { read: newSecret(), writer: newSecret(), owner: newSecret(), writerRead: newSecret() };
const AS = {
  ownerSession: { cookie: S.owner },
  writerSession: { cookie: S.writer },
  readToken: { bearer: TOK.read },
  writerReadToken: { bearer: TOK.writerRead },
  writerToken: { bearer: TOK.writer },
  ownerToken: { bearer: TOK.owner },
};

let w;
before(async () => {
  const seed = [
    `INSERT INTO users (id, login, slug, display_name, role, password_hash, disabled_at, created_at, updated_at) VALUES
      (1, 'olga', 'olga', 'Olga', 'owner', NULL, NULL, ${T}, ${T}),
      (2, 'will', 'will', 'Will', 'writer', NULL, NULL, ${T}, ${T});`,
    `INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES
      (${sq(sha256hex(S.owner))}, 1, ${T}, ${T + 29 * DAY}),
      (${sq(sha256hex(S.writer))}, 2, ${T}, ${T + 29 * DAY});`,
    `INSERT INTO api_tokens (id, user_id, name, token_hash, scope, created_at, expires_at, revoked_at) VALUES
      (11, 1, 'reader', ${sq(sha256hex(TOK.read))}, 'read', ${T}, NULL, NULL),
      (12, 2, 'agent', ${sq(sha256hex(TOK.writer))}, 'writer', ${T}, NULL, NULL),
      (13, 1, 'boss', ${sq(sha256hex(TOK.owner))}, 'owner', ${T}, NULL, NULL),
      (14, 2, 'will reads', ${sq(sha256hex(TOK.writerRead))}, 'read', ${T}, NULL, NULL);`,
    `INSERT INTO media (id, r2_key, mime, bytes, width, height, created_at) VALUES
      (1, 'o/1.jpg', 'image/jpeg', 10, 4, 3, ${T}),
      (2, 'o/2.jpg', 'image/jpeg', 10, 4, 3, ${T}),
      (3, 'o/3.jpg', 'image/jpeg', 10, 4, 3, ${T});`,
  ].join("\n");
  w = await startWorker({ seedSql: seed });
});

after(async () => {
  await w?.stop();
});

/** A request path for a route key, ids filled in. */
const pathOf = (key, id = 999999) =>
  key.split(" ")[1].replace("{id}", String(id)).replace("{key}", "zz_probe").replace("{name}", "minimal");
const methodOf = (key) => key.split(" ")[0];
const newPost = (slug, extra = {}) => ({ lang: "en", slug, title: `Title ${slug}`, markdown: `Body of ${slug}.`, ...extra });

describe("api v1", () => {
  test("reference: every route of the table, nothing else, read access", async () => {
    for (const as of [AS.ownerSession, AS.readToken]) {
      const r = await api(w, "GET", "/api/v1/reference", { as });
      assert.equal(r.status, 200, r.text);
      const keys = r.json.routes.map((x) => `${x.method} ${x.path}`);
      assert.deepEqual([...keys].sort(), [...ROUTES].sort());
      const create = r.json.routes.find((x) => x.method === "POST" && x.path === "/api/v1/posts");
      assert.equal(create.access, "post.write");
      assert.equal(create.max_body, MAX_POST_BODY);
      assert.ok(create.fields.some((f) => f.name === "markdown" && f.required && f.type === "string" && f.in === "body"));
    }
  });

  test("every table route is dispatched; anything else is api.not_found", async () => {
    for (const key of ROUTES) {
      const r = await api(w, methodOf(key), pathOf(key), { as: AS.ownerSession, body: methodOf(key) === "GET" || methodOf(key) === "DELETE" ? undefined : {} });
      assert.notDeepEqual(r.json, { error: "api.not_found" }, `${key}: ${r.text}`);
    }
    for (const [method, path] of [
      ["GET", "/api/v1/nope"],
      ["PUT", "/api/v1/posts/1"],
      ["GET", "/api/v1/posts/1/trash"],
      ["GET", "/api/v1/posts/"],
      ["GET", "/api/v1"],
    ]) {
      assertError(await api(w, method, path, { as: AS.ownerSession }), 404, "api.not_found");
    }
  });

  test("anonymous: 401 on every route", async () => {
    for (const key of ROUTES) {
      const body = methodOf(key) === "POST" || methodOf(key) === "PATCH" ? {} : undefined;
      assertError(await api(w, methodOf(key), pathOf(key, 1), { body }), 401, "auth.unauthenticated");
    }
  });

  // Reads a read token may not make: plugin options may hold a plugin's secrets; comments are
  // moderation data (writer and owner); users are owner only.
  const OWNER_READS = new Set(["GET /api/v1/plugins/{name}/options", "GET /api/v1/comments", "GET /api/v1/users"]);

  test("read token: every read answers (owner-only reads aside), every write is 403 and changes nothing", async () => {
    const made = await api(w, "POST", "/api/v1/posts", { as: AS.ownerSession, body: newPost("read-target") });
    assert.equal(made.status, 201, made.text);
    const id = made.json.id;
    for (const as of [AS.readToken, AS.writerReadToken]) {
      for (const key of ROUTES) {
        const method = methodOf(key);
        const body = method === "POST" ? newPost("from-read") : method === "PATCH" ? { expected_version: 1, title: "x" } : undefined;
        const r = await api(w, method, pathOf(key, id), { as, body });
        if (method === "GET" && !OWNER_READS.has(key)) {
          assert.ok(r.status === 200 || r.status === 404, `${key}: ${r.text}`);
        } else {
          assertError(r, 403, "auth.forbidden");
        }
      }
    }
    const after = await api(w, "GET", `/api/v1/posts/${id}`, { as: AS.readToken });
    assert.equal(after.json.version, 1);
    assert.equal(after.json.status, "draft");
    const list = await api(w, "GET", "/api/v1/posts?limit=100", { as: AS.readToken });
    assert.ok(!list.json.posts.some((p) => p.slug === "from-read"));
  });

  test("me: user, via and effective scope", async () => {
    const cases = [
      [AS.ownerSession, 1, "session", "owner"],
      [AS.writerSession, 2, "session", "writer"],
      [AS.readToken, 1, "token", "read"],
      [AS.writerToken, 2, "token", "writer"],
      [AS.ownerToken, 1, "token", "owner"],
    ];
    for (const [as, id, via, scope] of cases) {
      const r = await api(w, "GET", "/api/v1/me", { as });
      assert.equal(r.status, 200, r.text);
      assert.equal(r.json.user.id, id);
      assert.equal(r.json.via, via);
      assert.equal(r.json.scope, scope);
      assert.equal(r.json.permissions.includes("post.write"), scope !== "read");
      assert.equal(r.json.permissions.includes("settings.manage"), scope === "owner");
    }
  });

  for (const [label, as] of [["session", AS.writerSession], ["Bearer token", AS.writerToken]]) {
    test(`posts: create, read, list, update, publish, trash, restore, delete via ${label}`, async () => {
      const slug = `life-${label === "session" ? "s" : "t"}`;
      const tag = await api(w, "POST", "/api/v1/tags", { as, body: { slug: `t-${slug}`, name: "Tag" } });
      assert.equal(tag.status, 201, tag.text);
      const series = await api(w, "POST", "/api/v1/series", { as, body: { slug: `s-${slug}`, title: "Series" } });
      assert.equal(series.status, 201, series.text);
      const md = "# Heading\n\nHello **world**.\n\n![Heron](/media/2/heron.avif \"Cap\")\n\n![x](/media/1.avif)\n\n$$x^2$$\n\n```js\nlet a = 1;\n```\n";
      const c = await api(w, "POST", "/api/v1/posts", {
        as,
        body: newPost(slug, { markdown: md, tags: [tag.json.id], series_id: series.json.id, series_position: 1 }),
      });
      assert.equal(c.status, 201, c.text);
      const p = c.json;
      assert.equal(p.status, "draft");
      assert.equal(p.version, 1);
      assert.equal(p.author_id, 2);
      assert.equal(p.published_at, null);
      assert.deepEqual(p.tags, [tag.json.id]);
      assert.deepEqual(p.media_ids, [1, 2]);
      assert.equal(p.series_id, series.json.id);
      assert.match(p.html, /Heading<\/h1>/);
      assert.match(p.html, /<math/);
      assert.ok(p.excerpt.includes("Hello world"), p.excerpt);
      assert.deepEqual(p.warnings, []);

      const g = await api(w, "GET", `/api/v1/posts/${p.id}`, { as });
      assert.equal(g.status, 200, g.text);
      assert.equal(g.json.markdown, md);
      assert.deepEqual(g.json.media_ids, [1, 2]);

      const l = await api(w, "GET", `/api/v1/posts?tag=${tag.json.id}`, { as });
      assert.deepEqual(l.json.posts.map((x) => x.id), [p.id]);
      assert.equal(l.json.next_cursor, null);
      assert.equal(l.json.posts[0].markdown, undefined, "lists carry no bodies");

      const u = await api(w, "PATCH", `/api/v1/posts/${p.id}`, {
        as,
        body: { expected_version: 1, title: "New title", markdown: "Changed ![a](/media/3.avif)", excerpt: "Written." },
      });
      assert.equal(u.status, 200, u.text);
      assert.equal(u.json.version, 2);
      assert.equal(u.json.title, "New title");
      assert.equal(u.json.excerpt, "Written.");
      assert.deepEqual(u.json.media_ids, [3]);
      assert.deepEqual(u.json.tags, [tag.json.id], "untouched fields stay");

      const pub = await api(w, "PATCH", `/api/v1/posts/${p.id}`, { as, body: { expected_version: 2, status: "published" } });
      assert.equal(pub.status, 200, pub.text);
      assert.equal(pub.json.status, "published");
      assert.ok(pub.json.published_at >= T);
      assert.equal(pub.json.excerpt, "Written.", "a written excerpt survives other edits");

      const sched = await api(w, "PATCH", `/api/v1/posts/${p.id}`, {
        as,
        body: { expected_version: 3, status: "scheduled", published_at: T + DAY },
      });
      assert.equal(sched.status, 200, sched.text);
      assert.equal(sched.json.published_at, T + DAY);

      assertError(
        await api(w, "PATCH", `/api/v1/posts/${p.id}`, { as, body: { expected_version: 4, status: "scheduled", published_at: T - DAY } }),
        400, "input.invalid", "published_at",
      );
      assertError(await api(w, "DELETE", `/api/v1/posts/${p.id}`, { as }), 400, "input.invalid", "status");

      const tr = await api(w, "POST", `/api/v1/posts/${p.id}/trash`, { as });
      assert.equal(tr.status, 200, tr.text);
      assert.equal(tr.json.status, "trashed");
      assert.equal(tr.json.trashed_from, "scheduled");
      const re = await api(w, "POST", `/api/v1/posts/${p.id}/restore`, { as, body: "{}" });
      assert.equal(re.status, 200, re.text);
      assert.equal(re.json.status, "scheduled");
      await api(w, "POST", `/api/v1/posts/${p.id}/trash`, { as });
      assert.equal((await api(w, "DELETE", `/api/v1/posts/${p.id}`, { as })).status, 204);
      assertError(await api(w, "GET", `/api/v1/posts/${p.id}`, { as }), 404, "post.not_found");
    });

    test(`tags and series: list, get, update, delete via ${label}`, async () => {
      const k = label === "session" ? "s" : "t";
      const t = await api(w, "POST", "/api/v1/tags", { as, body: { slug: `tag-${k}`, name: "Tag" } });
      assert.equal(t.status, 201, t.text);
      assertError(await api(w, "POST", "/api/v1/tags", { as, body: { slug: `tag-${k}`, name: "Again" } }), 409, "store.conflict", "slug");
      assert.ok((await api(w, "GET", "/api/v1/tags", { as })).json.tags.some((x) => x.id === t.json.id));
      assert.deepEqual((await api(w, "GET", `/api/v1/tags/${t.json.id}`, { as })).json, t.json);
      const tu = await api(w, "PATCH", `/api/v1/tags/${t.json.id}`, { as, body: { name: "Renamed" } });
      assert.deepEqual(tu.json, { ...t.json, name: "Renamed" });
      assert.equal((await api(w, "DELETE", `/api/v1/tags/${t.json.id}`, { as })).status, 204);
      assertError(await api(w, "GET", `/api/v1/tags/${t.json.id}`, { as }), 404, "store.not_found");

      const s = await api(w, "POST", "/api/v1/series", { as, body: { slug: `ser-${k}`, title: "S", description: "Line\nline" } });
      assert.equal(s.status, 201, s.text);
      assert.deepEqual((await api(w, "GET", `/api/v1/series/${s.json.id}`, { as })).json, s.json);
      assert.ok((await api(w, "GET", "/api/v1/series", { as })).json.series.some((x) => x.id === s.json.id));
      const other = await api(w, "POST", "/api/v1/series", { as, body: { slug: `ser2-${k}`, title: "S2" } });
      assertError(await api(w, "PATCH", `/api/v1/series/${other.json.id}`, { as, body: { slug: `ser-${k}` } }), 409, "store.conflict", "slug");
      assert.equal((await api(w, "PATCH", `/api/v1/series/${s.json.id}`, { as, body: { title: "S!" } })).json.title, "S!");
      assert.equal((await api(w, "DELETE", `/api/v1/series/${s.json.id}`, { as })).status, 204);
      assertError(await api(w, "DELETE", `/api/v1/series/${s.json.id}`, { as }), 404, "store.not_found");
      assertError(await api(w, "POST", "/api/v1/tags", { as, body: { slug: "Bad Slug", name: "x" } }), 400, "input.invalid", "slug");
    });
  }

  test("owner token writes too; a session write needs the Origin, a token write does not", async () => {
    const r = await api(w, "POST", "/api/v1/posts", { as: AS.ownerToken, body: newPost("by-owner-token"), origin: null });
    assert.equal(r.status, 201, r.text);
    assertError(await api(w, "POST", "/api/v1/posts", { as: AS.ownerSession, body: newPost("no-origin"), origin: null }), 403, "auth.forbidden");
    assertError(await api(w, "POST", "/api/v1/posts", { as: AS.ownerSession, body: newPost("bad-origin"), origin: "https://evil.test" }), 403, "auth.forbidden");
  });

  test("expected_version: a stale save is 409 with the current version; two at once, one wins", async () => {
    const c = await api(w, "POST", "/api/v1/posts", { as: AS.writerSession, body: newPost("versioned") });
    const id = c.json.id;
    const u = await api(w, "PATCH", `/api/v1/posts/${id}`, { as: AS.writerSession, body: { expected_version: 1, title: "A" } });
    assert.equal(u.status, 200, u.text);
    const stale = await api(w, "PATCH", `/api/v1/posts/${id}`, { as: AS.writerToken, body: { expected_version: 1, title: "B" } });
    assert.equal(stale.status, 409, stale.text);
    assert.deepEqual(stale.json, { error: "store.conflict", field: "expected_version", version: 2 });
    assertError(await api(w, "PATCH", `/api/v1/posts/${id}`, { as: AS.writerToken, body: { title: "C" } }), 400, "input.invalid", "expected_version");

    const both = await Promise.all(
      ["X", "Y"].map((title) => api(w, "PATCH", `/api/v1/posts/${id}`, { as: AS.writerSession, body: { expected_version: 2, title } })),
    );
    const statuses = both.map((r) => r.status).sort();
    assert.deepEqual(statuses, [200, 409], both.map((r) => r.text).join(" | "));
    const winner = both.find((r) => r.status === 200).json;
    const loser = both.find((r) => r.status === 409).json;
    assert.equal(loser.version, 3);
    const now = await api(w, "GET", `/api/v1/posts/${id}`, { as: AS.readToken });
    assert.equal(now.json.title, winner.title);
    assert.equal(now.json.version, 3);

    const dup = await api(w, "PATCH", `/api/v1/posts/${id}`, { as: AS.writerSession, body: { expected_version: 3, slug: "by-owner-token" } });
    assertError(dup, 409, "store.conflict", "slug");
  });

  test("body caps: 160 KiB on post routes (exactly at is fine, one byte more is 413), 8 KiB elsewhere", async () => {
    const shell = (pad) => `{"lang":"en","slug":"cap","title":"Cap","markdown":"${pad}"}`;
    const fixed = shell("").length;
    const at = shell("word ".repeat(MAX_POST_BODY).slice(0, MAX_POST_BODY - fixed - 6) + "a".repeat(6));
    assert.equal(Buffer.byteLength(at), MAX_POST_BODY);
    const ok = await api(w, "POST", "/api/v1/posts", { as: AS.writerToken, body: at });
    assert.equal(ok.status, 201, ok.text.slice(0, 200));
    const over = shell("a".repeat(MAX_POST_BODY - fixed + 1));
    assert.equal(Buffer.byteLength(over), MAX_POST_BODY + 1);
    assertError(await api(w, "POST", "/api/v1/posts", { as: AS.writerToken, body: over }), 413, "api.body_too_large");
    const patchOver = `{"expected_version":1,"markdown":"${"a".repeat(MAX_POST_BODY)}"}`;
    assertError(await api(w, "PATCH", `/api/v1/posts/${ok.json.id}`, { as: AS.writerToken, body: patchOver }), 413, "api.body_too_large");
    const tagShell = (n) => `{"slug":"big","name":"${"n".repeat(n)}"}`;
    const tagAt = tagShell(MAX_BODY - tagShell(0).length);
    assert.equal(Buffer.byteLength(tagAt), MAX_BODY);
    assertError(await api(w, "POST", "/api/v1/tags", { as: AS.writerToken, body: tagAt }), 400, "input.invalid", "name");
    const tagOver = tagShell(MAX_BODY - tagShell(0).length + 1);
    assertError(await api(w, "POST", "/api/v1/tags", { as: AS.writerToken, body: tagOver }), 413, "api.body_too_large");
  });

  test("a ~20k-word post with code, math and 50 images saves within the cap", async () => {
    const words = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do ".split(" ").filter(Boolean);
    let md = "";
    for (let i = 0; i < 50; i++) {
      const para = Array.from({ length: 400 }, (_, j) => words[(i + j) % words.length]).join(" ");
      md += `## Section ${i}\n\n${para}\n\n\`\`\`rust\nfn f${i}() -> u32 { ${i} }\n\`\`\`\n\n$$x_${i}^2 + y^2$$\n\n![Image ${i}](/media/${(i % 3) + 1}.avif "Caption ${i}")\n\n`;
    }
    const body = JSON.stringify(newPost("long", { markdown: md }));
    assert.ok(Buffer.byteLength(body) <= MAX_POST_BODY, `${Buffer.byteLength(body)}`);
    const started = performance.now();
    const r = await api(w, "POST", "/api/v1/posts", { as: AS.writerToken, body });
    const ms = Math.round(performance.now() - started);
    assert.equal(r.status, 201, r.text.slice(0, 300));
    assert.ok(r.json.word_count >= 20_000, `${r.json.word_count}`);
    assert.deepEqual(r.json.media_ids, [1, 2, 3]);
    console.log(`long post: ${Buffer.byteLength(body)} bytes, ${r.json.word_count} words, saved in ${ms} ms wall (local)`);
    const rows = w.query(`SELECT media_id FROM post_media WHERE post_id = ${r.json.id} ORDER BY media_id`);
    assert.deepEqual(rows.map((x) => x.media_id), [1, 2, 3]);
  });

  test("hostile JSON: 400 naming the field", async () => {
    const base = `"lang":"en","slug":"hostile","title":"T","markdown":"x"`;
    for (const [extra, field] of [
      [`,"tags":${"[".repeat(40)}1${"]".repeat(40)}`, "tags"],
      [`,"tags":${"[".repeat(5000)}`, "tags"],
      [`,"series_id":99999999999999999999999`, "series_id"],
      [`,"series_id":-9223372036854775809`, "series_id"],
      [`,"series_id":1.5`, "series_id"],
      [`,"series_id":1e2`, "series_id"],
      [`,"series_id":01`, "series_id"],
      [`,"title":"again"`, "title"],
      [`,"surprise":1`, "surprise"],
      [`,"pinned":"true"`, "pinned"],
      [`,"tags":[1,"2"]`, "tags"],
      [`,"tags":[0]`, "tags"],
      [`,"status":"trashed"`, "status"],
      [`,"kind":"essay"`, "kind"],
      [`,"published_at":${T}`, "published_at"],
    ]) {
      const r = await api(w, "POST", "/api/v1/posts", { as: AS.writerToken, body: `{${base}${extra}}` });
      assertError(r, 400, "input.invalid", field);
    }
    // Over the 4096-value cap (5 x 1000 keys): its own code, refused while parsing.
    const wide = `,"tags":[${Array(5).fill(`{${Array.from({ length: 1000 }, (_, i) => `"k${i}":0`).join(",")}}`).join(",")}]`;
    assertError(await api(w, "POST", "/api/v1/posts", { as: AS.writerToken, body: `{${base}${wide}}` }), 413, "input.too_complex", "tags");
    for (const body of ["", "[]", "{", "null", `{${base}} x`]) {
      assertError(await api(w, "POST", "/api/v1/posts", { as: AS.writerToken, body }), 400, "input.invalid");
    }
    assertError(await api(w, "POST", "/api/v1/posts", { as: AS.writerToken, body: { lang: "en", slug: "a" } }), 400, "input.invalid", "title");
    assertError(await api(w, "GET", "/api/v1/posts?surprise=1", { as: AS.readToken }), 400, "input.invalid", "surprise");
    assertError(await api(w, "GET", "/api/v1/posts?limit=1.5", { as: AS.readToken }), 400, "input.invalid", "limit");
    assertError(await api(w, "GET", "/api/v1/posts/abc", { as: AS.readToken }), 404, "store.not_found");
    const list = await api(w, "GET", "/api/v1/posts?limit=100", { as: AS.readToken });
    assert.ok(!list.json.posts.some((p) => p.slug === "hostile"), "nothing was saved");
  });

  test("pagination: keyset pages, no gaps or repeats, limit and cursor checked", async () => {
    for (let i = 0; i < 5; i++) {
      const r = await api(w, "POST", "/api/v1/posts", { as: AS.writerSession, body: newPost(`page-${i}`, { lang: "vi" }) });
      assert.equal(r.status, 201, r.text);
    }
    const seen = [];
    let cursor = null;
    const sizes = [];
    do {
      const q = `/api/v1/posts?lang=vi&limit=2${cursor ? `&cursor=${cursor}` : ""}`;
      const r = await api(w, "GET", q, { as: AS.readToken });
      assert.equal(r.status, 200, r.text);
      sizes.push(r.json.posts.length);
      seen.push(...r.json.posts.map((p) => p.slug));
      cursor = r.json.next_cursor;
    } while (cursor);
    assert.deepEqual(sizes, [2, 2, 1]);
    assert.deepEqual(seen, ["page-4", "page-3", "page-2", "page-1", "page-0"]);
    const exact = await api(w, "GET", "/api/v1/posts?lang=vi&limit=5", { as: AS.readToken });
    assert.equal(exact.json.posts.length, 5);
    assert.equal(exact.json.next_cursor, null, "no cursor to an empty page");
    const all = await api(w, "GET", "/api/v1/posts?lang=vi&limit=100", { as: AS.readToken });
    assert.equal(all.json.posts.length, 5);
    for (const limit of ["0", "101", "-1"]) {
      assertError(await api(w, "GET", `/api/v1/posts?limit=${limit}`, { as: AS.readToken }), 400, "input.invalid", "limit");
    }
    const first = await api(w, "GET", "/api/v1/posts?lang=vi&limit=2", { as: AS.readToken });
    assertError(
      await api(w, "GET", `/api/v1/posts?order=published&cursor=${first.json.next_cursor}`, { as: AS.readToken }),
      400, "input.invalid", "cursor",
    );
    assertError(await api(w, "GET", "/api/v1/posts?cursor=bm9wZQ", { as: AS.readToken }), 400, "input.invalid", "cursor");
    assertError(await api(w, "GET", "/api/v1/posts?order=sideways", { as: AS.readToken }), 400, "input.invalid", "order");
    const drafts = await api(w, "GET", "/api/v1/posts?status=draft&kind=post&author=2&limit=100", { as: AS.readToken });
    assert.ok(drafts.json.posts.length >= 5 && drafts.json.posts.every((p) => p.status === "draft" && p.author_id === 2));
  });

  test("every write is logged with the acting user and how it came", async () => {
    const rows = w.query("SELECT actor_id, detail FROM system_log WHERE kind = 'content.changed' ORDER BY id");
    const has = (pred) => rows.some(pred);
    assert.ok(has((r) => r.actor_id === 2 && r.detail.includes("action\tpost.create") && r.detail.includes("via\ttoken") && r.detail.includes("client\tagent #12")), JSON.stringify(rows.slice(0, 5)));
    assert.ok(has((r) => r.actor_id === 2 && r.detail.includes("action\tpost.update") && r.detail.includes("via\tsession")));
    for (const action of ["post.trash", "post.restore", "post.delete", "tag.create", "tag.update", "tag.delete", "series.create", "series.update", "series.delete"]) {
      assert.ok(has((r) => r.detail.includes(`action\t${action}`)), action);
    }
    assert.ok(!has((r) => r.detail.includes("client\treader")), "read tokens wrote nothing");
  });
});
