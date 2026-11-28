// `/media/<id>/<name>` on the public site (issue #62): who may read which item, the cache
// rule, HEAD, Range, If-None-Match, variants by file name, items that must never be served,
// and that no request writes to D1 or sets a cookie.
import assert from "node:assert/strict";
import { createHash, randomBytes } from "node:crypto";
import { deflateSync } from "node:zlib";
import { after, before, describe, test } from "node:test";

import { startWorker } from "./harness.mjs";

const COOKIE = "__Host-plumblog_session";
const HOUR = 3_600_000;
const DAY = 24 * HOUR;
const T = Date.now();
const sha = (s) => createHash("sha256").update(s).digest("hex");
const secret = () => `plb_${randomBytes(32).toString("base64url")}`;

const ownerSession = randomBytes(32).toString("base64url");
// Less than half its lifetime left: an `/api/` request would renew it (one write).
const agingSession = randomBytes(32).toString("base64url");
const readToken = secret();
const writerToken = secret();
const AS = {
  anonymous: {},
  ownerSession: { cookie: `${COOKIE}=${ownerSession}` },
  agingSession: { cookie: `${COOKIE}=${agingSession}` },
  deadSession: { cookie: `${COOKIE}=${randomBytes(32).toString("base64url")}` },
  readToken: { authorization: `Bearer ${readToken}` },
  badToken: { authorization: `Bearer ${secret()}` },
  writerToken: { authorization: `Bearer ${writerToken}` },
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
before(async () => {
  w = await startWorker({
    seedSql: [
      `INSERT INTO users (id, login, slug, display_name, bio, role, password_hash, disabled_at, created_at, updated_at) VALUES
        (1, 'olga', 'olga', 'Olga', '', 'owner', NULL, NULL, ${T}, ${T}),
        (2, 'will', 'will', 'Will', '', 'writer', NULL, NULL, ${T}, ${T});`,
      `INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES
        ('${sha(ownerSession)}', 1, ${T}, ${T + 20 * DAY}),
        ('${sha(agingSession)}', 2, ${T - 20 * DAY}, ${T + 2 * DAY});`,
      `INSERT INTO api_tokens (id, user_id, name, token_hash, scope, created_at, expires_at, revoked_at) VALUES
        (11, 1, 'reader', '${sha(readToken)}', 'read', ${T}, NULL, NULL),
        (12, 2, 'writer', '${sha(writerToken)}', 'writer', ${T}, NULL, NULL);`,
    ].join("\n"),
  });
});
after(async () => {
  await w?.stop();
});

async function get(path, as = AS.anonymous, { method = "GET", headers = {} } = {}) {
  const res = await fetch(w.url + path, { method, headers: { ...as, ...headers } });
  return { status: res.status, headers: res.headers, bytes: Buffer.from(await res.arrayBuffer()) };
}

/** Waits (bounded) for wrangler's output to show `re`: it reaches the pipe after the response. */
async function logged(re, ms = 5_000) {
  const end = Date.now() + ms;
  while (!re.test(w.log())) {
    if (Date.now() > end) assert.match(w.log(), re);
    await new Promise((ok) => setTimeout(ok, 50));
  }
}

/** Every user table's rows, as text: equal snapshots mean nothing was written. */
function snapshot() {
  const tables = w
    .query("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '_cf_%' ORDER BY name")
    .map((r) => r.name);
  return JSON.stringify(tables.map((t) => [t, w.query(`SELECT * FROM "${t}"`)]));
}

const files = {};
async function upload(label, name) {
  const bytes = png(24, 16);
  const q = new URLSearchParams({ name, mime: "image/png", width: 24, height: 16 });
  const res = await fetch(`${w.url}/api/v1/media?${q}`, {
    method: "POST",
    headers: { ...AS.writerToken, "content-type": "application/octet-stream" },
    body: bytes,
  });
  const json = await res.json();
  assert.equal(res.status, 201, JSON.stringify(json));
  files[label] = { id: json.id, src: json.src, bytes };
}

const PUBLISHED_HEADERS = {
  "content-type": "image/png",
  "x-content-type-options": "nosniff",
  "content-security-policy": "sandbox",
  "accept-ranges": "bytes",
  "cache-control": "public, max-age=31536000, immutable",
};

describe("/media on the public site", () => {
  test("seed: uploads, and posts of every status using them", async () => {
    for (const [label, name] of [
      ["body", "Heron.png"],
      ["cover", "cover.png"],
      ["page", "page.png"],
      ["draft", "draft.png"],
      ["scheduled", "soon.png"],
      ["trashed", "gone.png"],
      ["unused", "unused.png"],
      ["svg", "evil.png"],
      ["short", "short.png"],
      ["lost", "lost.png"],
      ["variant", "heron-480.png"],
    ]) {
      await upload(label, name);
    }
    const id = (l) => files[l].id;
    const post = (pid, slug, status, extra = {}) => {
      const published = status === "draft" ? "NULL" : status === "scheduled" ? T + 10 * DAY : T - HOUR;
      const kind = extra.kind ?? "post";
      const cover = extra.cover ?? "NULL";
      const from = extra.trashedFrom ? `'${extra.trashedFrom}'` : "NULL";
      return `(${pid}, '${kind}', ${pid}, 'en', '${slug}', '${status}', ${from}, ${published}, 1, 'T', 'm', 'h', ${cover}, ${T}, ${T})`;
    };
    w.exec(`INSERT INTO posts (id, kind, translation_group, lang, slug, status, trashed_from, published_at, author_id, title, markdown, html, cover_media_id, created_at, updated_at) VALUES
      ${post(1, "live", "published", { cover: id("cover") })},
      ${post(2, "about", "published", { kind: "page" })},
      ${post(3, "draft", "draft")},
      ${post(4, "soon", "scheduled", { cover: id("scheduled") })},
      ${post(5, "gone", "trashed", { trashedFrom: "published" })};
    INSERT INTO post_media (post_id, media_id) VALUES
      (1, ${id("body")}), (1, ${id("svg")}), (1, ${id("short")}), (1, ${id("lost")}),
      (2, ${id("page")}), (3, ${id("draft")}), (5, ${id("trashed")});
    UPDATE media SET mime = 'image/svg+xml' WHERE id = ${id("svg")};
    UPDATE media SET bytes = bytes + 1 WHERE id = ${id("short")};
    UPDATE media SET r2_key = 'media/no-such-object/lost.png' WHERE id = ${id("lost")};`);
    // What the admin encoder (#13) will write: a variant entry whose key names its own object.
    const [v] = w.query(`SELECT r2_key, bytes FROM media WHERE id = ${id("variant")}`);
    const variants = JSON.stringify([{ key: v.r2_key, width: 24, height: 16, bytes: v.bytes, mime: "image/png" }]);
    w.exec(`UPDATE media SET variants_json = '${variants}' WHERE id = ${id("body")};`);
  });

  test("a published post's body image: 200, the stored bytes, every header, no cookie", async () => {
    const t0 = performance.now();
    const r = await get(files.body.src);
    const ms = performance.now() - t0;
    console.log(`# anonymous /media hit: ${ms.toFixed(1)} ms wall (wrangler dev, local)`);
    assert.equal(r.status, 200);
    assert.ok(r.bytes.equals(files.body.bytes), "bytes unchanged");
    for (const [k, v] of Object.entries(PUBLISHED_HEADERS)) assert.equal(r.headers.get(k), v, k);
    assert.equal(r.headers.get("content-length"), String(files.body.bytes.length));
    assert.match(r.headers.get("etag") ?? "", /^"[^"]+"$/);
    assert.equal(r.headers.get("set-cookie"), null);
  });

  test("the id alone and the original's file name are the same item", async () => {
    const r = await get(`/media/${files.body.id}`);
    assert.equal(r.status, 200);
    assert.ok(r.bytes.equals(files.body.bytes));
  });

  test("a variant is served by the file name of its key", async () => {
    const r = await get(`/media/${files.body.id}/heron-480.png`);
    assert.equal(r.status, 200);
    assert.ok(r.bytes.equals(files.variant.bytes), "the variant's own bytes");
    assert.equal(r.headers.get("cache-control"), PUBLISHED_HEADERS["cache-control"]);
  });

  test("a published post's cover and a published page's image: 200", async () => {
    for (const l of ["cover", "page"]) {
      const r = await get(files[l].src);
      assert.equal(r.status, 200, l);
      assert.ok(r.bytes.equals(files[l].bytes), l);
    }
  });

  test("draft-only, scheduled, trashed, unused and unknown: the same empty 404 for anonymous", async () => {
    const answers = [];
    for (const path of [
      files.draft.src,
      files.scheduled.src,
      files.trashed.src,
      files.unused.src,
      `/media/${files.variant.id + 1000}/x.png`,
    ]) {
      const r = await get(path);
      assert.equal(r.status, 404, path);
      assert.equal(r.bytes.length, 0, path);
      assert.equal(r.headers.get("cache-control"), "no-store", path);
      answers.push([...r.headers].filter(([k]) => !["date", "cf-ray"].includes(k)));
    }
    for (const a of answers) assert.deepEqual(a, answers[0], "nothing tells them apart");
  });

  test("an owner session and a read token see a draft's image, private and uncached", async () => {
    for (const who of ["ownerSession", "readToken", "agingSession"]) {
      const r = await get(files.draft.src, AS[who]);
      assert.equal(r.status, 200, who);
      assert.ok(r.bytes.equals(files.draft.bytes), who);
      assert.equal(r.headers.get("cache-control"), "private, no-store", who);
      assert.equal(r.headers.get("set-cookie"), null, who);
    }
  });

  test("a bad token or dead session on a draft's image is the anonymous 404, and is no key to public ones", async () => {
    for (const who of ["badToken", "deadSession"]) {
      const r = await get(files.draft.src, AS[who]);
      assert.equal(r.status, 404, who);
      assert.equal(r.headers.get("set-cookie"), null, who);
      const pub = await get(files.body.src, AS[who]);
      assert.equal(pub.status, 200, who);
      assert.equal(pub.headers.get("cache-control"), PUBLISHED_HEADERS["cache-control"], who);
    }
  });

  test("HEAD: the GET headers, no body", async () => {
    const g = await get(files.body.src);
    const r = await get(files.body.src, AS.anonymous, { method: "HEAD" });
    assert.equal(r.status, 200);
    assert.equal(r.bytes.length, 0);
    for (const k of [...Object.keys(PUBLISHED_HEADERS), "etag", "content-length"]) {
      assert.equal(r.headers.get(k), g.headers.get(k), k);
    }
    assert.equal((await get(files.draft.src, AS.anonymous, { method: "HEAD" })).status, 404);
    const etag = g.headers.get("etag");
    const nm = await get(files.body.src, AS.anonymous, { method: "HEAD", headers: { "if-none-match": etag } });
    assert.equal(nm.status, 304);
  });

  test("Range: a-b, a-, -n give 206 with Content-Range; past the end 416; others the full body", async () => {
    const all = files.body.bytes;
    const n = all.length;
    for (const [range, start, end] of [
      ["bytes=0-9", 0, 9],
      ["bytes=10-", 10, n - 1],
      ["bytes=-7", n - 7, n - 1],
      [`bytes=5-${n + 50}`, 5, n - 1],
    ]) {
      const r = await get(files.body.src, AS.anonymous, { headers: { range } });
      assert.equal(r.status, 206, range);
      assert.equal(r.headers.get("content-range"), `bytes ${start}-${end}/${n}`, range);
      assert.equal(r.headers.get("content-length"), String(end - start + 1), range);
      assert.ok(r.bytes.equals(all.subarray(start, end + 1)), range);
      assert.equal(r.headers.get("cache-control"), PUBLISHED_HEADERS["cache-control"], range);
    }
    for (const range of [`bytes=${n}-`, "bytes=-0"]) {
      const r = await get(files.body.src, AS.anonymous, { headers: { range } });
      assert.equal(r.status, 416, range);
      assert.equal(r.headers.get("content-range"), `bytes */${n}`, range);
      assert.equal(r.bytes.length, 0, range);
    }
    for (const range of ["bytes=0-1,4-5", "bytes=9-2", "pages=1-2", "bytes=x-"]) {
      const r = await get(files.body.src, AS.anonymous, { headers: { range } });
      assert.equal(r.status, 200, range);
      assert.ok(r.bytes.equals(all), range);
    }
  });

  test("If-None-Match with the ETag: 304, no body; another tag: 200", async () => {
    const etag = (await get(files.body.src)).headers.get("etag");
    const r = await get(files.body.src, AS.anonymous, { headers: { "if-none-match": etag } });
    assert.equal(r.status, 304);
    assert.equal(r.bytes.length, 0);
    assert.equal(r.headers.get("etag"), etag);
    assert.equal(r.headers.get("cache-control"), PUBLISHED_HEADERS["cache-control"]);
    const weak = await get(files.body.src, AS.anonymous, { headers: { "if-none-match": `W/${etag}` } });
    assert.equal(weak.status, 304, "weak comparison, as RFC 9110 asks for If-None-Match");
    const other = await get(files.body.src, AS.anonymous, { headers: { "if-none-match": '"nope"' } });
    assert.equal(other.status, 200);
    assert.ok(other.bytes.equals(files.body.bytes));
    // If-None-Match is decided before Range (RFC 9110 13.2.2).
    const past = `bytes=${files.body.bytes.length}-`;
    const held = await get(files.body.src, AS.anonymous, { headers: { "if-none-match": etag, range: past } });
    assert.equal(held.status, 304);
    const stale = await get(files.body.src, AS.anonymous, { headers: { "if-none-match": '"nope"', range: past } });
    assert.equal(stale.status, 416);
  });

  test("a wrong file name, a non-numeric or non-canonical id, extra segments: 404", async () => {
    const id = files.body.id;
    for (const path of [
      `/media/${id}/other.png`,
      `/media/${id}/heron.PNG`,
      "/media/abc/heron.png",
      `/media/0${id}/heron.png`,
      `/media/${id}.png`,
      `/media/${id}/heron.png/x`,
      `/media/${id}/`,
      "/media/",
    ]) {
      const r = await get(path);
      assert.equal(r.status, 404, path);
      assert.equal(r.bytes.length, 0, path);
      assert.equal(r.headers.get("cache-control"), "no-store", path);
    }
  });

  test("POST, PUT and DELETE are 405", async () => {
    for (const method of ["POST", "PUT", "DELETE"]) {
      const r = await fetch(w.url + files.body.src, { method, headers: { origin: w.url } });
      assert.equal(r.status, 405, method);
    }
  });

  test("a row whose mime is not an image type, or whose size is wrong, is never served: 500", async () => {
    for (const l of ["svg", "short"]) {
      for (const as of [AS.anonymous, AS.ownerSession]) {
        const r = await get(files[l].src, as);
        assert.equal(r.status, 500, l);
        assert.equal(r.bytes.length, 0, l);
      }
    }
    await logged(/media \d+ failed: media\/\S+: stored mime "image\/svg\+xml" is not an accepted image type/);
    await logged(/media \d+ failed: R2 object \S+ is \d+ bytes, row says \d+/);
  });

  test("a row whose R2 object is gone: 404, logged with the key", async () => {
    const r = await get(files.lost.src);
    assert.equal(r.status, 404);
    await logged(/\] media \d+: R2 object media\/no-such-object\/lost\.png is missing/);
  });

  test("no D1 write and no cookie across anonymous, session and token requests", async () => {
    const before = snapshot();
    for (const as of Object.values(AS)) {
      for (const f of ["body", "draft", "unused"]) {
        const r = await get(files[f].src, as);
        assert.equal(r.headers.get("set-cookie"), null);
        await get(files[f].src, as, { method: "HEAD" });
        await get(files[f].src, as, { headers: { range: "bytes=0-3" } });
      }
    }
    assert.equal(snapshot(), before, "D1 unchanged");
    // The probe sees writes: the same aging cookie on the API renews its session.
    const me = await fetch(`${w.url}/api/auth/me`, { headers: AS.agingSession });
    assert.equal(me.status, 200);
    assert.ok(me.headers.get("set-cookie"), "the API renews it");
    assert.notEqual(snapshot(), before, "the snapshot catches a write");
  });
});
