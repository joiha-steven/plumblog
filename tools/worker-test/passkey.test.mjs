// /api/auth/passkey/* over HTTP against the real Worker. Node plays the authenticator:
// node:crypto keys (ES256, RS256 2048, Ed25519), hand-built CBOR attestation objects (fmt
// "none"), authenticator data and clientDataJSON. Two Workers start in parallel:
//   plain:      configured origin = the Worker's own URL (rp id 127.0.0.1); seeded users,
//               sessions, passkeys and an expired challenge.
//   configured: settings `origin` = https://blog.test, unlike the host requests arrive on.
// A site without an origin is covered in auth.test.mjs (fresh site).
import assert from "node:assert/strict";
import { createHash, generateKeyPairSync, pbkdf2, pbkdf2Sync, randomBytes, sign } from "node:crypto";
import { after, before, describe, test } from "node:test";
import { promisify } from "node:util";

import { startWorker } from "./harness.mjs";

const COOKIE = "__Host-plumblog_session";
const DAY = 86_400_000;
const T = Date.now();
const sha256 = (b) => createHash("sha256").update(b).digest();
const sha256hex = (b) => createHash("sha256").update(b).digest("hex");
const b64u = (buf) => Buffer.from(buf).toString("base64url");
const sq = (s) => `'${String(s).replaceAll("'", "''")}'`;
const pbkdf2Async = promisify(pbkdf2);
/** The browser's stretch (web/admin/js/stretch.js); auth.test.mjs pins it. */
const stretch = (site, login, password) =>
  pbkdf2Async(password, `plumblog:${site}:${login.toLowerCase()}`, 600000, 32, "sha256").then(b64u);
/** Stored form of crates/auth hash_password, for seeded users. */
function serverHash(stretched) {
  const salt = randomBytes(16);
  const h = pbkdf2Sync(Buffer.from(stretched, "base64url"), salt, 1000, 32, "sha256");
  return `p1$1000$${b64u(salt)}$${b64u(h)}`;
}
const tokenOf = (setCookie) => setCookie?.match(/^__Host-plumblog_session=([A-Za-z0-9_-]*);/)?.[1];

const REG_OPT = "/api/auth/passkey/register/options";
const REG_VERIFY = "/api/auth/passkey/register/verify";
const LOGIN_OPT = "/api/auth/passkey/login/options";
const LOGIN_VERIFY = "/api/auth/passkey/login/verify";

// Every call gets its own address unless a test names one, so the per-address limiter only
// bites where a test means it to.
let ipSeq = 0;
const nextIp = () => `198.18.${Math.floor(++ipSeq / 250)}.${(ipSeq % 250) + 1}`;

async function api(w, method, path, { body, cookie, origin, ip } = {}) {
  const h = { "cf-connecting-ip": ip ?? nextIp() };
  if (origin !== null) h.origin = origin ?? w.site;
  if (cookie) h.cookie = `${COOKIE}=${cookie}`;
  let payload;
  if (body !== undefined) {
    h["content-type"] = "application/json";
    payload = typeof body === "string" ? body : JSON.stringify(body);
  }
  const res = await fetch(w.url + path, { method, headers: h, body: payload });
  const text = await res.text();
  return { status: res.status, text, json: text ? JSON.parse(text) : null, setCookie: res.headers.get("set-cookie") };
}

function assertError(r, status, code) {
  assert.equal(r.status, status, r.text);
  assert.deepEqual(r.json, { error: code });
}
const refused = (r) => assertError(r, 400, "auth.passkey_invalid");

// --- CBOR and the authenticator ------------------------------------------------------------

function head(major, n) {
  if (n < 24) return Buffer.from([(major << 5) | n]);
  if (n < 0x100) return Buffer.from([(major << 5) | 24, n]);
  if (n < 0x10000) return Buffer.from([(major << 5) | 25, n >> 8, n & 0xff]);
  const b = Buffer.alloc(5);
  b[0] = (major << 5) | 26;
  b.writeUInt32BE(n, 1);
  return b;
}
/** Numbers, Buffers, strings, arrays and Maps (Maps keep integer keys and their order). */
function cbor(v) {
  if (typeof v === "number") return v >= 0 ? head(0, v) : head(1, -1 - v);
  if (Buffer.isBuffer(v)) return Buffer.concat([head(2, v.length), v]);
  if (typeof v === "string") {
    const b = Buffer.from(v, "utf8");
    return Buffer.concat([head(3, b.length), b]);
  }
  if (Array.isArray(v)) return Buffer.concat([head(4, v.length), ...v.map(cbor)]);
  if (v instanceof Map) return Buffer.concat([head(5, v.size), ...[...v].flatMap(([k, x]) => [cbor(k), cbor(x)])]);
  throw new Error(`cbor: ${typeof v}`);
}

const UP = 0x01;
const UV = 0x04;
const AT = 0x40;

class Authenticator {
  constructor(alg) {
    this.alg = alg;
    this.credId = randomBytes(16);
    this.count = 0;
    if (alg === "ES256") this.keys = generateKeyPairSync("ec", { namedCurve: "P-256" });
    else if (alg === "RS256") this.keys = generateKeyPairSync("rsa", { modulusLength: 2048 });
    else this.keys = generateKeyPairSync("ed25519");
    this.jwk = this.keys.publicKey.export({ format: "jwk" });
  }
  get id() {
    return b64u(this.credId);
  }
  cose() {
    const j = this.jwk;
    const d = (s) => Buffer.from(s, "base64url");
    if (this.alg === "ES256") return new Map([[1, 2], [3, -7], [-1, 1], [-2, d(j.x)], [-3, d(j.y)]]);
    if (this.alg === "RS256") return new Map([[1, 3], [3, -257], [-1, d(j.n)], [-2, d(j.e)]]);
    return new Map([[1, 1], [3, -8], [-1, 6], [-2, d(j.x)]]);
  }
  /** passkeys.public_key form (crates/auth cose.rs), for seeded rows. */
  stored() {
    const j = this.jwk;
    if (this.alg === "ES256") return `es256:${b64u(Buffer.concat([Buffer.from(j.x, "base64url"), Buffer.from(j.y, "base64url")]))}`;
    if (this.alg === "RS256") return `rs256:${j.n}:${j.e}`;
    return `ed25519:${j.x}`;
  }
  sign(message) {
    if (this.alg === "ES256") return sign("sha256", message, { key: this.keys.privateKey, dsaEncoding: "der" });
    if (this.alg === "RS256") return sign("sha256", message, this.keys.privateKey);
    return sign(null, message, this.keys.privateKey);
  }
}

function authData(rpId, flags, count, attested) {
  const fixed = Buffer.alloc(37);
  sha256(rpId).copy(fixed, 0);
  fixed[32] = flags;
  fixed.writeUInt32BE(count, 33);
  if (!attested) return fixed;
  const len = Buffer.alloc(2);
  len.writeUInt16BE(attested.credId.length);
  return Buffer.concat([fixed, Buffer.alloc(16), len, attested.credId, cbor(attested.cose)]);
}
const clientData = (type, challenge, origin) =>
  Buffer.from(JSON.stringify({ type, challenge, origin, crossOrigin: false }));

/** Full registration ceremony; `o` bends one part of it. */
async function register(w, cookie, a, o = {}) {
  const opt = await api(w, "POST", REG_OPT, { cookie });
  assert.equal(opt.status, 200, opt.text);
  const pk = opt.json.publicKey;
  const cd = clientData(o.type ?? "webauthn.create", o.challenge ?? pk.challenge, o.origin ?? w.site);
  const ad = authData(o.rpId ?? pk.rp.id, o.flags ?? (UP | UV | AT), a.count, {
    credId: a.credId,
    cose: o.cose ?? a.cose(),
  });
  const att = o.att ?? cbor(new Map([["fmt", o.fmt ?? "none"], ["attStmt", new Map()], ["authData", ad]]));
  const verifyCookie = o.verifyCookie ?? cookie;
  // Re-authentication: the signed-in user's password by default; `reauth: null` sends none.
  const reauth = o.reauth === undefined ? { current_password: PW.get(verifyCookie) } : o.reauth;
  const body = { name: o.name ?? `${a.alg} key`, client_data_json: b64u(cd), attestation_object: b64u(att), ...reauth };
  const r = await api(w, "POST", REG_VERIFY, { cookie: verifyCookie, body });
  return Object.assign(r, { options: pk, body });
}

/** Full sign-in ceremony; `o` bends one part of it. Returns the answer and the body sent. */
async function login(w, a, o = {}) {
  let challenge = o.challenge;
  let rpId = w.host;
  if (challenge === undefined) {
    const opt = await api(w, "POST", LOGIN_OPT, {});
    assert.equal(opt.status, 200, opt.text);
    ({ challenge, rpId } = opt.json.publicKey);
  }
  const count = o.count ?? ++a.count;
  const cd = clientData(o.type ?? "webauthn.get", challenge, o.origin ?? w.site);
  const ad = authData(o.rpId ?? rpId, o.flags ?? (UP | UV), count);
  const signed = o.signedAd ?? ad;
  const signature = o.signature ?? a.sign(Buffer.concat([signed, sha256(cd)]));
  const body = {
    credential_id: o.credId ?? a.id,
    client_data_json: b64u(cd),
    authenticator_data: b64u(o.ad ?? ad),
    signature: b64u(signature),
    ...o.extra,
  };
  const r = await api(w, o.method ?? "POST", o.path ?? LOGIN_VERIFY, { body, cookie: o.cookie, ip: o.ip });
  return Object.assign(r, { body });
}

/** Re-authentication fields from a fresh assertion by `a` (adding or removing a passkey). */
async function reauthWith(w, a) {
  const opt = await api(w, "POST", LOGIN_OPT, {});
  assert.equal(opt.status, 200, opt.text);
  const { challenge, rpId } = opt.json.publicKey;
  const cd = clientData("webauthn.get", challenge, w.site);
  const ad = authData(rpId, UP | UV, ++a.count);
  return {
    reauth_credential_id: a.id,
    reauth_client_data_json: b64u(cd),
    reauth_authenticator_data: b64u(ad),
    reauth_signature: b64u(a.sign(Buffer.concat([ad, sha256(cd)]))),
  };
}

// --- workers -------------------------------------------------------------------------------

const tok = () => b64u(randomBytes(32));
const S = { olga: tok(), pat: tok(), wes: tok(), dis: tok(), cara: tok() };
const SEEDED = {
  olga: new Authenticator("ES256"),
  dis: new Authenticator("Ed25519"),
  pat: new Authenticator("Ed25519"),
  // Stored with a key that is not a P-256 point (x = 7..., y = 9...), as an old row might be.
  bad: new Authenticator("ES256"),
};
const OFF_CURVE = `es256:${b64u(Buffer.concat([Buffer.alloc(32, 7), Buffer.alloc(32, 9)]))}`;
/** Session token -> stretched password, for re-authentication. */
const PW = new Map();
const EXPIRED = randomBytes(32);
let plain, configured;

before(async () => {
  const [olgaPw, wesPw, caraPw] = await Promise.all([
    stretch("site-pk", "olga", "olga pass"),
    stretch("site-pk", "wes", "wes pass"),
    stretch("site-cfg", "cara", "cara pass"),
  ]);
  PW.set(S.olga, olgaPw).set(S.wes, wesPw).set(S.cara, caraPw);
  const users = `INSERT INTO users (id, login, slug, display_name, role, password_hash, disabled_at, created_at, updated_at) VALUES
    (1, 'olga', 'olga', 'Olga', 'owner', ${sq(serverHash(olgaPw))}, NULL, ${T}, ${T}),
    (2, 'pat', 'pat', 'Pat', 'writer', NULL, NULL, ${T}, ${T}),
    (3, 'dis', 'dis', 'Dis', 'writer', NULL, ${T - 1000}, ${T}, ${T}),
    (4, 'wes', 'wes', 'Wes', 'writer', ${sq(serverHash(wesPw))}, NULL, ${T}, ${T});`;
  const sessions = (pairs) =>
    `INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES ${pairs
      .map(([t, u]) => `(${sq(sha256hex(t))}, ${u}, ${T}, ${T + 29 * DAY})`)
      .join(", ")};`;
  const seedPlain = (url) => [
    `INSERT INTO settings (key, value_json, updated_at) VALUES ('site_id', '"site-pk"', ${T}),
      ('origin', ${sq(JSON.stringify(url))}, ${T});`,
    users,
    sessions([[S.olga, 1], [S.pat, 2], [S.dis, 3], [S.wes, 4]]),
    `INSERT INTO passkeys (user_id, credential_id, public_key, sign_count, name, created_at) VALUES
      (1, ${sq(SEEDED.olga.id)}, ${sq(SEEDED.olga.stored())}, 100, 'seeded', ${T}),
      (3, ${sq(SEEDED.dis.id)}, ${sq(SEEDED.dis.stored())}, 0, 'disabled', ${T}),
      (2, ${sq(SEEDED.pat.id)}, ${sq(SEEDED.pat.stored())}, 0, 'pat phone', ${T}),
      (4, ${sq(SEEDED.bad.id)}, ${sq(OFF_CURVE)}, 0, 'off curve', ${T});`,
    `INSERT INTO auth_challenges (challenge_hash, purpose, user_id, expires_at) VALUES
      (${sq(sha256hex(EXPIRED))}, 'login', NULL, ${T - 1});`,
  ].join("\n");
  const seedConfigured = [
    `INSERT INTO settings (key, value_json, updated_at) VALUES ('site_id', '"site-cfg"', ${T}),
      ('origin', '"https://blog.test"', ${T});`,
    `INSERT INTO users (id, login, slug, display_name, role, password_hash, disabled_at, created_at, updated_at) VALUES
      (1, 'cara', 'cara', 'Cara', 'owner', ${sq(serverHash(caraPw))}, NULL, ${T}, ${T});`,
    sessions([[S.cara, 1]]),
  ].join("\n");
  // `plain` makes more limited attempts in one window than the site-wide cap allows; that cap
  // has its own test (globalcap.test.mjs), so it is raised out of the way here.
  [plain, configured] = await Promise.all([
    startWorker({ seedSql: seedPlain, vars: { PLUMBLOG_TEST_GLOBAL_CAP: "100000" } }),
    startWorker({ seedSql: seedConfigured }),
  ]);
  plain.site = plain.url;
  plain.host = "127.0.0.1";
  configured.site = "https://blog.test";
  configured.host = "blog.test";
});

after(async () => {
  await Promise.all([plain?.stop(), configured?.stop()]);
});

const count = (w, sql) => w.query(sql)[0].n;

// --- plain site ----------------------------------------------------------------------------

describe("passkeys, no configured origin", () => {
  // First, before any options call sweeps expired challenges.
  test("expired challenge is refused (and removed)", async () => {
    refused(await login(plain, SEEDED.olga, { challenge: b64u(EXPIRED), count: 101 }));
    assert.equal(count(plain, "SELECT COUNT(*) AS n FROM auth_challenges WHERE expires_at < " + T), 0);
  });

  test("options: anonymous register is 401; login options name no user; Origin required", async () => {
    assertError(await api(plain, "POST", REG_OPT), 401, "auth.unauthenticated");
    assertError(await api(plain, "POST", REG_VERIFY, { body: {} }), 401, "auth.unauthenticated");
    assertError(await api(plain, "GET", "/api/auth/passkey"), 401, "auth.unauthenticated");
    assertError(await api(plain, "POST", LOGIN_OPT, { origin: null }), 403, "auth.forbidden");
    assertError(await api(plain, "POST", LOGIN_VERIFY, { origin: null, body: {} }), 403, "auth.forbidden");
    assertError(await api(plain, "POST", REG_OPT, { origin: "http://evil.example", cookie: S.olga }), 403, "auth.forbidden");
    const r = await api(plain, "POST", LOGIN_OPT);
    assert.equal(r.status, 200, r.text);
    const pk = r.json.publicKey;
    assert.equal(Buffer.from(pk.challenge, "base64url").length, 32);
    assert.deepEqual(
      { ...pk, challenge: "" },
      { challenge: "", rpId: "127.0.0.1", timeout: 300000, userVerification: "required", allowCredentials: [] },
    );
    // Stored hashed: the raw challenge is nowhere in the table.
    const rows = plain.query("SELECT challenge_hash, purpose, user_id FROM auth_challenges");
    assert.ok(rows.some((x) => x.challenge_hash === sha256hex(Buffer.from(pk.challenge, "base64url"))));
    assert.ok(!JSON.stringify(rows).includes(pk.challenge));
  });

  test("anonymous sign-in options: 20 per address per window, then locked before any write", async () => {
    const ip = "192.0.2.150";
    const ok = await Promise.all(Array.from({ length: 20 }, () => api(plain, "POST", LOGIN_OPT, { ip })));
    assert.deepEqual([...new Set(ok.map((r) => r.status))], [200]);
    const rows = count(plain, "SELECT COUNT(*) AS n FROM auth_challenges");
    assertError(await api(plain, "POST", LOGIN_OPT, { ip }), 429, "auth.locked");
    assert.equal(count(plain, "SELECT COUNT(*) AS n FROM auth_challenges"), rows);
    assert.equal((await api(plain, "POST", LOGIN_OPT)).status, 200, "another address is not affected");
  });

  test("register options: rp, user handle, algorithms, UV required, existing keys excluded", async () => {
    const r = await api(plain, "POST", REG_OPT, { cookie: S.olga, body: "{}" });
    assert.equal(r.status, 200, r.text);
    const pk = r.json.publicKey;
    assert.deepEqual(pk.rp, { id: "127.0.0.1", name: "127.0.0.1" });
    assert.deepEqual(pk.user, { id: b64u(Buffer.from([0, 0, 0, 0, 0, 0, 0, 1])), name: "olga", displayName: "Olga" });
    assert.deepEqual(pk.pubKeyCredParams.map((p) => p.alg), [-7, -8, -257]);
    assert.equal(pk.authenticatorSelection.userVerification, "required");
    assert.equal(pk.attestation, "none");
    assert.deepEqual(pk.excludeCredentials, [{ type: "public-key", id: SEEDED.olga.id }]);
    assertError(await api(plain, "POST", REG_OPT, { cookie: S.olga, body: { x: "1" } }), 400, "input.invalid");
  });

  for (const alg of ["ES256", "RS256", "Ed25519"]) {
    test(`${alg}: register, list, sign in (session rotated), sign in again`, async () => {
      const a = new Authenticator(alg);
      const reg = await register(plain, S.wes, a, { name: `wes ${alg}` });
      assert.equal(reg.status, 201, reg.text);
      assert.equal(reg.json.passkey.name, `wes ${alg}`);
      assert.equal(reg.json.passkey.last_used_at, null);
      const row = plain.query(`SELECT user_id, public_key, sign_count FROM passkeys WHERE credential_id = ${sq(a.id)}`);
      assert.deepEqual(row, [{ user_id: 4, public_key: a.stored(), sign_count: 0 }]);

      const list = await api(plain, "GET", "/api/auth/passkey", { cookie: S.wes });
      assert.ok(list.json.passkeys.some((k) => k.id === reg.json.passkey.id));

      const first = await login(plain, a);
      assert.equal(first.status, 200, first.text);
      assert.equal(first.json.user.login, "wes");
      const cookie = tokenOf(first.setCookie);
      assert.match(cookie, /^[A-Za-z0-9_-]{43}$/);
      assert.equal((await api(plain, "GET", "/api/auth/me", { cookie })).json.user.login, "wes");
      // Signing in with a session rotates it, like a password sign-in.
      const second = await login(plain, a, { cookie });
      assert.equal(second.status, 200, second.text);
      assertError(await api(plain, "GET", "/api/auth/me", { cookie }), 401, "auth.unauthenticated");
      const used = plain.query(`SELECT sign_count, last_used_at FROM passkeys WHERE credential_id = ${sq(a.id)}`)[0];
      assert.equal(used.sign_count, 2);
      assert.ok(used.last_used_at >= T);
    });
  }

  test("an authenticator without a counter (always 0) still signs in", async () => {
    const a = new Authenticator("Ed25519");
    assert.equal((await register(plain, S.wes, a)).status, 201);
    assert.equal((await login(plain, a, { count: 0 })).status, 200);
    assert.equal((await login(plain, a, { count: 0 })).status, 200);
  });

  test("register refusals: origin, rp id, UV, UP, type, fmt, another user's challenge; nothing stored", async () => {
    const before = count(plain, "SELECT COUNT(*) AS n FROM passkeys");
    const cases = {
      "wrong origin": { origin: "http://evil.example" },
      "origin with another port": { origin: plain.url.replace(/:\d+$/, ":1") },
      "wrong rp id hash": { rpId: "evil.example" },
      "no UV": { flags: UP | AT },
      "no UP": { flags: UV | AT },
      "get type": { type: "webauthn.get" },
      "packed fmt": { fmt: "packed" },
      "made-up challenge": { challenge: b64u(randomBytes(32)) },
    };
    for (const [what, o] of Object.entries(cases)) {
      refused(await register(plain, S.wes, new Authenticator("ES256"), o), what);
    }
    // A register challenge belongs to the user it was issued to.
    const r = await register(plain, S.olga, new Authenticator("ES256"), { verifyCookie: S.wes });
    refused(r);
    assert.equal(count(plain, "SELECT COUNT(*) AS n FROM passkeys"), before);
  });

  test("register refusals: unsupported algorithms (-35, -36, unknown, mismatched kty)", async () => {
    const a = new Authenticator("ES256");
    const ec = a.cose();
    const x = ec.get(-2);
    for (const cose of [
      new Map([[1, 2], [3, -35], [-1, 2], [-2, x], [-3, x]]),
      new Map([[1, 2], [3, -36], [-1, 3], [-2, x], [-3, x]]),
      new Map([[1, 2], [3, -999], [-1, 1], [-2, x], [-3, x]]),
      new Map([[1, 2], [3, -8], [-1, 6], [-2, x]]),
      new Map([[1, 3], [3, -7], [-1, x], [-2, Buffer.from([1, 0, 1])]]),
      new Map([[1, 2], [3, -7], [-1, 1], [-2, x], [-3, x], [-4, x]]), // private key part
    ]) {
      refused(await register(plain, S.wes, a, { cose }));
    }
    assert.equal(count(plain, `SELECT COUNT(*) AS n FROM passkeys WHERE credential_id = ${sq(a.id)}`), 0);
  });

  test("register refusals: malformed, truncated and oversized CBOR; bad base64; bad name", async () => {
    const a = new Authenticator("ES256");
    const ad = authData("127.0.0.1", UP | UV | AT, 0, { credId: a.credId, cose: a.cose() });
    const good = cbor(new Map([["fmt", "none"], ["attStmt", new Map()], ["authData", ad]]));
    for (const att of [
      randomBytes(64),
      good.subarray(0, good.length - 1),
      good.subarray(0, 10),
      Buffer.concat([good, Buffer.from([0])]),
      Buffer.from([0xbf, 0xff]), // indefinite-length map
      Buffer.from([0x5b, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff]), // huge byte string
      cbor(new Map([["fmt", "none"], ["attStmt", new Map()], ["authData", ad.subarray(0, 50)]])),
      cbor(new Map([["fmt", "none"], ["attStmt", new Map([["x", 1]])], ["authData", ad]])),
      Buffer.alloc(0),
    ]) {
      refused(await register(plain, S.wes, a, { att }));
    }
    const opt = await api(plain, "POST", REG_OPT, { cookie: S.wes });
    const cd = b64u(clientData("webauthn.create", opt.json.publicKey.challenge, plain.url));
    refused(await api(plain, "POST", REG_VERIFY, { cookie: S.wes, body: { name: "k", client_data_json: cd, attestation_object: "***" } }));
    for (const name of ["", "   ", "a\u0007", "x".repeat(65)]) {
      assertError(
        await api(plain, "POST", REG_VERIFY, { cookie: S.wes, body: { name, client_data_json: cd, attestation_object: b64u(good) } }),
        400,
        "input.invalid",
      );
    }
  });

  test("a challenge is single use, even when the ceremony that used it failed", async () => {
    const a = new Authenticator("ES256");
    const bad = await register(plain, S.wes, a, { origin: "http://evil.example" });
    refused(bad);
    refused(await register(plain, S.wes, a, { challenge: bad.options.challenge }));
    // Sign-in: replaying a successful assertion is refused.
    assert.equal((await register(plain, S.wes, a)).status, 201);
    const ok = await login(plain, a);
    assert.equal(ok.status, 200, ok.text);
    refused(await api(plain, "POST", LOGIN_VERIFY, { body: ok.body }));
  });

  test("a challenge of the other purpose is refused both ways, and left for its own use", async () => {
    const a = new Authenticator("Ed25519");
    const regOpt = await api(plain, "POST", REG_OPT, { cookie: S.wes });
    const regChallenge = regOpt.json.publicKey.challenge;
    assert.equal((await register(plain, S.wes, a)).status, 201);
    refused(await login(plain, a, { challenge: regChallenge }));
    const loginOpt = await api(plain, "POST", LOGIN_OPT);
    refused(await register(plain, S.wes, new Authenticator("ES256"), { challenge: loginOpt.json.publicKey.challenge }));
    // The register challenge was not consumed by the wrong-purpose attempt.
    assert.equal((await register(plain, S.wes, new Authenticator("ES256"), { challenge: regChallenge })).status, 201);
  });

  test("sign-in refusals answer alike: origin, rp id, UV, UP, type, tampered, unknown, disabled", async () => {
    const other = new Authenticator("ES256");
    const cases = {
      "wrong origin": { origin: "https://evil.example" },
      "wrong rp id hash": { rpId: "evil.example" },
      "no UV": { flags: UP },
      "no UP": { flags: UV },
      "create type": { type: "webauthn.create" },
      "signature over other data": { signedAd: authData("127.0.0.1", UP | UV, 999) },
      "signature by another key": { signature: other.sign(Buffer.from("x")) },
      "truncated authenticator data": { ad: authData("127.0.0.1", UP | UV, 50).subarray(0, 36) },
      "attested data in an assertion": { ad: authData("127.0.0.1", UP | UV | AT, 50, { credId: other.credId, cose: other.cose() }) },
      "unknown credential": { credId: b64u(randomBytes(16)) },
    };
    const answers = new Set();
    for (const [what, o] of Object.entries(cases)) {
      // A fresh passkey per case, so the per-credential limiter stays out of the way.
      const a = new Authenticator("ES256");
      assert.equal((await register(plain, S.wes, a)).status, 201);
      const r = await login(plain, a, o);
      refused(r, what);
      assert.equal(r.setCookie, null, what);
      answers.add(r.text);
    }
    // A disabled user's own valid passkey answers exactly like the rest.
    const d = await login(plain, SEEDED.dis);
    refused(d);
    answers.add(d.text);
    assert.equal(answers.size, 1);
    assert.equal(count(plain, "SELECT COUNT(*) AS n FROM system_log WHERE kind = 'auth.sign_in_failed' AND actor_id = 3"), 1);
  });

  test("a sign counter that does not increase is refused, logged, and left unchanged", async () => {
    refused(await login(plain, SEEDED.olga, { count: 100 }));
    refused(await login(plain, SEEDED.olga, { count: 7 }));
    assert.equal(plain.query(`SELECT sign_count FROM passkeys WHERE credential_id = ${sq(SEEDED.olga.id)}`)[0].sign_count, 100);
    const logged = plain.query("SELECT actor_id, detail FROM system_log WHERE kind = 'auth.sign_in_failed' AND detail LIKE '%counter%'");
    assert.equal(logged.length, 2);
    assert.equal(logged[0].actor_id, 1);
    const ok = await login(plain, SEEDED.olga, { count: 101 });
    assert.equal(ok.status, 200, ok.text);
    assert.equal(ok.json.user.login, "olga");
  });

  test("failed passkey sign-ins count per credential and address: 5 lock (Turnstile off)", async () => {
    const a = new Authenticator("RS256");
    assert.equal((await register(plain, S.wes, a)).status, 201);
    const ip = "192.0.2.77";
    for (let i = 0; i < 5; i++) refused(await login(plain, a, { ip, origin: "https://evil.example" }));
    assertError(await login(plain, a, { ip }), 429, "auth.locked");
    // The credential key is locked from any address too.
    assertError(await login(plain, a), 429, "auth.locked");
    // A locked address spends nothing: no site-wide count, no challenge taken, no row written
    // for a new key.
    const b = new Authenticator("ES256");
    assert.equal((await register(plain, S.wes, b)).status, 201);
    const { challenge } = (await api(plain, "POST", LOGIN_OPT)).json.publicKey;
    const snap = () => plain.query("SELECT key_hash, count, window_start FROM login_failures WHERE key_hash <> 'global' ORDER BY key_hash");
    const global = () => count(plain, "SELECT count AS n FROM login_failures WHERE key_hash = 'global'");
    const before = snap();
    const globalBefore = global();
    assertError(await login(plain, b, { ip, challenge }), 429, "auth.locked");
    assert.deepEqual(snap(), before);
    assert.equal(global(), globalBefore, "a locked address moved the site-wide count");
    assert.equal(count(plain, `SELECT COUNT(*) AS n FROM auth_challenges WHERE challenge_hash = ${sq(sha256hex(Buffer.from(challenge, "base64url")))}`), 1);
    assert.ok(count(plain, "SELECT COUNT(*) AS n FROM system_log WHERE kind = 'auth.sign_in_locked' AND detail = 'passkey'") >= 1);
    // Bad request shapes are refused before counting.
    assertError(await api(plain, "POST", LOGIN_VERIFY, { body: { credential_id: "" } }), 400, "input.invalid");
    assertError(await api(plain, "POST", LOGIN_VERIFY, { body: { credential_id: "x".repeat(1400) } }), 400, "input.invalid");
  });

  test("list and delete: own passkeys only; another user's id is 404; anonymous 401", async () => {
    const mine = (await api(plain, "GET", "/api/auth/passkey", { cookie: S.olga })).json.passkeys;
    assert.deepEqual(mine.map((k) => k.name), ["seeded"]);
    assert.equal(mine[0].last_used_at === null, false);
    const id = mine[0].id;
    assertError(await api(plain, "DELETE", `/api/auth/passkey/${id}`, { cookie: S.wes }), 404, "store.not_found");
    assertError(await api(plain, "DELETE", `/api/auth/passkey/${id}`), 401, "auth.unauthenticated");
    assertError(await api(plain, "DELETE", `/api/auth/passkey/${id}`, { cookie: S.olga, origin: null }), 403, "auth.forbidden");
    for (const bad of ["999999", "abc", "-1", "", "1/2"]) {
      assertError(await api(plain, "DELETE", `/api/auth/passkey/${bad}`, { cookie: S.olga }), 404, "store.not_found");
    }
    assert.equal(count(plain, `SELECT COUNT(*) AS n FROM passkeys WHERE id = ${id}`), 1);
    const r = await api(plain, "DELETE", `/api/auth/passkey/${id}`, { cookie: S.olga, body: { current_password: PW.get(S.olga) } });
    assert.equal(r.status, 204, r.text);
    assert.equal(count(plain, `SELECT COUNT(*) AS n FROM passkeys WHERE id = ${id}`), 0);
    assert.equal(count(plain, "SELECT COUNT(*) AS n FROM system_log WHERE kind = 'auth.passkey_removed' AND actor_id = 1"), 1);
    // Gone: the next sign-in with it is unknown.
    refused(await login(plain, SEEDED.olga, { count: 200 }));
  });

  test("passkey-only account: last passkey stays; login change re-authenticates with a passkey", async () => {
    const a = SEEDED.pat;
    const [key] = (await api(plain, "GET", "/api/auth/passkey", { cookie: S.pat })).json.passkeys;
    assert.equal(key.name, "pat phone");
    const body = await reauthWith(plain, a);
    assertError(await api(plain, "DELETE", `/api/auth/passkey/${key.id}`, { cookie: S.pat, body }), 409, "store.conflict");

    const patch = (o) => login(plain, a, { method: "PATCH", path: "/api/auth/me", cookie: S.pat, ...o });
    // No assertion, a password instead, or another user's passkey: refused, nothing changed.
    assertError(
      await api(plain, "PATCH", "/api/auth/me", { cookie: S.pat, body: { login: "patty" } }),
      400,
      "input.invalid",
    );
    assertError(await patch({ extra: { login: "patty", current_password: "x", new_password: "y" } }), 400, "input.invalid");
    const wes = new Authenticator("ES256");
    assert.equal((await register(plain, S.wes, wes)).status, 201);
    refused(await login(plain, wes, { method: "PATCH", path: "/api/auth/me", cookie: S.pat, extra: { login: "patty" } }));
    refused(await patch({ extra: { login: "patty" }, origin: "https://evil.example" }));
    assert.equal(plain.query("SELECT login FROM users WHERE id = 2")[0].login, "pat");

    const r = await patch({ extra: { login: "patty" } });
    assert.equal(r.status, 200, r.text);
    assert.equal(r.json.user.login, "patty");
    const next = tokenOf(r.setCookie);
    assert.ok(next && next !== S.pat);
    assertError(await api(plain, "GET", "/api/auth/me", { cookie: S.pat }), 401, "auth.unauthenticated");
    assert.equal((await api(plain, "GET", "/api/auth/me", { cookie: next })).json.user.login, "patty");
    assert.deepEqual(plain.query("SELECT login, password_hash FROM users WHERE id = 2"), [{ login: "patty", password_hash: null }]);

    // An account with a password keeps the password path.
    assertError(
      await login(plain, wes, { method: "PATCH", path: "/api/auth/me", cookie: S.wes, extra: { login: "wesley" } }),
      400,
      "input.invalid",
    );
  });

  test("adding or removing a passkey re-authenticates: none 403, wrong password 401 and counted, password or own passkey ok", async () => {
    const wesKey = sq(sha256hex("login:wes"));
    const failures = () => plain.query(`SELECT count FROM login_failures WHERE key_hash = ${wesKey}`)[0]?.count ?? 0;
    const passkeys = () => count(plain, "SELECT COUNT(*) AS n FROM passkeys WHERE user_id = 4");
    const before = passkeys();
    const a = new Authenticator("ES256");
    assertError(await register(plain, S.wes, a, { reauth: null }), 403, "auth.reauth_required");
    const counted = failures();
    assertError(await register(plain, S.wes, a, { reauth: { current_password: b64u(randomBytes(32)) } }), 401, "auth.invalid_credentials");
    assert.equal(failures(), counted + 1);
    // Both at once is a malformed request, refused before counting.
    const both = { current_password: PW.get(S.wes), ...(await reauthWith(plain, SEEDED.olga)) };
    assertError(await register(plain, S.wes, a, { reauth: both }), 400, "input.invalid");
    // Another user's passkey does not re-authenticate this one.
    const olgaKey = new Authenticator("Ed25519");
    assert.equal((await register(plain, S.olga, olgaKey)).status, 201);
    refused(await register(plain, S.wes, a, { reauth: await reauthWith(plain, olgaKey) }));
    assert.equal(passkeys(), before);

    // With the password; then a second key with an assertion from the first.
    const viaPassword = await register(plain, S.wes, a);
    assert.equal(viaPassword.status, 201, viaPassword.text);
    const b = new Authenticator("Ed25519");
    const viaKey = await register(plain, S.wes, b, { reauth: await reauthWith(plain, a) });
    assert.equal(viaKey.status, 201, viaKey.text);

    // `me` lists the user's passkeys with when they were added and last used.
    const me = await api(plain, "GET", "/api/auth/me", { cookie: S.wes });
    assert.equal(me.status, 200, me.text);
    assert.equal(me.json.user.login, "wes");
    const listed = me.json.passkeys.find((k) => k.id === viaKey.json.passkey.id);
    assert.deepEqual(Object.keys(listed).sort(), ["created_at", "id", "last_used_at", "name"]);
    assert.ok(listed.created_at >= T);
    assert.equal(listed.last_used_at, null);
    assert.ok(!JSON.stringify(me.json).includes(a.id), "no credential ids or keys in me");

    // Removing: none, wrong password, then the other key's assertion and the password.
    const del = (id, body) => api(plain, "DELETE", `/api/auth/passkey/${id}`, { cookie: S.wes, body });
    const aId = viaPassword.json.passkey.id;
    const bId = viaKey.json.passkey.id;
    assertError(await del(bId), 403, "auth.reauth_required");
    assertError(await del(bId, {}), 403, "auth.reauth_required");
    const c2 = failures();
    assertError(await del(bId, { current_password: b64u(randomBytes(32)) }), 401, "auth.invalid_credentials");
    assert.equal(failures(), c2 + 1);
    refused(await del(bId, await reauthWith(plain, olgaKey)));
    assert.equal(count(plain, `SELECT COUNT(*) AS n FROM passkeys WHERE id = ${bId}`), 1);
    const removed = count(plain, "SELECT COUNT(*) AS n FROM system_log WHERE kind = 'auth.passkey_removed' AND actor_id = 4");
    assert.equal((await del(bId, await reauthWith(plain, a))).status, 204);
    assert.equal((await del(aId, { current_password: PW.get(S.wes) })).status, 204);
    assert.equal(count(plain, `SELECT COUNT(*) AS n FROM passkeys WHERE id IN (${aId}, ${bId})`), 0);
    assert.equal(count(plain, "SELECT COUNT(*) AS n FROM system_log WHERE kind = 'auth.passkey_removed' AND actor_id = 4"), removed + 2);
    assert.equal(passkeys(), before);
  });

  test("an ES256 key off the curve: registration refused; a stored one fails sign-in as passkey_invalid, counted", async () => {
    const failed = () => count(plain, "SELECT COUNT(*) AS n FROM system_log WHERE kind = 'auth.sign_in_failed' AND actor_id = 4");
    const logged = failed();
    const r = await login(plain, SEEDED.bad);
    refused(r);
    const key = sq(sha256hex(`passkey:${SEEDED.bad.id}`));
    assert.equal(count(plain, `SELECT count AS n FROM login_failures WHERE key_hash = ${key}`), 1);
    assert.equal(failed(), logged + 1);
    const a = new Authenticator("ES256");
    const cose = new Map([[1, 2], [3, -7], [-1, 1], [-2, Buffer.alloc(32, 7)], [-3, Buffer.alloc(32, 9)]]);
    refused(await register(plain, S.wes, a, { cose }));
    assert.equal(count(plain, `SELECT COUNT(*) AS n FROM passkeys WHERE credential_id = ${sq(a.id)}`), 0);
  });

  test("system log: passkey events, no challenges, keys or addresses", async () => {
    const rows = plain.query("SELECT kind, detail FROM system_log");
    const kinds = new Set(rows.map((r) => r.kind));
    for (const k of ["auth.passkey_added", "auth.passkey_removed", "auth.sign_in", "auth.sign_in_failed", "auth.sign_in_locked"]) {
      assert.ok(kinds.has(k), `missing ${k}`);
    }
    const text = JSON.stringify(rows);
    for (const secret of [SEEDED.olga.id, SEEDED.olga.stored(), "198.18.", "192.0.2", b64u(EXPIRED)]) {
      assert.ok(!text.includes(secret), `leaked ${secret}`);
    }
  });
});

// --- configured origin ---------------------------------------------------------------------

describe("passkeys, configured origin", () => {
  test("CSRF compares Origin with the configured origin, not the request's own", async () => {
    assertError(await api(configured, "POST", LOGIN_OPT, { origin: configured.url }), 403, "auth.forbidden");
    assertError(await api(configured, "POST", "/api/auth/logout", { origin: configured.url, cookie: S.cara }), 403, "auth.forbidden");
    assert.equal((await api(configured, "POST", LOGIN_OPT)).status, 200);
    assert.equal((await api(configured, "GET", "/api/auth/me", { cookie: S.cara, origin: null })).status, 200);
  });

  test("the configured origin is the relying party, not the request's host", async () => {
    const opt = await api(configured, "POST", REG_OPT, { cookie: S.cara });
    assert.equal(opt.json.publicKey.rp.id, "blog.test");
    assert.equal((await api(configured, "POST", LOGIN_OPT)).json.publicKey.rpId, "blog.test");

    const a = new Authenticator("ES256");
    // Built for the host the request came to: refused.
    refused(await register(configured, S.cara, a, { origin: configured.url, rpId: "127.0.0.1" }));
    refused(await register(configured, S.cara, a, { rpId: "127.0.0.1" }));
    const reg = await register(configured, S.cara, a);
    assert.equal(reg.status, 201, reg.text);
    refused(await login(configured, a, { origin: configured.url }));
    refused(await login(configured, a, { rpId: "127.0.0.1" }));
    const ok = await login(configured, a);
    assert.equal(ok.status, 200, ok.text);
    assert.equal(ok.json.user.login, "cara");
  });
});
