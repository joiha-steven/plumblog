// /api/v1 media, redirects, settings, theme/plugin options and plugin state over HTTP against
// the real Worker with local D1 and R2 (issue #10, part 3): uploads per image type, magic
// bytes, size header, body cap, R2 object lifetime, media in use, internal settings, the
// origin rule, and every permission boundary.
import assert from "node:assert/strict";
import { createHash, randomBytes } from "node:crypto";
import { deflateSync } from "node:zlib";
import { after, before, describe, test } from "node:test";

import { startWorker } from "./harness.mjs";

const COOKIE = "__Host-plumblog_session";
const DAY = 86_400_000;
const T = Date.now();
const MAX_MEDIA_BODY = 20 * 1024 * 1024;
const sha256hex = (s) => createHash("sha256").update(s).digest("hex");
const b64u = (buf) => Buffer.from(buf).toString("base64url");
const sq = (s) => `'${String(s).replaceAll("'", "''")}'`;
const newSecret = () => `plb_${b64u(randomBytes(32))}`;

// --- Tiny images, built here so no binary fixture is checked in. ---
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
/** A decodable RGBA PNG of `w` x `h` red pixels. */
function png(w, h) {
  const row = Buffer.concat([Buffer.from([0]), Buffer.alloc(w * 4, Buffer.from([255, 0, 0, 255]))]);
  return Buffer.concat([
    Buffer.from("\x89PNG\r\n\x1a\n", "latin1"),
    chunk("IHDR", Buffer.concat([u32(w), u32(h), Buffer.from([8, 6, 0, 0, 0])])),
    chunk("IDAT", deflateSync(Buffer.concat(Array(h).fill(row)))),
    chunk("IEND", Buffer.alloc(0)),
  ]);
}
// The classic 1x1 transparent GIF.
const GIF = Buffer.from("R0lGODlhAQABAIAAAP///wAAACH5BAEAAAAALAAAAAABAAEAAAICRAEAOw==", "base64");
// A 1x1 lossless WebP.
const WEBP = Buffer.from("UklGRhoAAABXRUJQVlA4TA0AAAAvAAAAEAcQERGIiP4HAA==", "base64");
/** JPEG headers only (SOI, JFIF, baseline frame header, EOI): the Worker reads no further. */
function jpeg(w, h) {
  const sof = Buffer.from([0xff, 0xc0, 0, 11, 8, h >> 8, h & 255, w >> 8, w & 255, 1, 1, 0x11, 0]);
  const app0 = Buffer.from([0xff, 0xe0, 0, 16, 0x4a, 0x46, 0x49, 0x46, 0, 1, 1, 0, 0, 1, 0, 1, 0, 0]);
  return Buffer.concat([Buffer.from([0xff, 0xd8]), app0, sof, Buffer.from([0xff, 0xd9])]);
}
/** AVIF boxes up to the `ispe` property (ftyp, meta > iprp > ipco > ispe). */
function avif(w, h) {
  const box = (type, ...parts) => {
    const body = Buffer.concat(parts);
    return Buffer.concat([u32(8 + body.length), Buffer.from(type), body]);
  };
  const ispe = box("ispe", u32(0), u32(w), u32(h));
  return Buffer.concat([
    box("ftyp", Buffer.from("avif"), u32(0), Buffer.from("avifmif1miaf")),
    box("meta", u32(0), box("iprp", box("ipco", ispe))),
  ]);
}
const IMAGES = [
  ["image/png", png(3, 2), 3, 2, "Red.PNG", "red.png"],
  ["image/gif", GIF, 1, 1, "dot.gif", "dot.gif"],
  ["image/webp", WEBP, 1, 1, "dot.webp", "dot.webp"],
  ["image/jpeg", jpeg(640, 480), 640, 480, "\u1ea2nh ch\u1ee5p 1.jpg", "nh-ch-p-1.jpg"],
  ["image/avif", avif(800, 600), 800, 600, "heron.avif", "heron.avif"],
];

/** One API call. `as` is {cookie} or {bearer}; `origin: null` omits the header. `raw` sends bytes. */
async function api(w, method, path, { body, raw, as = {}, origin, headers = {} } = {}) {
  const h = { ...headers };
  if (origin !== null) h.origin = origin ?? w.url;
  if (as.cookie) h.cookie = `${COOKIE}=${as.cookie}`;
  if (as.bearer) h.authorization = `Bearer ${as.bearer}`;
  let payload = raw;
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

const uploadPath = (q) => `/api/v1/media?${new URLSearchParams(q)}`;
const upload = (w, as, [mime, bytes, width, height, name], extra = {}) =>
  api(w, "POST", uploadPath({ name, mime, width, height, ...extra }), { as, raw: bytes });

const S = { owner: b64u(randomBytes(32)), writer: b64u(randomBytes(32)) };
const TOK = { read: newSecret(), writer: newSecret(), owner: newSecret() };
const AS = {
  ownerSession: { cookie: S.owner },
  writerSession: { cookie: S.writer },
  readToken: { bearer: TOK.read },
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
      (13, 1, 'boss', ${sq(sha256hex(TOK.owner))}, 'owner', ${T}, NULL, NULL);`,
    `INSERT INTO settings (key, value_json, updated_at) VALUES
      ('setup_code_hash', '"deadbeef"', ${T}),
      ('setup_code_expires_at', '${T + DAY}', ${T}),
      ('site_id', '"site-abc"', ${T}),
      ('title', '"My blog"', ${T});`,
  ].join("\n");
  w = await startWorker({ seedSql: seed });
});

after(async () => {
  await w?.stop();
});

const r2Key = (id) => w.query(`SELECT r2_key FROM media WHERE id = ${id}`)[0]?.r2_key;
const settingRow = (key) => w.query(`SELECT value_json FROM settings WHERE key = ${sq(key)}`)[0]?.value_json;

describe("api v1 site", () => {
  test("reference: the upload takes the original as raw bytes; AVIF sizes come from the admin encoder", async () => {
    const r = await api(w, "GET", "/api/v1/reference", { as: AS.readToken });
    const up = r.json.routes.find((x) => x.method === "POST" && x.path === "/api/v1/media");
    assert.equal(up.body, "original");
    assert.equal(up.variants, "admin_encoder");
    assert.equal(up.max_body, MAX_MEDIA_BODY);
    assert.equal(up.access, "media.upload");
    assert.deepEqual(up.fields.filter((f) => f.required).map((f) => f.name).sort(), ["height", "mime", "name", "width"]);
    const put = r.json.routes.find((x) => x.method === "PUT" && x.path === "/api/v1/settings/{key}");
    assert.equal(put.body, "json");
    assert.equal(put.variants, null);
    assert.ok(put.fields.some((f) => f.name === "value" && f.type === "json"));
  });

  for (const [label, as] of [["session", AS.writerSession], ["token", AS.writerToken]]) {
    test(`media: upload every type via ${label}; R2 holds the original; row and object go on delete`, async () => {
      for (const img of IMAGES) {
        const [mime, bytes, width, height, , keyName] = img;
        const r = await upload(w, as, img, { alt: "A thing" });
        assert.equal(r.status, 201, `${mime}: ${r.text}`);
        const m = r.json;
        assert.equal(m.mime, mime);
        assert.equal(m.bytes, bytes.length);
        assert.deepEqual([m.width, m.height, m.alt, m.uploaded_by], [width, height, "A thing", 2]);
        assert.equal(m.src, `/media/${m.id}/${keyName}`);
        assert.deepEqual(m.usage, []);
        assert.deepEqual(m.variants, []);
        const key = r2Key(m.id);
        assert.match(key, new RegExp(`^media/[A-Za-z0-9_-]{22}/${keyName.replaceAll(".", "\\.")}$`));
        assert.equal(w.r2Size(key), bytes.length, `${mime}: R2 object`);
        const got = await api(w, "GET", `/api/v1/media/${m.id}`, { as: AS.readToken });
        assert.deepEqual(got.json, m);
        const del = await api(w, "DELETE", `/api/v1/media/${m.id}`, { as });
        assert.equal(del.status, 204, del.text);
        assert.equal(w.r2Size(key), null, `${mime}: R2 object gone`);
        assertError(await api(w, "GET", `/api/v1/media/${m.id}`, { as }), 404, "store.not_found");
      }
    });
  }

  test("media: the same name twice gets two keys", async () => {
    const a = await upload(w, AS.writerToken, IMAGES[0]);
    const b = await upload(w, AS.writerToken, IMAGES[0]);
    assert.equal(a.status, 201, a.text);
    assert.notEqual(r2Key(a.json.id), r2Key(b.json.id));
  });

  test("media: wrong magic bytes, bad type, bad or missing size are 400 and store nothing", async () => {
    const before = w.query("SELECT COUNT(*) AS n FROM media")[0].n;
    const [, pngBytes] = IMAGES[0];
    const bad = [
      [{ mime: "image/jpeg", width: 3, height: 2 }, pngBytes, "mime"],
      [{ mime: "image/png", width: 3, height: 2 }, Buffer.from("<svg xmlns='http://www.w3.org/2000/svg'/>"), "mime"],
      [{ mime: "image/svg+xml", width: 3, height: 2 }, Buffer.from("<svg/>"), "mime"],
      [{ mime: "image/png", width: 3, height: 2 }, Buffer.alloc(0), "mime"],
      [{ mime: "image/png", width: 2, height: 3 }, pngBytes, "width"],
      [{ mime: "image/png", width: 3, height: 3 }, pngBytes, "width"],
      [{ mime: "image/png", width: 0, height: 2 }, pngBytes, "width"],
      [{ mime: "image/png", width: -3, height: 2 }, pngBytes, "width"],
      [{ mime: "image/png", width: 3 }, pngBytes, "height"],
      [{ mime: "image/png", height: 2 }, pngBytes, "width"],
      [{ mime: "image/png", width: "3.0", height: 2 }, pngBytes, "width"],
      [{ mime: "image/png", width: 3, height: 2, alt: "a\u0007b" }, pngBytes, "alt"],
      [{ mime: "image/png", width: 3, height: 2, name: "..." }, pngBytes, "name"],
      [{ mime: "image/png", width: 3, height: 2, extra: "1" }, pngBytes, "extra"],
    ];
    for (const [q, bytes, field] of bad) {
      const r = await api(w, "POST", uploadPath({ name: "x.png", ...q }), { as: AS.writerSession, raw: bytes });
      assertError(r, 400, "input.invalid", field);
    }
    // Missing name.
    const r = await api(w, "POST", uploadPath({ mime: "image/png", width: 3, height: 2 }), { as: AS.writerToken, raw: pngBytes });
    assertError(r, 400, "input.invalid", "name");
    assert.equal(w.query("SELECT COUNT(*) AS n FROM media")[0].n, before);
  });

  test("media: a body over 20 MiB is 413; at the cap it is read", async () => {
    const big = Buffer.alloc(MAX_MEDIA_BODY + 1);
    pngBytesInto(big);
    const q = { name: "big.png", mime: "image/png", width: 3, height: 2 };
    assertError(await api(w, "POST", uploadPath(q), { as: AS.writerToken, raw: big }), 413, "api.body_too_large");
    // Anonymous and too large: refused for size before anything else.
    assertError(await api(w, "POST", uploadPath(q), { raw: big }), 413, "api.body_too_large");
    // Exactly the cap passes the gate; the header (a 3x2 PNG) is what is checked.
    const r = await api(w, "POST", uploadPath(q), { as: AS.writerToken, raw: big.subarray(0, MAX_MEDIA_BODY) });
    assert.equal(r.status, 201, r.text);
    assert.equal(w.r2Size(r2Key(r.json.id)), MAX_MEDIA_BODY);
  });

  test("media: who may upload, edit and delete", async () => {
    const img = IMAGES[1];
    assertError(await upload(w, {}, img), 401, "auth.unauthenticated");
    assertError(await upload(w, AS.readToken, img), 403, "auth.forbidden");
    // A session write needs the site's Origin.
    const noOrigin = await api(w, "POST", uploadPath({ name: "a.gif", mime: "image/gif", width: 1, height: 1 }), {
      as: AS.writerSession,
      raw: img[1],
      origin: null,
    });
    assertError(noOrigin, 403, "auth.forbidden");
    const made = await upload(w, AS.writerToken, img);
    const id = made.json.id;
    assertError(await api(w, "PATCH", `/api/v1/media/${id}`, { as: AS.readToken, body: { alt: "x" } }), 403, "auth.forbidden");
    assertError(await api(w, "DELETE", `/api/v1/media/${id}`, { as: AS.readToken }), 403, "auth.forbidden");
    assertError(await api(w, "DELETE", `/api/v1/media/${id}`, {}), 401, "auth.unauthenticated");
    assert.ok(w.r2Size(r2Key(id)) > 0);
    assert.equal((await api(w, "GET", `/api/v1/media/${id}`, { as: AS.readToken })).json.alt, "");
  });

  test("media: alt text, filters and keyset pages", async () => {
    const a = (await upload(w, AS.writerToken, IMAGES[0])).json;
    const p = await api(w, "PATCH", `/api/v1/media/${a.id}`, { as: AS.writerSession, body: { alt: "Red pixels" } });
    assert.equal(p.status, 200, p.text);
    assert.equal(p.json.alt, "Red pixels");
    assertError(await api(w, "PATCH", `/api/v1/media/${a.id}`, { as: AS.writerSession, body: { alt: 1 } }), 400, "input.invalid", "alt");
    assertError(await api(w, "PATCH", "/api/v1/media/999999", { as: AS.writerSession, body: { alt: "x" } }), 404, "store.not_found");
    const missing = await api(w, "GET", "/api/v1/media?missing_alt=true&limit=100", { as: AS.readToken });
    assert.ok(missing.json.media.every((m) => m.alt === ""));
    assert.ok(!missing.json.media.some((m) => m.id === a.id));
    const all = [];
    let cursor = null;
    do {
      const q = cursor ? `?limit=2&cursor=${cursor}` : "?limit=2";
      const r = await api(w, "GET", `/api/v1/media${q}`, { as: AS.readToken });
      assert.equal(r.status, 200, r.text);
      assert.ok(r.json.media.length <= 2);
      all.push(...r.json.media.map((m) => m.id));
      cursor = r.json.next_cursor;
    } while (cursor);
    const count = w.query("SELECT COUNT(*) AS n FROM media")[0].n;
    assert.equal(all.length, count);
    assert.equal(new Set(all).size, count);
    assert.equal(all[0], Math.max(...all));
    assertError(await api(w, "GET", "/api/v1/media?cursor=nope", { as: AS.readToken }), 400, "input.invalid", "cursor");
    assertError(await api(w, "GET", "/api/v1/media?limit=101", { as: AS.readToken }), 400, "input.invalid", "limit");
  });

  test("media: in use is 409 with the post ids, and the row and object stay", async () => {
    const m = (await upload(w, AS.writerToken, IMAGES[0])).json;
    const body = await api(w, "POST", "/api/v1/posts", {
      as: AS.writerToken,
      body: { lang: "en", slug: "uses-media", title: "T", markdown: `![a](/media/${m.id}/red.png)` },
    });
    assert.equal(body.status, 201, body.text);
    const cover = await api(w, "POST", "/api/v1/posts", {
      as: AS.writerToken,
      body: { lang: "en", slug: "cover-media", title: "T", markdown: "x", cover_media_id: m.id },
    });
    assert.equal(cover.status, 201, cover.text);
    const ids = [body.json.id, cover.json.id].sort((a, b) => a - b);
    const r = await api(w, "DELETE", `/api/v1/media/${m.id}`, { as: AS.writerToken });
    assert.equal(r.status, 409, r.text);
    assert.deepEqual(r.json, { error: "store.conflict", posts: ids });
    assert.deepEqual((await api(w, "GET", `/api/v1/media/${m.id}`, { as: AS.readToken })).json.usage, ids);
    assert.ok(w.r2Size(r2Key(m.id)) > 0);
    const unused = await api(w, "GET", "/api/v1/media?unused=true&limit=100", { as: AS.readToken });
    assert.ok(!unused.json.media.some((x) => x.id === m.id));
  });

  test("redirects: owner puts, lists and deletes; paths must stay on the site", async () => {
    for (const as of [AS.ownerSession, AS.ownerToken]) {
      const r = await api(w, "PUT", "/api/v1/redirects", { as, body: { from: "/old", to: "/new" } });
      assert.equal(r.status, 204, r.text);
    }
    // Chains collapse: /older -> /old becomes /older -> /new.
    await api(w, "PUT", "/api/v1/redirects", { as: AS.ownerToken, body: { from: "/new", to: "/newest" } });
    const list = await api(w, "GET", "/api/v1/redirects", { as: AS.readToken });
    assert.deepEqual(list.json.redirects.map((x) => [x.from, x.to]), [["/new", "/newest"], ["/old", "/newest"]]);
    for (const [from, to, field] of [
      ["//evil.test", "/x", "from"],
      ["/x", "//evil.test", "to"],
      ["/x", "/\\evil.test", "to"],
      ["/x", "https://evil.test", "to"],
      ["/x", "/x", "to"],
      ["/a b", "/x", "from"],
    ]) {
      assertError(await api(w, "PUT", "/api/v1/redirects", { as: AS.ownerToken, body: { from, to } }), 400, "input.invalid", field);
    }
    assertError(await api(w, "PUT", "/api/v1/redirects", { as: AS.ownerToken, body: { from: "/x" } }), 400, "input.invalid", "to");
    const del = await api(w, "DELETE", "/api/v1/redirects?from=%2Fold", { as: AS.ownerToken });
    assert.equal(del.status, 204, del.text);
    assertError(await api(w, "DELETE", "/api/v1/redirects", { as: AS.ownerToken }), 400, "input.invalid", "from");
    const after = await api(w, "GET", "/api/v1/redirects", { as: AS.ownerSession });
    assert.deepEqual(after.json.redirects.map((x) => x.from), ["/new"]);
  });

  test("settings: internal keys are never shown or written; values round-trip as strict JSON", async () => {
    const r = await api(w, "GET", "/api/v1/settings", { as: AS.ownerToken });
    assert.equal(r.status, 200, r.text);
    assert.equal(r.json.settings.title, "My blog");
    assert.equal(r.json.settings.site_id, "site-abc");
    for (const k of ["setup_code_hash", "setup_code_expires_at"]) {
      assert.ok(!(k in r.json.settings), k);
      assert.ok(!r.text.includes("deadbeef"));
      for (const [method, body] of [["PUT", { value: "x" }], ["DELETE", undefined]]) {
        assertError(await api(w, method, `/api/v1/settings/${k}`, { as: AS.ownerToken, body }), 400, "input.invalid", "key");
      }
    }
    // The site id salts every password; it is read-only here.
    assertError(await api(w, "PUT", "/api/v1/settings/site_id", { as: AS.ownerToken, body: { value: "x" } }), 400, "input.invalid", "key");
    assertError(await api(w, "DELETE", "/api/v1/settings/site_id", { as: AS.ownerToken }), 400, "input.invalid", "key");
    assert.equal(settingRow("setup_code_hash"), '"deadbeef"');
    assert.equal(settingRow("site_id"), '"site-abc"');
    const put = await api(w, "PUT", "/api/v1/settings/nav", { as: AS.ownerSession, body: '{ "value" : {"items": [ {"label":"Home","href":"/"} ], "n": -1, "on": true, "x": null} }' });
    assert.equal(put.status, 204, put.text);
    assert.equal(settingRow("nav"), '{"items":[{"label":"Home","href":"/"}],"n":-1,"on":true,"x":null}');
    const got = await api(w, "GET", "/api/v1/settings", { as: AS.readToken });
    assert.deepEqual(got.json.settings.nav, { items: [{ label: "Home", href: "/" }], n: -1, on: true, x: null });
    for (const body of ['{"value":1.5}', '{"value":1,"value":2}', "{}", '{"value":"a","other":1}']) {
      assert.equal((await api(w, "PUT", "/api/v1/settings/nav", { as: AS.ownerToken, body })).status, 400, body);
    }
    assertError(await api(w, "PUT", "/api/v1/settings/Bad.Key", { as: AS.ownerToken, body: { value: 1 } }), 400, "input.invalid", "key");
    assert.equal((await api(w, "DELETE", "/api/v1/settings/nav", { as: AS.ownerToken })).status, 204);
    assert.equal(settingRow("nav"), undefined);
  });

  test("settings: origin passes the sign-in code's own rule", async () => {
    for (const bad of ["http://blog.example", "https://Blog.Example", "https://blog.example/path", "https://u@blog.example", "blog.example", 42]) {
      const r = await api(w, "PUT", "/api/v1/settings/origin", { as: AS.ownerToken, body: { value: bad } });
      assertError(r, 400, "input.invalid", "value");
    }
    assert.equal(settingRow("origin"), undefined);
    const ok = await api(w, "PUT", "/api/v1/settings/origin", { as: AS.ownerToken, body: { value: "https://blog.example" } });
    assert.equal(ok.status, 204, ok.text);
    assert.equal(settingRow("origin"), '"https://blog.example"');
    // The CSRF check now wants that origin from a session.
    assertError(await api(w, "PUT", "/api/v1/settings/title", { as: AS.ownerSession, body: { value: "x" } }), 403, "auth.forbidden");
    const back = await api(w, "DELETE", "/api/v1/settings/origin", { as: AS.ownerToken });
    assert.equal(back.status, 204, back.text);
  });

  test("theme and plugin options round-trip; plugins enable and disable", async () => {
    for (const kind of ["themes", "plugins"]) {
      const base = `/api/v1/${kind}/minimal/options`;
      assert.equal((await api(w, "PUT", `${base}/accent`, { as: AS.ownerToken, body: { value: "#336699" } })).status, 204);
      assert.equal((await api(w, "PUT", `${base}/excerpt_words`, { as: AS.ownerSession, body: { value: 40 } })).status, 204);
      const got = await api(w, "GET", base, { as: AS.readToken });
      assert.deepEqual(got.json.options, { accent: "#336699", excerpt_words: 40 });
      assertError(await api(w, "PUT", `${base}/a.b`, { as: AS.ownerToken, body: { value: 1 } }), 400, "input.invalid", "key");
      assertError(await api(w, "GET", `/api/v1/${kind}/Mini/options`, { as: AS.readToken }), 400, "input.invalid", "name");
      assert.equal((await api(w, "DELETE", base, { as: AS.ownerToken })).status, 204);
      assert.deepEqual((await api(w, "GET", base, { as: AS.readToken })).json.options, {});
    }
    // Options of one owner kind never leak into the other.
    await api(w, "PUT", "/api/v1/themes/minimal/options/only_theme", { as: AS.ownerToken, body: { value: true } });
    assert.deepEqual((await api(w, "GET", "/api/v1/plugins/minimal/options", { as: AS.readToken })).json.options, {});
    for (const enabled of [true, false]) {
      assert.equal((await api(w, "PUT", "/api/v1/plugins/reading-time", { as: AS.ownerToken, body: { enabled } })).status, 204);
      const list = await api(w, "GET", "/api/v1/plugins", { as: AS.readToken });
      assert.deepEqual(list.json.plugins.map((p) => [p.name, p.enabled, p.disabled_reason]), [["reading-time", enabled, null]]);
    }
    assertError(await api(w, "PUT", "/api/v1/plugins/reading-time", { as: AS.ownerToken, body: { enabled: "yes" } }), 400, "input.invalid", "enabled");
  });

  test("owner-only routes: writers and read tokens get 403 and change nothing", async () => {
    const writes = [
      ["PUT", "/api/v1/redirects", { from: "/w", to: "/x" }],
      ["DELETE", "/api/v1/redirects?from=%2Fnew", undefined],
      ["PUT", "/api/v1/settings/title", { value: "pwned" }],
      ["DELETE", "/api/v1/settings/title", undefined],
      ["PUT", "/api/v1/settings/setup_code_hash", { value: "x" }],
      ["PUT", "/api/v1/themes/minimal/options/accent", { value: "#000" }],
      ["DELETE", "/api/v1/themes/minimal/options", undefined],
      ["PUT", "/api/v1/plugins/reading-time", { enabled: true }],
      ["PUT", "/api/v1/plugins/reading-time/options/x", { value: 1 }],
      ["DELETE", "/api/v1/plugins/reading-time/options", undefined],
    ];
    const snapshot = () =>
      JSON.stringify([
        w.query("SELECT * FROM settings ORDER BY key"),
        w.query("SELECT * FROM redirects ORDER BY from_path"),
        w.query("SELECT * FROM extension_options ORDER BY owner_kind, owner_id, key"),
        w.query("SELECT * FROM plugins ORDER BY id"),
      ]);
    const before = snapshot();
    for (const as of [AS.writerSession, AS.writerToken, AS.readToken]) {
      for (const [method, path, body] of writes) {
        assertError(await api(w, method, path, { as, body }), 403, "auth.forbidden");
      }
    }
    for (const [method, path, body] of writes) {
      assertError(await api(w, method, path, { body }), 401, "auth.unauthenticated");
    }
    // Reads are open to every signed-in caller, internal settings still hidden.
    for (const as of [AS.writerSession, AS.readToken]) {
      for (const path of ["/api/v1/redirects", "/api/v1/settings", "/api/v1/plugins", "/api/v1/themes/minimal/options"]) {
        const r = await api(w, "GET", path, { as });
        assert.equal(r.status, 200, `${path}: ${r.text}`);
        assert.ok(!r.text.includes("setup_code"), path);
      }
    }
    assert.equal(snapshot(), before);
  });
});

/** Writes a 3x2 PNG header at the start of `buf`. */
function pngBytesInto(buf) {
  IMAGES[0][1].copy(buf, 0);
}
