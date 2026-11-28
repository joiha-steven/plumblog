// The reads a full export needs (issue #79): media originals, every comment, every user.
// Who may read each one, paging by id across a cursor, empty pages, the original's bytes
// coming back unchanged, a missing R2 object, and users answered without any secret.
import assert from "node:assert/strict";
import { createHash, randomBytes } from "node:crypto";
import { deflateSync } from "node:zlib";
import { after, before, describe, test } from "node:test";

import { startWorker } from "./harness.mjs";

const COOKIE = "__Host-plumblog_session";
const DAY = 86_400_000;
const T = Date.now();
const sha = (s) => createHash("sha256").update(s).digest("hex");
const secret = () => `plb_${randomBytes(32).toString("base64url")}`;
// Stored hashes that must never come back from GET /api/v1/users.
const PASSWORD_HASH = `pbkdf2$1000$${randomBytes(16).toString("hex")}$${randomBytes(32).toString("hex")}`;

const ownerSession = randomBytes(32).toString("base64url");
const writerSession = randomBytes(32).toString("base64url");
const readToken = secret();
const writerToken = secret();
const ownerToken = secret();
// A writer whose token was made with scope owner: it acts as min(scope, role) = writer.
const writerOwnerScopeToken = secret();
// A user disabled after their session and token were made.
const disabledSession = randomBytes(32).toString("base64url");
const disabledToken = secret();
const AS = {
  anonymous: {},
  readToken: { authorization: `Bearer ${readToken}` },
  writerToken: { authorization: `Bearer ${writerToken}` },
  writerSession: { cookie: `${COOKIE}=${writerSession}` },
  ownerSession: { cookie: `${COOKIE}=${ownerSession}` },
  ownerToken: { authorization: `Bearer ${ownerToken}` },
  writerOwnerScopeToken: { authorization: `Bearer ${writerOwnerScopeToken}` },
  disabledSession: { cookie: `${COOKIE}=${disabledSession}` },
  disabledToken: { authorization: `Bearer ${disabledToken}` },
};

// --- A decodable PNG, built here so no binary fixture is checked in. ---
const CRC = Array.from({ length: 256 }, (_, n) => {
  let c = n;
  for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  return c >>> 0;
});
const crc32 = (buf) => {
  let c = 0xffffffff;
  for (const b of buf) c = CRC[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
};
const u32 = (n) => {
  const b = Buffer.alloc(4);
  b.writeUInt32BE(n);
  return b;
};
const chunk = (type, data) => {
  const td = Buffer.concat([Buffer.from(type), data]);
  return Buffer.concat([u32(data.length), td, u32(crc32(td))]);
};
/** An RGBA PNG of random pixels: incompressible, so any byte the route changed shows. */
function png(w, h) {
  const rows = Array.from({ length: h }, () => Buffer.concat([Buffer.from([0]), randomBytes(w * 4)]));
  return Buffer.concat([
    Buffer.from("\x89PNG\r\n\x1a\n", "latin1"),
    chunk("IHDR", Buffer.concat([u32(w), u32(h), Buffer.from([8, 6, 0, 0, 0])])),
    chunk("IDAT", deflateSync(Buffer.concat(rows))),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}

let w;
let postId;

before(async () => {
  w = await startWorker({
    seedSql: [
      `INSERT INTO users (id, login, slug, display_name, bio, role, password_hash, disabled_at, created_at, updated_at) VALUES
        (1, 'olga', 'olga', 'Olga', 'Owner bio', 'owner', '${PASSWORD_HASH}', NULL, ${T}, ${T + 1}),
        (2, 'will', 'will', 'Will', '', 'writer', '${PASSWORD_HASH}x', NULL, ${T + 2}, ${T + 3}),
        (3, 'gone', 'gone', 'Gone', '', 'writer', NULL, ${T + 4}, ${T + 4}, ${T + 5}),
        (4, 'dora', 'dora', 'Dora', '', 'owner', NULL, NULL, ${T + 6}, ${T + 6});`,
      `INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES
        ('${sha(ownerSession)}', 1, ${T}, ${T + DAY}), ('${sha(writerSession)}', 2, ${T}, ${T + DAY}),
        ('${sha(disabledSession)}', 4, ${T}, ${T + DAY});`,
      `INSERT INTO api_tokens (id, user_id, name, token_hash, scope, created_at, expires_at, revoked_at) VALUES
        (11, 1, 'reader', '${sha(readToken)}', 'read', ${T}, NULL, NULL),
        (12, 2, 'writer', '${sha(writerToken)}', 'writer', ${T}, NULL, NULL),
        (13, 1, 'owner', '${sha(ownerToken)}', 'owner', ${T}, NULL, NULL),
        (14, 2, 'writer as owner', '${sha(writerOwnerScopeToken)}', 'owner', ${T}, NULL, NULL),
        (15, 4, 'soon disabled', '${sha(disabledToken)}', 'owner', ${T}, NULL, NULL);`,
    ].join("\n"),
  });
});
after(async () => {
  await w?.stop();
});

async function api(path, as, { method = "GET", query, raw } = {}) {
  const q = query ? `?${new URLSearchParams(query)}` : "";
  const headers = { origin: w.url, ...as };
  if (raw !== undefined) headers["content-type"] = "application/octet-stream";
  const res = await fetch(w.url + path + q, { method, headers, body: raw });
  const bytes = Buffer.from(await res.arrayBuffer());
  const text = bytes.toString("utf8");
  const type = res.headers.get("content-type") ?? "";
  return {
    status: res.status,
    headers: res.headers,
    bytes,
    text,
    json: type.startsWith("application/json") && text ? JSON.parse(text) : null,
  };
}

/** Waits (bounded) for wrangler's output to show `re`: it reaches the pipe after the response. */
async function logged(re, ms = 5_000) {
  const end = Date.now() + ms;
  while (!re.test(w.log())) {
    if (Date.now() > end) assert.match(w.log(), re);
    await new Promise((ok) => setTimeout(ok, 50));
  }
}

/** Every page of a list, following `next_cursor`. */
async function pages(path, key, limit, as = AS.ownerSession) {
  const out = [];
  let cursor = null;
  for (let i = 0; i < 50; i++) {
    const r = await api(path, as, { query: cursor ? { limit, cursor } : { limit } });
    assert.equal(r.status, 200, r.text);
    out.push(r.json[key]);
    cursor = r.json.next_cursor;
    if (cursor === null) return out;
    assert.equal(typeof cursor, "string");
  }
  throw new Error("more than 50 pages");
}

describe("export reads", () => {
  let mediaId;
  let uploaded;

  test("comments: an empty site answers an empty page with no cursor", async () => {
    const r = await api("/api/v1/comments", AS.ownerSession);
    assert.equal(r.status, 200, r.text);
    assert.deepEqual(r.json, { comments: [], next_cursor: null });
  });

  test("seed: a post with a comment in every status, one of them a reply; one uploaded original", async () => {
    // The post is made by the API so its row is complete; comments go straight into D1.
    const made = await fetch(`${w.url}/api/v1/posts`, {
      method: "POST",
      headers: { ...AS.ownerSession, origin: w.url, "content-type": "application/json" },
      body: JSON.stringify({ lang: "en", slug: "hello", title: "Hello", markdown: "x" }),
    });
    const text = await made.text();
    assert.equal(made.status, 201, text);
    postId = JSON.parse(text).id;
    w.exec(`INSERT INTO comments (id, post_id, parent_id, author_name, body, body_html, status, created_at) VALUES
      (1, ${postId}, NULL, 'Ann', 'First', '<p>First</p>', 'approved', ${T + 50}),
      (2, ${postId}, 1, 'Bo', 'Reply', '<p>Reply</p>', 'pending', ${T + 10}),
      (3, ${postId}, NULL, 'Spam Bot', 'Buy', '<p>Buy</p>', 'spam', ${T + 30}),
      (4, ${postId}, NULL, 'Cy', 'Old', NULL, 'trashed', ${T + 20}),
      (5, ${postId}, NULL, 'Di', 'Second', '<p>Second</p>', 'approved', ${T + 5});`);

    uploaded = png(40, 30);
    const up = await api("/api/v1/media", AS.writerToken, {
      method: "POST",
      query: { name: "Photo.png", mime: "image/png", width: 40, height: 30 },
      raw: uploaded,
    });
    assert.equal(up.status, 201, up.text);
    mediaId = up.json.id;
  });

  // Who may call each route. 200 or the error status; errors carry the stable code.
  const MATRIX = [
    ["original", () => `/api/v1/media/${mediaId}/original`, { anonymous: 401, readToken: 200, writerToken: 200, writerSession: 200, ownerSession: 200, ownerToken: 200, writerOwnerScopeToken: 200 }],
    ["comments", () => "/api/v1/comments", { anonymous: 401, readToken: 403, writerToken: 200, writerSession: 200, ownerSession: 200, ownerToken: 200, writerOwnerScopeToken: 200 }],
    ["users", () => "/api/v1/users", { anonymous: 401, readToken: 403, writerToken: 403, writerSession: 403, ownerSession: 200, ownerToken: 200, writerOwnerScopeToken: 403 }],
  ];
  for (const [name, path, want] of MATRIX) {
    for (const [who, status] of Object.entries(want)) {
      test(`access: ${name} as ${who} -> ${status}`, async () => {
        const r = await api(path(), AS[who]);
        assert.equal(r.status, status, r.text);
        if (status === 401) assert.deepEqual(r.json, { error: "auth.unauthenticated" });
        if (status === 403) assert.deepEqual(r.json, { error: "auth.forbidden" });
      });
    }
  }

  test("access: a disabled user's session and token are 401 on all three routes", async () => {
    // Both worked while the account was active.
    for (const as of [AS.disabledSession, AS.disabledToken]) {
      assert.equal((await api("/api/v1/users", as)).status, 200);
    }
    w.exec(`UPDATE users SET disabled_at = ${T + 7} WHERE id = 4;`);
    for (const [, path] of MATRIX) {
      for (const who of ["disabledSession", "disabledToken"]) {
        const r = await api(path(), AS[who]);
        assert.equal(r.status, 401, `${who} ${path()}: ${r.text}`);
        assert.deepEqual(r.json, { error: "auth.unauthenticated" });
      }
    }
  });

  test("reference: the three routes, their access, and the original marked as raw bytes", async () => {
    const r = await api("/api/v1/reference", AS.readToken);
    assert.equal(r.status, 200, r.text);
    const find = (p) => r.json.routes.find((x) => x.method === "GET" && x.path === p);
    assert.equal(find("/api/v1/media/{id}/original").access, "read");
    assert.equal(find("/api/v1/media/{id}/original").response, "original");
    assert.equal(find("/api/v1/comments").access, "comment.moderate");
    assert.equal(find("/api/v1/comments").response, "comment_page");
    assert.equal(find("/api/v1/users").access, "user.manage");
    assert.equal(find("/api/v1/users").response, "user_page");
  });

  test("original: the uploaded bytes, byte for byte, with its type and length, never cached", async () => {
    const r = await api(`/api/v1/media/${mediaId}/original`, AS.readToken);
    assert.equal(r.status, 200, r.text);
    assert.equal(r.headers.get("content-type"), "image/png");
    assert.equal(r.headers.get("content-length"), String(uploaded.length));
    assert.equal(r.headers.get("cache-control"), "private, no-store");
    assert.equal(r.headers.get("x-content-type-options"), "nosniff");
    assert.equal(r.headers.get("content-security-policy"), "sandbox");
    assert.equal(Buffer.compare(r.bytes, uploaded), 0, "bytes differ from the upload");
  });

  test("original: a missing row, or a path id that is no id, is 404 store.not_found", async () => {
    const r = await api("/api/v1/media/999999/original", AS.readToken);
    assert.equal(r.status, 404, r.text);
    assert.deepEqual(r.json, { error: "store.not_found" });
    const bad = await api("/api/v1/media/x/original", AS.readToken);
    assert.equal(bad.status, 404, bad.text);
    assert.deepEqual(bad.json, { error: "store.not_found" });
  });

  test("original: a row whose R2 object is gone is 404 and logged with the key", async () => {
    const key = `media/${randomBytes(16).toString("base64url")}/lost.png`;
    w.exec(`INSERT INTO media (id, r2_key, mime, bytes, width, height, created_at) VALUES (900, '${key}', 'image/png', 10, 1, 1, ${T});`);
    const r = await api("/api/v1/media/900/original", AS.ownerSession);
    assert.equal(r.status, 404, r.text);
    assert.deepEqual(r.json, { error: "store.not_found" });
    await logged(new RegExp(`media 900: R2 object ${key} is missing`));
  });

  test("original: a row whose mime is not an accepted image type is refused and logged", async () => {
    // The real object stays in R2; only the row says SVG, as a write around the API would.
    w.exec(`UPDATE media SET mime = 'image/svg+xml' WHERE id = ${mediaId};`);
    try {
      const r = await api(`/api/v1/media/${mediaId}/original`, AS.ownerSession);
      assert.equal(r.status, 500, r.text);
      assert.deepEqual(r.json, { error: "store.failed" });
      assert.equal(r.headers.get("content-type"), "application/json; charset=utf-8");
      await logged(new RegExp(`media ${mediaId}: stored mime "image/svg\\+xml" is not an accepted image type`));
    } finally {
      w.exec(`UPDATE media SET mime = 'image/png' WHERE id = ${mediaId};`);
    }
  });

  test("original: an object whose size disagrees with its row is refused and logged", async () => {
    w.exec(`UPDATE media SET bytes = bytes + 1 WHERE id = ${mediaId};`);
    try {
      const r = await api(`/api/v1/media/${mediaId}/original`, AS.readToken);
      assert.equal(r.status, 500, r.text);
      assert.deepEqual(r.json, { error: "store.failed" });
      await logged(new RegExp(`media ${mediaId}: R2 object \\S+ is ${uploaded.length} bytes, row says ${uploaded.length + 1}`));
    } finally {
      w.exec(`UPDATE media SET bytes = bytes - 1 WHERE id = ${mediaId};`);
    }
  });

  test("comments: every status, by id, paged across cursors, with exactly the export fields", async () => {
    const all = await pages("/api/v1/comments", "comments", 2, AS.writerToken);
    assert.deepEqual(all.map((p) => p.length), [2, 2, 1]);
    const rows = all.flat();
    assert.deepEqual(rows.map((c) => c.id), [1, 2, 3, 4, 5], "id order, not created_at");
    assert.deepEqual([...new Set(rows.map((c) => c.status))].sort(), ["approved", "pending", "spam", "trashed"]);
    assert.deepEqual(rows[1], {
      id: 2,
      post_id: postId,
      parent_id: 1,
      author_name: "Bo",
      body: "Reply",
      body_html: "<p>Reply</p>",
      status: "pending",
      created_at: T + 10,
    });
    assert.equal(rows[0].parent_id, null);
    assert.equal(rows[3].body_html, null, "rows from before 0004 have no stored HTML");
    // A full last page still says there is no next one.
    const exact = await api("/api/v1/comments", AS.ownerSession, { query: { limit: 5 } });
    assert.equal(exact.json.comments.length, 5);
    assert.equal(exact.json.next_cursor, null);
    // Default limit holds them all.
    const def = await api("/api/v1/comments", AS.ownerSession);
    assert.equal(def.json.comments.length, 5);
  });

  test("comments and users: limit 1-100 and a cursor of this listing only", async () => {
    for (const path of ["/api/v1/comments", "/api/v1/users"]) {
      for (const limit of ["0", "101", "-1"]) {
        const r = await api(path, AS.ownerSession, { query: { limit } });
        assert.equal(r.status, 400, `${path} limit ${limit}: ${r.text}`);
        assert.deepEqual(r.json, { error: "input.invalid", field: "limit" });
      }
      for (const cursor of ["", "!", Buffer.from("x").toString("base64url"), Buffer.from("0").toString("base64url")]) {
        const r = await api(path, AS.ownerSession, { query: { cursor } });
        assert.equal(r.status, 400, `${path} cursor ${cursor}: ${r.text}`);
        assert.deepEqual(r.json, { error: "input.invalid", field: "cursor" });
      }
      const ok = await api(path, AS.ownerSession, { query: { limit: "100" } });
      assert.equal(ok.status, 200, ok.text);
    }
  });

  test("users: every user by id, paged, a page past the end is empty", async () => {
    const all = await pages("/api/v1/users", "users", 1, AS.ownerToken);
    assert.deepEqual(all.map((p) => p.length), [1, 1, 1, 1]);
    const rows = all.flat();
    assert.deepEqual(rows.map((u) => u.id), [1, 2, 3, 4]);
    assert.deepEqual(rows[0], {
      id: 1,
      login: "olga",
      slug: "olga",
      display_name: "Olga",
      bio: "Owner bio",
      role: "owner",
      disabled_at: null,
      created_at: T,
      updated_at: T + 1,
    });
    assert.equal(rows[2].disabled_at, T + 4);
    const past = await api("/api/v1/users", AS.ownerSession, { query: { cursor: Buffer.from("4").toString("base64url") } });
    assert.equal(past.status, 200, past.text);
    assert.deepEqual(past.json, { users: [], next_cursor: null });
  });

  test("users: the raw JSON holds no password hash, session, passkey or token data", async () => {
    const r = await api("/api/v1/users", AS.ownerSession);
    assert.equal(r.status, 200, r.text);
    assert.equal(r.text.includes("password"), false, r.text);
    assert.equal(r.text.includes(PASSWORD_HASH), false, "hash text leaked");
    assert.equal(r.text.includes("pbkdf2"), false, "hash text leaked");
    for (const s of [ownerSession, writerSession, readToken, writerToken, ownerToken, writerOwnerScopeToken, disabledSession, disabledToken]) {
      assert.equal(r.text.includes(s), false, "a secret leaked");
      assert.equal(r.text.includes(sha(s)), false, "a secret hash leaked");
    }
    for (const u of r.json.users) {
      assert.deepEqual(Object.keys(u), ["id", "login", "slug", "display_name", "bio", "role", "disabled_at", "created_at", "updated_at"]);
    }
  });

  test("comments: limit=100 with more than 100 rows still gives a next_cursor", async () => {
    w.exec(`WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 101)
      INSERT INTO comments (post_id, author_name, body, status, created_at)
      SELECT ${postId}, 'Flood', 'x', 'spam', ${T} + i FROM n;`);
    const first = await api("/api/v1/comments", AS.ownerSession, { query: { limit: "100" } });
    assert.equal(first.status, 200, first.text);
    assert.equal(first.json.comments.length, 100);
    assert.notEqual(first.json.next_cursor, null);
    const rest = await api("/api/v1/comments", AS.ownerSession, { query: { limit: "100", cursor: first.json.next_cursor } });
    assert.equal(rest.json.comments.length, 6, "5 seeded + 101 flood = 106");
    assert.equal(rest.json.next_cursor, null);
  });
});
