// POST /api/v1/media/{id}/variants (issue #13) against the real Worker with local D1 and R2:
// one admin-encoded AVIF or WebP size per call, checked like an upload, stored next to the
// original, kept under parallel adds, and removed with the item.
import assert from "node:assert/strict";
import { createHash, randomBytes } from "node:crypto";
import { readFileSync } from "node:fs";
import { after, before, describe, test } from "node:test";

import { startWorker } from "./harness.mjs";

const COOKIE = "__Host-plumblog_session";
const DAY = 86_400_000;
const T = Date.now();
const MAX_VARIANT_BODY = 8 * 1024 * 1024;
const sha256hex = (s) => createHash("sha256").update(s).digest("hex");
const b64u = (buf) => Buffer.from(buf).toString("base64url");
const sq = (s) => `'${String(s).replaceAll("'", "''")}'`;
const newSecret = () => `plb_${b64u(randomBytes(32))}`;

// Real encoder output (avifenc, cwebp) of one 480x360 gradient, plus a 960x720 one.
const fixture = (name) => readFileSync(new URL(`./fixtures/${name}`, import.meta.url));
const AVIF480 = fixture("variant-480.avif");
const AVIF480_OTHER = fixture("variant-480-other.avif");
const WEBP480 = fixture("variant-480.webp");
const AVIF960 = fixture("variant-960.avif");

const u32 = (n) => {
  const b = Buffer.alloc(4);
  b.writeUInt32BE(n);
  return b;
};
/** JPEG headers only: the original the sizes are made from. */
function jpeg(w, h) {
  const sof = Buffer.from([0xff, 0xc0, 0, 11, 8, h >> 8, h & 255, w >> 8, w & 255, 1, 1, 0x11, 0]);
  const app0 = Buffer.from([0xff, 0xe0, 0, 16, 0x4a, 0x46, 0x49, 0x46, 0, 1, 1, 0, 0, 1, 0, 1, 0, 0]);
  return Buffer.concat([Buffer.from([0xff, 0xd8]), app0, sof, Buffer.from([0xff, 0xd9])]);
}
/** PNG signature and IHDR: an original that carries no orientation. */
const png = (w, h) => Buffer.concat([Buffer.from("\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR", "latin1"), u32(w), u32(h), Buffer.from([8, 2, 0, 0, 0])]);
/** An extended WebP header (`VP8X`): the canvas size only. */
function webp(w, h) {
  const b = Buffer.alloc(30);
  b.write("RIFF", 0);
  b.write("WEBPVP8X", 8);
  b.writeUInt32LE(10, 16);
  b.writeUIntLE(w - 1, 24, 3);
  b.writeUIntLE(h - 1, 27, 3);
  return b;
}
/** AVIF boxes up to `ispe`: enough for the header check, for the many widths of the cap test. */
function avif(w, h) {
  const box = (type, ...parts) => {
    const body = Buffer.concat(parts);
    return Buffer.concat([u32(8 + body.length), Buffer.from(type), body]);
  };
  return Buffer.concat([
    box("ftyp", Buffer.from("avif"), u32(0), Buffer.from("avifmif1miaf")),
    box("meta", u32(0), box("iprp", box("ipco", box("ispe", u32(0), u32(w), u32(h))))),
  ]);
}

async function api(w, method, path, { body, raw, as = {} } = {}) {
  const h = { origin: w.url };
  if (as.cookie) h.cookie = `${COOKIE}=${as.cookie}`;
  if (as.bearer) h.authorization = `Bearer ${as.bearer}`;
  let payload = raw;
  if (body !== undefined) {
    h["content-type"] = "application/json";
    payload = JSON.stringify(body);
  }
  const res = await fetch(w.url + path, { method, headers: h, body: payload });
  const text = await res.text();
  return { status: res.status, text, json: text ? JSON.parse(text) : null };
}

function assertError(r, status, code, field) {
  assert.equal(r.status, status, r.text);
  assert.deepEqual(r.json, field === undefined ? { error: code } : { error: code, field });
}

const S = { owner: b64u(randomBytes(32)), writer: b64u(randomBytes(32)), writer2: b64u(randomBytes(32)) };
const TOK = { read: newSecret(), writer: newSecret() };
const AS = {
  owner: { cookie: S.owner },
  writer: { cookie: S.writer },
  writer2: { cookie: S.writer2 },
  readToken: { bearer: TOK.read },
  writerToken: { bearer: TOK.writer },
};

let w;
before(async () => {
  const seed = [
    `INSERT INTO users (id, login, slug, display_name, role, password_hash, disabled_at, created_at, updated_at) VALUES
      (1, 'olga', 'olga', 'Olga', 'owner', NULL, NULL, ${T}, ${T}),
      (2, 'will', 'will', 'Will', 'writer', NULL, NULL, ${T}, ${T}),
      (3, 'wren', 'wren', 'Wren', 'writer', NULL, NULL, ${T}, ${T});`,
    `INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES
      (${sq(sha256hex(S.owner))}, 1, ${T}, ${T + 29 * DAY}),
      (${sq(sha256hex(S.writer))}, 2, ${T}, ${T + 29 * DAY}),
      (${sq(sha256hex(S.writer2))}, 3, ${T}, ${T + 29 * DAY});`,
    `INSERT INTO api_tokens (id, user_id, name, token_hash, scope, created_at, expires_at, revoked_at) VALUES
      (11, 1, 'reader', ${sq(sha256hex(TOK.read))}, 'read', ${T}, NULL, NULL),
      (12, 2, 'agent', ${sq(sha256hex(TOK.writer))}, 'writer', ${T}, NULL, NULL);`,
  ].join("\n");
  w = await startWorker({ seedSql: seed });
});

after(async () => {
  await w?.stop();
});

let n = 0;
/** A new original uploaded by `as` (JPEG, 1600x1200 unless given); its id and R2 key. */
async function original(as, name = `heron-${++n}.jpg`, width = 1600, height = 1200, mime = "image/jpeg") {
  const q = new URLSearchParams({ name, mime, width, height });
  const raw = mime === "image/png" ? png(width, height) : jpeg(width, height);
  const r = await api(w, "POST", `/api/v1/media?${q}`, { as, raw });
  assert.equal(r.status, 201, r.text);
  return { id: r.json.id, key: w.query(`SELECT r2_key FROM media WHERE id = ${r.json.id}`)[0].r2_key };
}

const variantPath = (id, q) => `/api/v1/media/${id}/variants?${new URLSearchParams(q)}`;
/** Adds `bytes` as a size of item `id`; `q` overrides the query fields. */
const add = (as, id, bytes, mime, width, height, q = {}) =>
  api(w, "POST", variantPath(id, { mime, width, height, sha256: sha256hex(bytes), ...q }), { as, raw: bytes });
const version = () => w.query("SELECT content_version AS v FROM site WHERE id = 1")[0].v;
const stored = (id) => JSON.parse(w.query(`SELECT variants_json FROM media WHERE id = ${id}`)[0].variants_json);
const dirOf = (key) => key.slice(0, key.lastIndexOf("/") + 1);
/** The R2 key of a size: next to the original, `<stem>-<width>-<sha256>.<ext>`. */
const vkey = (key, stem, width, bytes, ext) => `${dirOf(key)}${stem}-${width}-${sha256hex(bytes)}.${ext}`;

describe("media variants", () => {
  test("the route is in the reference: raw body, media.upload, owner-or-delete_any, sha256 required", async () => {
    const r = await api(w, "GET", "/api/v1/reference", { as: AS.readToken });
    const v = r.json.routes.find((x) => x.method === "POST" && x.path === "/api/v1/media/{id}/variants");
    assert.equal(v.body, "original");
    assert.equal(v.access, "media.upload");
    assert.deepEqual(v.also, ["media.delete_any"]);
    assert.equal(v.max_body, MAX_VARIANT_BODY);
    assert.equal(v.status, 201);
    assert.equal(v.response, "media");
    assert.deepEqual(v.fields.filter((f) => f.required).map((f) => f.name).sort(), ["height", "id", "mime", "sha256", "width"]);
  });

  test("a writer and the owner add sizes to their own items: 201, row, R2 object next to the original", async () => {
    for (const as of [AS.writer, AS.owner]) {
      const { id, key } = await original(as, "Heron Photo.jpg");
      const r = await add(as, id, AVIF480, "image/avif", 480, 360);
      assert.equal(r.status, 201, r.text);
      const vk = vkey(key, "heron-photo", 480, AVIF480, "avif");
      const entry = { key: vk, width: 480, height: 360, bytes: AVIF480.length, mime: "image/avif", sha256: sha256hex(AVIF480) };
      assert.deepEqual(r.json.variants, [entry]);
      assert.deepEqual(r.json.usage, []);
      assert.deepEqual(stored(id), [entry]);
      assert.equal(w.r2Size(vk), AVIF480.length);
      // The original is untouched.
      assert.equal(w.r2Size(key), jpeg(1600, 1200).length);
      const webp = await add(as, id, WEBP480, "image/webp", 480, 360);
      assert.equal(webp.status, 201, webp.text);
      assert.deepEqual(stored(id).map((v) => v.key), [vk, vkey(key, "heron-photo", 480, WEBP480, "webp")]);
    }
    // A writer token works like the session.
    const { id } = await original(AS.writerToken);
    assert.equal((await add(AS.writerToken, id, AVIF480, "image/avif", 480, 360)).status, 201);
  });

  test("/media/<id>/<stem>-480-<sha256>.avif serves the stored size once a published post uses the item", async () => {
    const { id } = await original(AS.owner, "Kingfisher.jpg");
    assert.equal((await add(AS.owner, id, AVIF480, "image/avif", 480, 360)).status, 201);
    const name = stored(id)[0].key.split("/").pop();
    assert.equal(name, `kingfisher-480-${sha256hex(AVIF480)}.avif`);
    const url = `${w.url}/media/${id}/${name}`;
    assert.equal((await fetch(url)).status, 404, "not public while no published post uses it");
    const post = await api(w, "POST", "/api/v1/posts", {
      as: AS.owner,
      body: { lang: "en", slug: `kingfisher-${id}`, title: "Kingfisher", markdown: `![A kingfisher](/media/${id}/${name})`, status: "published" },
    });
    assert.equal(post.status, 201, post.text);
    const res = await fetch(url);
    assert.equal(res.status, 200);
    assert.equal(res.headers.get("content-type"), "image/avif");
    assert.deepEqual(Buffer.from(await res.arrayBuffer()), AVIF480);
  });

  test("another writer's item is 403 and writes nothing; the owner may add to it", async () => {
    const { id, key } = await original(AS.writer);
    assertError(await add(AS.writer2, id, AVIF480, "image/avif", 480, 360), 403, "auth.forbidden");
    assert.deepEqual(stored(id), []);
    assert.equal(w.r2Size(vkey(key, `heron-${n}`, 480, AVIF480, "avif")), null);
    assert.equal((await add(AS.owner, id, AVIF480, "image/avif", 480, 360)).status, 201);
    assert.equal(w.r2Size(vkey(key, `heron-${n}`, 480, AVIF480, "avif")), AVIF480.length);
  });

  test("an item whose uploader was deleted needs the owner", async () => {
    const { id } = await original(AS.writer);
    w.exec(`UPDATE media SET uploaded_by = NULL WHERE id = ${id}`);
    assertError(await add(AS.writer, id, AVIF480, "image/avif", 480, 360), 403, "auth.forbidden");
    assert.equal((await add(AS.owner, id, AVIF480, "image/avif", 480, 360)).status, 201);
  });

  test("a read token is 403, anonymous 401, an unknown id 404", async () => {
    const { id } = await original(AS.writer);
    assertError(await add(AS.readToken, id, AVIF480, "image/avif", 480, 360), 403, "auth.forbidden");
    assertError(await add({}, id, AVIF480, "image/avif", 480, 360), 401, "auth.unauthenticated");
    assertError(await add(AS.owner, 999999, AVIF480, "image/avif", 480, 360), 404, "store.not_found");
    assert.deepEqual(stored(id), []);
  });

  test("bad input is 400 on the field at fault, and stores nothing", async () => {
    const { id, key } = await original(AS.writer);
    const cases = [
      ["upscale (wider than the original)", avif(2000, 1500), "image/avif", 2000, 1500, {}, "width"],
      ["one pixel wider than the original", avif(1601, 1201), "image/avif", 1601, 1201, {}, "width"],
      ["not a standard width", avif(500, 375), "image/avif", 500, 375, {}, "width"],
      ["a standard width wider than the original", avif(2400, 1800), "image/avif", 2400, 1800, {}, "width"],
      ["wrong aspect", avif(480, 400), "image/avif", 480, 400, {}, "height"],
      ["a type that is not AVIF or WebP", jpeg(480, 360), "image/jpeg", 480, 360, {}, "mime"],
      ["magic bytes of another type", WEBP480, "image/avif", 480, 360, {}, "mime"],
      ["size not the header's", AVIF480, "image/avif", 481, 361, {}, "width"],
      ["sha256 not of the body", AVIF480, "image/avif", 480, 360, { sha256: sha256hex("other") }, "sha256"],
      ["sha256 not hex", AVIF480, "image/avif", 480, 360, { sha256: "x".repeat(64) }, "sha256"],
    ];
    for (const [what, bytes, mime, width, height, q, field] of cases) {
      const r = await add(AS.writer, id, bytes, mime, width, height, q);
      assert.equal(r.status, 400, `${what}: ${r.text}`);
      assert.deepEqual(r.json, { error: "input.invalid", field }, what);
    }
    // A missing sha256 is refused by the route table.
    const r = await api(w, "POST", variantPath(id, { mime: "image/avif", width: 480, height: 360 }), { as: AS.writer, raw: AVIF480 });
    assertError(r, 400, "input.invalid", "sha256");
    assert.deepEqual(stored(id), []);
    assert.equal(w.r2Size(vkey(key, `heron-${n}`, 480, AVIF480, "avif")), null);
    // Within one pixel of the original's aspect is fine (1600x1200 at 480 is 360 exactly).
    assert.equal((await add(AS.writer, id, avif(480, 361), "image/avif", 480, 361)).status, 201);
    // The original's own width is a format conversion, not an upscale.
    assert.equal((await add(AS.writer, id, avif(1600, 1200), "image/avif", 1600, 1200)).status, 201);
    // An odd original width is allowed as itself, never as a resize target.
    const odd = await original(AS.writer, "odd.jpg", 1234, 926);
    assert.equal((await add(AS.writer, odd.id, avif(1234, 926), "image/avif", 1234, 926)).status, 201);
    assert.equal((await add(AS.writer, odd.id, avif(960, 720), "image/avif", 960, 720)).status, 201);
    assertError(await add(AS.writer, odd.id, avif(1233, 925), "image/avif", 1233, 925), 400, "input.invalid", "width");
  });

  test("only JPEG and AVIF originals take a quarter-turned size: a PNG keeps its own aspect", async () => {
    const { id } = await original(AS.writer, "flat.png", 4000, 3000, "image/png");
    assertError(await add(AS.writer, id, avif(960, 1280), "image/avif", 960, 1280), 400, "input.invalid", "height");
    assert.equal((await add(AS.writer, id, avif(960, 720), "image/avif", 960, 720)).status, 201);
    assert.equal(stored(id).length, 1);
  });

  test("an original stored a quarter turned (3x4 header) takes sizes in display order (4x3)", async () => {
    const q = new URLSearchParams({ name: "turned.jpg", mime: "image/jpeg", width: 1200, height: 1600 });
    const up = await api(w, "POST", `/api/v1/media?${q}`, { as: AS.writer, raw: jpeg(1200, 1600) });
    assert.equal(up.status, 201, up.text);
    const id = up.json.id;
    assert.equal((await add(AS.writer, id, AVIF480, "image/avif", 480, 360)).status, 201);
    // One size per (mime, width): the portrait one goes in the other type.
    assert.equal((await add(AS.writer, id, webp(480, 640), "image/webp", 480, 640)).status, 201);
    // Never wider than the matching orientation: 4x3 is at most 1600 wide, 3x4 at most 1200.
    assert.equal((await add(AS.writer, id, avif(1600, 1200), "image/avif", 1600, 1200)).status, 201);
    assertError(await add(AS.writer, id, avif(1604, 1203), "image/avif", 1604, 1203), 400, "input.invalid", "width");
    assertError(await add(AS.writer, id, avif(1204, 1605), "image/avif", 1204, 1605), 400, "input.invalid", "width");
    assertError(await add(AS.writer, id, avif(480, 480), "image/avif", 480, 480), 400, "input.invalid", "height");
    assert.equal(stored(id).length, 3);
  });

  test("the same bytes again are 200 with the item; other bytes for that mime and width are 409", async () => {
    const { id, key } = await original(AS.writer);
    assert.equal((await add(AS.writer, id, AVIF480, "image/avif", 480, 360)).status, 201);
    const again = await add(AS.writer, id, AVIF480, "image/avif", 480, 360);
    assert.equal(again.status, 200, again.text);
    assert.equal(again.json.id, id);
    assert.equal(again.json.variants.length, 1);
    assert.equal(stored(id).length, 1);
    assertError(await add(AS.writer, id, AVIF480_OTHER, "image/avif", 480, 360), 409, "store.conflict", "width");
    assert.equal(stored(id)[0].sha256, sha256hex(AVIF480));
    // The object still holds the first bytes.
    assert.equal(w.r2Size(vkey(key, `heron-${n}`, 480, AVIF480, "avif")), AVIF480.length);
  });

  test("at most 8 sizes: the 9th is 409 and stores nothing", async () => {
    // 3000 px wide: five allowed widths in two types, so the cap, not the widths, refuses.
    const { id, key } = await original(AS.writer, `heron-${++n}.jpg`, 3000, 2250);
    const sizes = [480, 960, 1600, 2400].flatMap((x) => [
      [avif(x, x * 0.75), "image/avif", x],
      [webp(x, x * 0.75), "image/webp", x],
    ]);
    for (const [bytes, mime, x] of sizes) {
      const r = await add(AS.writer, id, bytes, mime, x, x * 0.75);
      assert.equal(r.status, 201, `${mime} ${x}: ${r.text}`);
    }
    assertError(await add(AS.writer, id, avif(3000, 2250), "image/avif", 3000, 2250), 409, "store.conflict", "variants");
    assert.equal(stored(id).length, 8);
    assert.equal(w.r2Size(vkey(key, `heron-${n}`, 3000, avif(3000, 2250), "avif")), null);
  });

  test("a body over the 8 MiB cap is 413 before access or the body are looked at", async () => {
    const { id } = await original(AS.writer);
    const big = Buffer.alloc(MAX_VARIANT_BODY + 1);
    AVIF480.copy(big);
    for (const as of [AS.writer, AS.writer2, {}]) {
      assertError(await add(as, id, big, "image/avif", 480, 360), 413, "api.body_too_large");
    }
    assert.deepEqual(stored(id), []);
  });

  test("a path id that is not an id is 404", async () => {
    // Refused before the body is read (a unit test in api/v1/mod.rs proves the order:
    // wrangler dev takes in the whole body before the Worker answers, so HTTP cannot show it).
    for (const bad of ["abc", "0", "-1"]) {
      assertError(await add(AS.writer, bad, AVIF480, "image/avif", 480, 360), 404, "store.not_found");
    }
  });

  test("a size bumps the content version only when a published post uses the item", async () => {
    const { id } = await original(AS.owner);
    const v = version();
    assert.equal((await add(AS.owner, id, AVIF480, "image/avif", 480, 360)).status, 201);
    assert.equal(version(), v, "unused: on no reader page");
    const name = stored(id)[0].key.split("/").pop();
    const post = await api(w, "POST", "/api/v1/posts", {
      as: AS.owner,
      body: { lang: "en", slug: `bump-${id}`, title: "Bump", markdown: `![x](/media/${id}/${name})`, status: "published" },
    });
    assert.equal(post.status, 201, post.text);
    const v2 = version();
    assert.equal((await add(AS.owner, id, WEBP480, "image/webp", 480, 360)).status, 201);
    assert.equal(version(), v2 + 1, "in a published post's srcset");
    // The same bytes again write nothing, so nothing to bump.
    assert.equal((await add(AS.owner, id, WEBP480, "image/webp", 480, 360)).status, 200);
    assert.equal(version(), v2 + 1);
  });

  test("two sizes added at once are both kept", async () => {
    for (let round = 0; round < 3; round++) {
      const { id, key } = await original(AS.writer);
      const rs = await Promise.all([
        add(AS.writer, id, AVIF480, "image/avif", 480, 360),
        add(AS.writer, id, WEBP480, "image/webp", 480, 360),
        add(AS.writer, id, AVIF960, "image/avif", 960, 720),
      ]);
      assert.deepEqual(rs.map((r) => r.status), [201, 201, 201], rs.map((r) => r.text).join("\n"));
      const got = stored(id).map((v) => `${v.mime} ${v.width}`).sort();
      assert.deepEqual(got, ["image/avif 480", "image/avif 960", "image/webp 480"]);
      for (const v of stored(id)) assert.ok(w.r2Size(v.key) > 0, v.key);
      assert.ok(dirOf(stored(id)[0].key) === dirOf(key));
    }
  });

  test("two encodings of one mime and width at once: one 201, one 409; the row, the served bytes and R2 agree", async () => {
    for (let round = 0; round < 3; round++) {
      const { id, key } = await original(AS.owner);
      const rs = await Promise.all([
        add(AS.owner, id, AVIF480, "image/avif", 480, 360),
        add(AS.owner, id, AVIF480_OTHER, "image/avif", 480, 360),
      ]);
      assert.deepEqual(rs.map((r) => r.status).sort(), [201, 409], rs.map((r) => r.text).join("\n"));
      // The order cannot be forced: whichever won, the row names the winner's bytes.
      const [winner, loser] = rs[0].status === 201 ? [AVIF480, AVIF480_OTHER] : [AVIF480_OTHER, AVIF480];
      assert.deepEqual(rs.find((r) => r.status === 409).json, { error: "store.conflict", field: "width" });
      const row = stored(id);
      assert.equal(row.length, 1);
      assert.equal(row[0].sha256, sha256hex(winner));
      assert.equal(row[0].key, vkey(key, `heron-${n}`, 480, winner, "avif"));
      assert.equal(w.r2Size(row[0].key), winner.length);
      assert.equal(w.r2Size(vkey(key, `heron-${n}`, 480, loser, "avif")), null, "the loser's object is removed");
      if (round === 0) {
        const post = await api(w, "POST", "/api/v1/posts", {
          as: AS.owner,
          body: { lang: "en", slug: `race-${id}`, title: "Race", markdown: `![x](/media/${id}/${row[0].key.split("/").pop()})`, status: "published" },
        });
        assert.equal(post.status, 201, post.text);
        const res = await fetch(`${w.url}/media/${id}/${row[0].key.split("/").pop()}`);
        assert.equal(res.status, 200);
        const got = Buffer.from(await res.arrayBuffer());
        assert.equal(sha256hex(got), row[0].sha256);
      }
    }
  });

  test("the same size sent twice at once is one size", async () => {
    const { id } = await original(AS.writer);
    const rs = await Promise.all([1, 2].map(() => add(AS.writer, id, AVIF480, "image/avif", 480, 360)));
    assert.deepEqual(rs.map((r) => r.status).sort(), [200, 201], rs.map((r) => r.text).join("\n"));
    assert.equal(stored(id).length, 1);
  });

  test("deleting the item removes its size objects too", async () => {
    const { id, key } = await original(AS.writer);
    for (const [bytes, mime, width, height] of [
      [AVIF480, "image/avif", 480, 360],
      [WEBP480, "image/webp", 480, 360],
    ]) {
      assert.equal((await add(AS.writer, id, bytes, mime, width, height)).status, 201);
    }
    const keys = stored(id).map((v) => v.key);
    for (const k of [key, ...keys]) assert.ok(w.r2Size(k) > 0, k);
    const del = await api(w, "DELETE", `/api/v1/media/${id}`, { as: AS.writer });
    assert.equal(del.status, 204, del.text);
    for (const k of [key, ...keys]) assert.equal(w.r2Size(k), null, k);
    // A size for a deleted item is 404 and leaves no object.
    assertError(await add(AS.writer, id, AVIF960, "image/avif", 960, 720), 404, "store.not_found");
    assert.equal(w.r2Size(vkey(key, `heron-${n}`, 960, AVIF960, "avif")), null);
  });
});
