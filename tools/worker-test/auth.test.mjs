// /api/auth/* over HTTP against the real Worker (wrangler dev, local D1), plus the WebCrypto
// adapter through the test-only /__crypto route. Three Workers start in parallel:
//   fresh:   site_id + a valid setup code, Turnstile off (setup, sign-in, sessions, CSRF).
//   seeded:  users, sessions and invites written by SQL before start, Turnstile on (stub).
//   blank:   no site_id, an expired setup code; also runs the crypto checks.
// Node computes stretched passwords with node:crypto, like the browser would.
import assert from "node:assert/strict";
import {
  createHash, generateKeyPairSync, pbkdf2, pbkdf2Sync, randomBytes, sign as edSign, createSign,
} from "node:crypto";
import { createServer } from "node:http";
import { after, before, describe, test } from "node:test";
import { promisify } from "node:util";

import { startWorker } from "./harness.mjs";
import { stretch as stretchJs } from "../../web/admin/src/auth/stretch.ts";

const ITER = 600000; // crates/auth STRETCH_ITERATIONS; stretch.test.mjs pins it.
const COOKIE = "__Host-plumblog_session";
const DAY = 86_400_000;
const WINDOW = 900_000; // crates/auth ratelimit::WINDOW_MS
const sha256hex = (s) => createHash("sha256").update(s).digest("hex");
const b64u = (buf) => Buffer.from(buf).toString("base64url");
const pbkdf2Async = promisify(pbkdf2);

const stretchCache = new Map();
async function stretch(site, login, password) {
  const k = `${site}\0${login}\0${password}`;
  if (!stretchCache.has(k)) {
    const salt = `plumblog:${site}:${login.toLowerCase()}`;
    stretchCache.set(k, pbkdf2Async(password, salt, ITER, 32, "sha256").then(b64u));
  }
  return stretchCache.get(k);
}
const wrong = () => b64u(randomBytes(32));
/** Stored form of crates/auth hash_password, for seeded users. */
function serverHash(stretched) {
  const salt = randomBytes(16);
  const h = pbkdf2Sync(Buffer.from(stretched, "base64url"), salt, 1000, 32, "sha256");
  return `p1$1000$${b64u(salt)}$${b64u(h)}`;
}
const sq = (s) => `'${String(s).replaceAll("'", "''")}'`;

/** One API call. `origin: null` omits the header; default is the Worker's own origin. */
async function api(w, method, path, { body, cookie, origin, ip = "203.0.113.1", headers = {} } = {}) {
  const h = { ...headers, "cf-connecting-ip": ip };
  if (origin !== null) h.origin = origin ?? w.url;
  if (cookie) h.cookie = `${COOKIE}=${cookie}`;
  let payload;
  if (body !== undefined) {
    h["content-type"] = "application/json";
    payload = typeof body === "string" ? body : JSON.stringify(body);
  }
  const res = await fetch(w.url + path, { method, headers: h, body: payload });
  const text = await res.text();
  return {
    status: res.status,
    text,
    json: text ? JSON.parse(text) : null,
    setCookie: res.headers.get("set-cookie"),
    cache: res.headers.get("cache-control"),
  };
}
const tokenOf = (setCookie) => setCookie?.match(/^__Host-plumblog_session=([A-Za-z0-9_-]*);/)?.[1];
const CLEAR = `${COOKIE}=; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=0`;
const freshCookie = /^__Host-plumblog_session=[A-Za-z0-9_-]{43}; HttpOnly; Secure; SameSite=Strict; Path=\/; Max-Age=2592000$/;

function assertError(r, status, code) {
  assert.equal(r.status, status, r.text);
  assert.deepEqual(r.json, { error: code });
}

// --- workers -----------------------------------------------------------------------------

const SITE_A = "site-fresh-1";
const SETUP_CODE = "setup-code-0123456789abcdef";
const OWNER = { login: "Owner", slug: "owner", display_name: "The Owner", password: "owner pass 1" };

const SITE_B = "site-seeded-2";
const T = Date.now();
const tok = () => b64u(randomBytes(32));
const S = { wendy: tok(), expired: tok(), renew: tok(), dora: tok(), bob: tok() };
const INVITE_OWNER = "invite-owner-code";
const INVITE_EXPIRED = "invite-expired-code";
const TS_SITE_KEY = "1x00000000000000000000AA";
const TS_SECRET = "stub-secret-value";

let fresh, seeded, blank, stub;
const stubCalls = [];

before(async () => {
  stub = createServer((req, res) => {
    let data = "";
    req.on("data", (d) => (data += d));
    req.on("end", () => {
      const form = new URLSearchParams(data);
      stubCalls.push(form);
      res.setHeader("content-type", "application/json");
      // Like Cloudflare: `hostname` is where the widget was solved. "pass-elsewhere" is a real
      // token from another site.
      const response = form.get("response");
      const success = response === "pass" || response === "pass-elsewhere";
      const hostname = response === "pass" ? "127.0.0.1" : "evil.example";
      res.end(JSON.stringify({ success, hostname, "error-codes": [] }));
    });
  });
  await new Promise((ok) => stub.listen(0, "127.0.0.1", ok));
  const stubUrl = `http://127.0.0.1:${stub.address().port}/verify`;

  const [bobPw, wendyPw, doraPw] = await Promise.all([
    stretch(SITE_B, "Bob", "bob pass"),
    stretch(SITE_B, "wendy", "wendy pass"),
    stretch(SITE_B, "dora", "dora pass"),
  ]);
  const seedB = [
    `INSERT INTO settings (key, value_json, updated_at) VALUES ('site_id', ${sq(JSON.stringify(SITE_B))}, ${T}),
      ('turnstile_site_key', ${sq(JSON.stringify(TS_SITE_KEY))}, ${T});`,
    `INSERT INTO users (id, login, slug, display_name, role, password_hash, disabled_at, created_at, updated_at) VALUES
      (1, 'Bob', 'bob', 'Bob', 'owner', ${sq(serverHash(bobPw))}, NULL, ${T}, ${T}),
      (2, 'wendy', 'wendy', 'Wendy', 'writer', ${sq(serverHash(wendyPw))}, NULL, ${T}, ${T}),
      (3, 'dora', 'dora', 'Dora', 'writer', ${sq(serverHash(doraPw))}, ${T - 1000}, ${T}, ${T});`,
    `INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES
      (${sq(sha256hex(S.wendy))}, 2, ${T}, ${T + 29 * DAY}),
      (${sq(sha256hex(S.expired))}, 1, ${T - 31 * DAY}, ${T - 1000}),
      (${sq(sha256hex(S.renew))}, 1, ${T - 20 * DAY}, ${T + 10 * DAY}),
      (${sq(sha256hex(S.dora))}, 3, ${T}, ${T + 29 * DAY}),
      (${sq(sha256hex(S.bob))}, 1, ${T}, ${T + 29 * DAY});`,
    `INSERT INTO invites (code_hash, role, created_by, created_at, expires_at) VALUES
      (${sq(sha256hex(INVITE_OWNER))}, 'owner', 1, ${T}, ${T + DAY}),
      (${sq(sha256hex(INVITE_EXPIRED))}, 'writer', 1, ${T - 2 * DAY}, ${T - DAY});`,
  ].join("\n");

  const seedA = `INSERT INTO settings (key, value_json, updated_at) VALUES
    ('site_id', ${sq(JSON.stringify(SITE_A))}, ${T}),
    ('setup_code_hash', ${sq(JSON.stringify(sha256hex(SETUP_CODE)))}, ${T}),
    ('setup_code_expires_at', '${T + DAY}', ${T});`;
  const seedC = `INSERT INTO settings (key, value_json, updated_at) VALUES
    ('setup_code_hash', ${sq(JSON.stringify(sha256hex(SETUP_CODE)))}, ${T}),
    ('setup_code_expires_at', '${T - 1}', ${T});
    INSERT INTO login_failures (key_hash, count, window_start) VALUES
    ('ended-1', 3, ${T - 2 * WINDOW}), ('ended-2', 20, ${T - WINDOW}), ('open-1', 2, ${T});`;

  [fresh, seeded, blank] = await Promise.all([
    startWorker({ seedSql: seedA }),
    // Turnstile checks the host a token names against the configured origin.
    startWorker({
      seedSql: (url) => `${seedB}\nINSERT INTO settings (key, value_json, updated_at) VALUES ('origin', ${sq(JSON.stringify(url))}, ${T});`,
      // This site makes more attempts than the site-wide cap allows in one window; the cap
      // itself is covered by globalcap.test.mjs.
      vars: { TURNSTILE_SECRET: TS_SECRET, TURNSTILE_VERIFY_URL: stubUrl, PLUMBLOG_TEST_GLOBAL_CAP: "100000" },
    }),
    startWorker({ seedSql: seedC }),
  ]);
});

after(async () => {
  await Promise.all([fresh?.stop(), seeded?.stop(), blank?.stop()]);
  stub?.close();
});

// --- fresh site: setup, sign-in, sessions, CSRF ------------------------------------------

describe("fresh site", () => {
  let ownerCookie;
  const login = async (w, site, who, password, opts = {}) =>
    api(w, "POST", "/api/auth/login", {
      body: { login: who, password: await stretch(site, who, password) },
      ...opts,
    });

  test("params: site id, stretch iterations, no Turnstile, no internal keys", async () => {
    const r = await api(fresh, "GET", "/api/auth/params");
    assert.equal(r.status, 200);
    assert.deepEqual(r.json, { site_id: SITE_A, stretch_iterations: ITER, turnstile_site_key: null, setup_required: true });
    assert.equal(r.cache, "no-store");
    assert.doesNotMatch(r.text, /setup_code/);
  });

  test("anonymous: me and the owner-only probe are 401", async () => {
    assertError(await api(fresh, "GET", "/api/auth/me"), 401, "auth.unauthenticated");
    assertError(await api(fresh, "GET", "/api/__probe/owner"), 401, "auth.unauthenticated");
    assertError(await api(fresh, "GET", "/api/nope"), 404, "api.not_found");
  });

  test("CSRF: POST without or with a foreign Origin is 403 before anything runs", async () => {
    const body = { login: "x", password: wrong() };
    for (const origin of [null, "http://evil.example", fresh.url.replace("127.0.0.1", "localhost"), "null"]) {
      assertError(await api(fresh, "POST", "/api/auth/login", { body, origin }), 403, "auth.forbidden");
    }
    // Nothing counted: the rate-limit table is still empty.
    assert.deepEqual(fresh.query("SELECT COUNT(*) AS n FROM login_failures"), [{ n: 0 }]);
  });

  test("setup: wrong code refused, nothing created", async () => {
    const body = { ...OWNER, code: "wrong-code", password: await stretch(SITE_A, OWNER.login, OWNER.password) };
    assertError(await api(fresh, "POST", "/api/auth/setup", { body }), 403, "setup.invalid");
    assert.deepEqual(fresh.query("SELECT COUNT(*) AS n FROM users"), [{ n: 0 }]);
  });

  test("setup: bad input is 400 (unknown field, short password, bad slug)", async () => {
    const pw = await stretch(SITE_A, OWNER.login, OWNER.password);
    for (const body of [
      { ...OWNER, code: SETUP_CODE, password: pw, role: "owner" },
      { ...OWNER, code: SETUP_CODE, password: "c2hvcnQ" },
      { ...OWNER, code: SETUP_CODE, password: pw, slug: "Bad Slug" },
      "not json",
    ]) {
      assertError(await api(fresh, "POST", "/api/auth/setup", { body }), 400, "input.invalid");
    }
  });

  test("setup: wrong codes count per address; 5 lock it (Turnstile off), even for the right code", async () => {
    const ip = "198.51.100.30";
    const pw = await stretch(SITE_A, OWNER.login, OWNER.password);
    for (let i = 0; i < 5; i++) {
      const r = await api(fresh, "POST", "/api/auth/setup", { body: { ...OWNER, code: `guess-${i}`, password: pw }, ip });
      assertError(r, 403, "setup.invalid");
    }
    const right = { ...OWNER, code: SETUP_CODE, password: pw };
    assertError(await api(fresh, "POST", "/api/auth/setup", { body: right, ip }), 429, "auth.locked");
    assert.deepEqual(fresh.query("SELECT COUNT(*) AS n FROM users"), [{ n: 0 }]);
    assert.ok(fresh.query("SELECT COUNT(*) AS n FROM system_log WHERE kind = 'auth.sign_in_locked' AND detail = 'setup code'")[0].n >= 1);
  });

  test("setup: right code creates the owner, signs in, consumes the code; second setup refused", async () => {
    const body = { ...OWNER, code: SETUP_CODE, password: await stretch(SITE_A, OWNER.login, OWNER.password) };
    const r = await api(fresh, "POST", "/api/auth/setup", { body });
    assert.equal(r.status, 201, r.text);
    assert.equal(r.json.user.role, "owner");
    assert.equal(r.json.user.login, "Owner");
    assert.match(r.setCookie, freshCookie);
    ownerCookie = tokenOf(r.setCookie);
    const me = await api(fresh, "GET", "/api/auth/me", { cookie: ownerCookie });
    assert.equal(me.status, 200);
    assert.equal(me.json.user.slug, "owner");
    assert.equal(me.setCookie, null, "a fresh session is not renewed");
    assert.deepEqual(fresh.query("SELECT key FROM settings WHERE key LIKE 'setup_code%'"), []);
    assertError(await api(fresh, "POST", "/api/auth/setup", { body }), 403, "setup.invalid");
    // The sign-in page learns that setup is over, also when the only owner is later disabled
    // (setup refuses while any user row exists).
    assert.equal((await api(fresh, "GET", "/api/auth/params")).json.setup_required, false);
    fresh.exec("UPDATE users SET disabled_at = 1");
    assert.equal((await api(fresh, "GET", "/api/auth/params")).json.setup_required, false);
    fresh.exec("UPDATE users SET disabled_at = NULL");
    // Only the hash of the session token is stored.
    const rows = fresh.query("SELECT token_hash FROM sessions");
    assert.deepEqual(rows, [{ token_hash: sha256hex(ownerCookie) }]);
  });

  test("owner reaches the owner-only probe", async () => {
    const r = await api(fresh, "GET", "/api/__probe/owner", { cookie: ownerCookie });
    assert.equal(r.status, 204, r.text);
  });

  test("login: ok sets an exact cookie and rotates the session it came with", async () => {
    const r = await login(fresh, SITE_A, "owner", OWNER.password, { cookie: ownerCookie });
    assert.equal(r.status, 200, r.text);
    assert.match(r.setCookie, freshCookie);
    const next = tokenOf(r.setCookie);
    assert.notEqual(next, ownerCookie);
    const old = await api(fresh, "GET", "/api/auth/me", { cookie: ownerCookie });
    assertError(old, 401, "auth.unauthenticated");
    assert.equal(old.setCookie, CLEAR);
    assert.equal((await api(fresh, "GET", "/api/auth/me", { cookie: next })).status, 200);
    ownerCookie = next;
  });

  test("login: unknown user and wrong password answer the same", async () => {
    const a = await api(fresh, "POST", "/api/auth/login", { body: { login: "owner", password: wrong() }, ip: "203.0.113.20" });
    const b = await api(fresh, "POST", "/api/auth/login", { body: { login: "nobody", password: wrong() }, ip: "203.0.113.21" });
    assertError(a, 401, "auth.invalid_credentials");
    assert.equal(b.status, a.status);
    assert.equal(b.text, a.text);
    assert.equal(a.setCookie, null);
    // Keys are SHA-256 hex only: no raw login or address in the table. The one exception is
    // the fixed site-wide key (ratelimit::GLOBAL_KEY).
    for (const { key_hash } of fresh.query("SELECT key_hash FROM login_failures WHERE key_hash <> 'global'")) {
      assert.match(key_hash, /^[0-9a-f]{64}$/);
    }
  });

  test("Turnstile not set up: 5 failures from one address lock it, even for a right password", async () => {
    const ip = "198.51.100.7";
    for (let i = 0; i < 5; i++) {
      const r = await api(fresh, "POST", "/api/auth/login", { body: { login: `ghost${i}`, password: wrong() }, ip });
      assertError(r, 401, "auth.invalid_credentials");
    }
    assertError(await login(fresh, SITE_A, "owner", OWNER.password, { ip }), 429, "auth.locked");
    // Another address still signs in: the lock is per key.
    assert.equal((await login(fresh, SITE_A, "owner", OWNER.password, { ip: "198.51.100.8" })).status, 200);
    assert.equal(fresh.query("SELECT COUNT(*) AS n FROM system_log WHERE kind = 'auth.sign_in_locked' AND detail = 'password'")[0].n, 1);
  });

  test("a locked address writes nothing: no rows for new logins, its own count stays, the site-wide count stays", async () => {
    const ip = "198.51.100.90";
    for (let i = 0; i < 5; i++) {
      const r = await api(fresh, "POST", "/api/auth/login", { body: { login: `random${i}`, password: wrong() }, ip });
      assertError(r, 401, "auth.invalid_credentials");
    }
    // Refused from a read of its own row: one address cannot fill the site-wide cap.
    const snap = () => fresh.query("SELECT key_hash, count, window_start FROM login_failures WHERE key_hash <> 'global' ORDER BY key_hash");
    const global = () => fresh.query("SELECT count FROM login_failures WHERE key_hash = 'global'")[0].count;
    const before = snap();
    const globalBefore = global();
    for (let i = 0; i < 10; i++) {
      const r = await api(fresh, "POST", "/api/auth/login", { body: { login: `later${i}`, password: wrong() }, ip });
      assertError(r, 429, "auth.locked");
    }
    assert.deepEqual(snap(), before);
    assert.equal(global(), globalBefore, "a locked address moved the site-wide count");
  });

  test("bodies: over 8 KiB by Content-Length is 413 before reading; no Content-Length (chunked) is 411", async () => {
    const ip = "198.51.100.91";
    const big = JSON.stringify({ login: "x", password: "a".repeat(9000) });
    assertError(await api(fresh, "POST", "/api/auth/login", { body: big, ip }), 413, "api.body_too_large");
    const stream = new ReadableStream({
      start(c) {
        c.enqueue(new TextEncoder().encode('{"login":"x","password":"'));
        c.enqueue(new TextEncoder().encode(`${wrong()}"}`));
        c.close();
      },
    });
    const res = await fetch(`${fresh.url}/api/auth/login`, {
      method: "POST",
      body: stream,
      duplex: "half",
      headers: { origin: fresh.url, "content-type": "application/json", "cf-connecting-ip": ip },
    });
    assert.equal(res.status, 411, await res.clone().text());
    assert.deepEqual(await res.json(), { error: "api.length_required" });
    // Neither was counted: refused before the route ran.
    assert.equal(fresh.query(`SELECT COUNT(*) AS n FROM login_failures WHERE key_hash = ${sq(sha256hex("ip:" + ip))}`)[0].n, 0);
  });

  test("no configured origin: passkeys refuse with settings.origin_missing; passwords still work", async () => {
    for (const [path, opts] of [
      ["/api/auth/passkey/login/options", {}],
      ["/api/auth/passkey/register/options", { cookie: ownerCookie }],
      ["/api/auth/passkey/register/verify", { cookie: ownerCookie, body: { name: "k", client_data_json: "e30", attestation_object: "oA" } }],
      ["/api/auth/passkey/login/verify", { body: { credential_id: "AAAA", client_data_json: "e30", authenticator_data: "AA", signature: "AA" } }],
    ]) {
      assertError(await api(fresh, "POST", path, { ...opts, ip: "198.51.100.60" }), 503, "settings.origin_missing");
    }
    // Nothing spent or counted.
    assert.deepEqual(fresh.query("SELECT COUNT(*) AS n FROM auth_challenges"), [{ n: 0 }]);
    assert.equal((await login(fresh, SITE_A, "owner", OWNER.password, { ip: "198.51.100.60" })).status, 200);
  });

  test("an Authorization header that is not an API token is 401 (tokens.test.mjs has the rest)", async () => {
    const r = await api(fresh, "GET", "/api/auth/me", { headers: { authorization: "Bearer x" } });
    assertError(r, 401, "auth.unauthenticated");
  });

  test("password change: wrong current refused; right one ends other sessions and rotates own", async () => {
    const other = tokenOf((await login(fresh, SITE_A, "owner", OWNER.password, { ip: "203.0.113.30" })).setCookie);
    assertError(
      await api(fresh, "POST", "/api/auth/password", { body: { current_password: wrong(), new_password: wrong() } }),
      401,
      "auth.unauthenticated",
    );
    const bad = await api(fresh, "POST", "/api/auth/password", {
      cookie: ownerCookie,
      body: { current_password: wrong(), new_password: wrong() },
    });
    assertError(bad, 401, "auth.invalid_credentials");
    assert.equal((await api(fresh, "GET", "/api/auth/me", { cookie: ownerCookie })).status, 200);

    const newPw = await stretch(SITE_A, "owner", "owner pass 2");
    const r = await api(fresh, "POST", "/api/auth/password", {
      cookie: ownerCookie,
      body: { current_password: await stretch(SITE_A, "Owner", OWNER.password), new_password: newPw },
    });
    assert.equal(r.status, 204, r.text);
    assert.match(r.setCookie, freshCookie);
    const rotated = tokenOf(r.setCookie);
    assertError(await api(fresh, "GET", "/api/auth/me", { cookie: ownerCookie }), 401, "auth.unauthenticated");
    assertError(await api(fresh, "GET", "/api/auth/me", { cookie: other }), 401, "auth.unauthenticated");
    assert.equal((await api(fresh, "GET", "/api/auth/me", { cookie: rotated })).status, 200);
    assertError(await login(fresh, SITE_A, "owner", OWNER.password, { ip: "203.0.113.31" }), 401, "auth.invalid_credentials");
    assert.equal((await login(fresh, SITE_A, "owner", "owner pass 2", { ip: "203.0.113.31" })).status, 200);
    ownerCookie = rotated;
  });

  test("profile: a plain edit keeps the session; a login change needs the current password", async () => {
    const plain = await api(fresh, "PATCH", "/api/auth/me", { cookie: ownerCookie, body: { display_name: "Renamed" } });
    assert.equal(plain.status, 200, plain.text);
    assert.equal(plain.json.user.display_name, "Renamed");
    assert.equal(plain.setCookie, null);

    const newPw = await stretch(SITE_A, "chief", "owner pass 2");
    assertError(
      await api(fresh, "PATCH", "/api/auth/me", { cookie: ownerCookie, body: { login: "chief", new_password: newPw } }),
      400,
      "input.invalid",
    );
    assertError(
      await api(fresh, "PATCH", "/api/auth/me", {
        cookie: ownerCookie,
        body: { login: "chief", current_password: wrong(), new_password: newPw },
      }),
      401,
      "auth.invalid_credentials",
    );
    assertError(
      await api(fresh, "PATCH", "/api/auth/me", { body: { display_name: "x" } }),
      401,
      "auth.unauthenticated",
    );
    const r = await api(fresh, "PATCH", "/api/auth/me", {
      cookie: ownerCookie,
      body: { login: "chief", current_password: await stretch(SITE_A, "owner", "owner pass 2"), new_password: newPw },
    });
    assert.equal(r.status, 200, r.text);
    assert.equal(r.json.user.login, "chief");
    assert.match(r.setCookie, freshCookie);
    assertError(await api(fresh, "GET", "/api/auth/me", { cookie: ownerCookie }), 401, "auth.unauthenticated");
    ownerCookie = tokenOf(r.setCookie);
    assert.equal((await login(fresh, SITE_A, "chief", "owner pass 2", { ip: "203.0.113.40" })).status, 200);
    assertError(await login(fresh, SITE_A, "owner", "owner pass 2", { ip: "203.0.113.40" }), 401, "auth.invalid_credentials");
  });

  test("logout ends the session and clears the cookie", async () => {
    const r = await api(fresh, "POST", "/api/auth/logout", { cookie: ownerCookie });
    assert.equal(r.status, 204);
    assert.equal(r.setCookie, CLEAR);
    assertError(await api(fresh, "GET", "/api/auth/me", { cookie: ownerCookie }), 401, "auth.unauthenticated");
    assert.equal((await api(fresh, "POST", "/api/auth/logout")).status, 204, "signing out twice is harmless");
  });

  test("system log: events recorded, no secrets or addresses", async () => {
    const rows = fresh.query("SELECT kind, detail, request_id FROM system_log ORDER BY id");
    const kinds = new Set(rows.map((r) => r.kind));
    for (const k of ["user.created", "auth.sign_in", "auth.sign_in_failed", "auth.sign_in_locked", "auth.password_changed", "auth.sign_out"]) {
      assert.ok(kinds.has(k), `missing ${k}`);
    }
    const text = JSON.stringify(rows) + JSON.stringify(fresh.query("SELECT * FROM settings"));
    for (const secret of [SETUP_CODE, sha256hex(SETUP_CODE), "198.51.100", "203.0.113", await stretch(SITE_A, "chief", "owner pass 2")]) {
      assert.ok(!text.includes(secret), `leaked ${secret}`);
    }
  });
});

// --- seeded site: Turnstile, lockout, renewal, expired, disabled, writer, invites -------

describe("seeded site", () => {
  const bobLogin = async (opts = {}) =>
    api(seeded, "POST", "/api/auth/login", {
      body: { login: "bob", password: await stretch(SITE_B, "bob", "bob pass"), ...opts.extra },
      ip: opts.ip,
    });

  test("params offer the Turnstile site key when the secret is set", async () => {
    const r = await api(seeded, "GET", "/api/auth/params");
    assert.deepEqual(r.json, { site_id: SITE_B, stretch_iterations: ITER, turnstile_site_key: TS_SITE_KEY, setup_required: false });
  });

  test("Turnstile is required after 5 failures; a valid token lets a right password in", async () => {
    const ip = "192.0.2.10";
    for (let i = 0; i < 5; i++) {
      const r = await api(seeded, "POST", "/api/auth/login", { body: { login: "bob", password: wrong() }, ip });
      assertError(r, 401, "auth.invalid_credentials");
    }
    assertError(await bobLogin({ ip }), 403, "auth.turnstile_required");
    assertError(await bobLogin({ ip, extra: { turnstile_token: "fail" } }), 403, "auth.turnstile_required");
    const ok = await bobLogin({ ip, extra: { turnstile_token: "pass" } });
    assert.equal(ok.status, 200, ok.text);
    const last = stubCalls.at(-1);
    assert.equal(last.get("secret"), TS_SECRET);
    assert.equal(last.get("remoteip"), null, "the client address is not sent");
  });

  test("Turnstile: a token solved on another host is refused", async () => {
    const ip = "192.0.2.11";
    for (let i = 0; i < 5; i++) {
      const r = await api(seeded, "POST", "/api/auth/login", { body: { login: "bob", password: wrong() }, ip });
      assertError(r, 401, "auth.invalid_credentials");
    }
    assertError(await bobLogin({ ip, extra: { turnstile_token: "pass-elsewhere" } }), 403, "auth.turnstile_required");
    assert.equal(stubCalls.at(-1).get("response"), "pass-elsewhere");
    assert.equal((await bobLogin({ ip, extra: { turnstile_token: "pass" } })).status, 200);
  });

  test("30 concurrent wrong passwords for one login: at most 5 reach the password check", async () => {
    const failedBefore = seeded.query("SELECT COUNT(*) AS n FROM system_log WHERE kind = 'auth.sign_in_failed' AND detail = 'password'")[0].n;
    const answers = await Promise.all(
      Array.from({ length: 30 }, (_, i) =>
        api(seeded, "POST", "/api/auth/login", { body: { login: "racer", password: wrong() }, ip: `192.0.2.${100 + i}` }),
      ),
    );
    const checked = answers.filter((r) => r.status === 401).length;
    // The rest are refused unchecked: Turnstile after 5, locked from 20.
    for (const r of answers.filter((x) => x.status !== 401)) {
      assert.ok(["auth.turnstile_required", "auth.locked"].includes(r.json.error), r.text);
    }
    assert.ok(checked >= 1 && checked <= 5, `${checked} attempts reached the password check`);
    const failedAfter = seeded.query("SELECT COUNT(*) AS n FROM system_log WHERE kind = 'auth.sign_in_failed' AND detail = 'password'")[0].n;
    assert.equal(failedAfter - failedBefore, checked);
    const row = seeded.query(`SELECT count FROM login_failures WHERE key_hash = ${sq(sha256hex("login:racer"))}`);
    // Counted up to the lock (20 with Turnstile on), then a locked key costs no more writes.
    assert.equal(row[0].count, 20);
  });

  test("20 failures lock even with a valid Turnstile token", async () => {
    const ip = "192.0.2.20";
    for (let i = 0; i < 20; i++) {
      const r = await api(seeded, "POST", "/api/auth/login", {
        body: { login: "mallory", password: wrong(), turnstile_token: "pass" },
        ip,
      });
      assertError(r, 401, "auth.invalid_credentials");
    }
    assertError(await bobLogin({ ip, extra: { turnstile_token: "pass" } }), 429, "auth.locked");
    const locks = seeded.query("SELECT COUNT(*) AS n FROM system_log WHERE kind = 'auth.sign_in_locked'")[0].n;
    assert.ok(locks >= 1);
  });

  test("renewal: under half the lifetime left renews once, same token, full Max-Age", async () => {
    const r = await api(seeded, "GET", "/api/auth/me", { cookie: S.renew });
    assert.equal(r.status, 200);
    assert.equal(r.setCookie, `${COOKIE}=${S.renew}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=2592000`);
    const [{ expires_at }] = seeded.query(`SELECT expires_at FROM sessions WHERE token_hash = '${sha256hex(S.renew)}'`);
    assert.ok(expires_at > T + 29 * DAY, `renewed expiry ${expires_at}`);
    const again = await api(seeded, "GET", "/api/auth/me", { cookie: S.renew });
    assert.equal(again.setCookie, null, "a renewed session is not renewed again");
    assert.equal((await api(seeded, "GET", "/api/auth/me", { cookie: S.bob })).setCookie, null);
  });

  test("expired session is unauthenticated and the cookie is cleared", async () => {
    const r = await api(seeded, "GET", "/api/auth/me", { cookie: S.expired });
    assertError(r, 401, "auth.unauthenticated");
    assert.equal(r.setCookie, CLEAR);
  });

  test("disabled user: session does not resolve; right password answers like a wrong one", async () => {
    assertError(await api(seeded, "GET", "/api/auth/me", { cookie: S.dora }), 401, "auth.unauthenticated");
    const ip = "192.0.2.30";
    const right = await api(seeded, "POST", "/api/auth/login", {
      body: { login: "dora", password: await stretch(SITE_B, "dora", "dora pass") },
      ip,
    });
    const unknown = await api(seeded, "POST", "/api/auth/login", { body: { login: "zed", password: wrong() }, ip });
    assertError(right, 401, "auth.invalid_credentials");
    assert.equal(right.text, unknown.text);
  });

  test("writer is forbidden on owner-only; owner passes", async () => {
    const w = await api(seeded, "GET", "/api/auth/me", { cookie: S.wendy });
    assert.equal(w.json.user.role, "writer");
    assertError(await api(seeded, "GET", "/api/__probe/owner", { cookie: S.wendy }), 403, "auth.forbidden");
    assert.equal((await api(seeded, "GET", "/api/__probe/owner", { cookie: S.bob })).status, 204);
  });

  test("malformed or doubled session cookie is unauthenticated, never guessed", async () => {
    for (const cookie of ["short", `${S.bob}; ${COOKIE}=${S.wendy}`]) {
      const r = await api(seeded, "GET", "/api/auth/me", { cookie });
      assertError(r, 401, "auth.unauthenticated");
      assert.equal(r.setCookie, CLEAR);
    }
  });

  test("invite accept: role comes from the invite; used and expired invites refused", async () => {
    const body = {
      code: INVITE_OWNER,
      login: "ivy",
      slug: "ivy",
      display_name: "Ivy",
      password: await stretch(SITE_B, "ivy", "ivy pass"),
    };
    const r = await api(seeded, "POST", "/api/auth/invite/accept", { body });
    assert.equal(r.status, 201, r.text);
    assert.equal(r.json.user.role, "owner");
    assert.match(r.setCookie, freshCookie);
    assertError(
      await api(seeded, "POST", "/api/auth/invite/accept", { body: { ...body, login: "ivy2", slug: "ivy2" } }),
      410,
      "invite.invalid",
    );
    assertError(
      await api(seeded, "POST", "/api/auth/invite/accept", {
        body: { ...body, code: INVITE_EXPIRED, login: "late", slug: "late" },
      }),
      410,
      "invite.invalid",
    );
    const signIn = await api(seeded, "POST", "/api/auth/login", { body: { login: "IVY", password: body.password }, ip: "192.0.2.40" });
    assert.equal(signIn.status, 200, signIn.text);
  });

  test("invite accept: wrong codes count per address; Turnstile after 5, then the code is checked", async () => {
    const ip = "192.0.2.50";
    const body = { login: "guess", slug: "guess", display_name: "G", password: await stretch(SITE_B, "guess", "pw") };
    for (let i = 0; i < 5; i++) {
      const r = await api(seeded, "POST", "/api/auth/invite/accept", { body: { ...body, code: `guess-${i}` }, ip });
      assertError(r, 410, "invite.invalid");
    }
    assertError(await api(seeded, "POST", "/api/auth/invite/accept", { body: { ...body, code: "guess-5" }, ip }), 403, "auth.turnstile_required");
    assertError(
      await api(seeded, "POST", "/api/auth/invite/accept", { body: { ...body, code: "guess-6", turnstile_token: "pass" }, ip }),
      410,
      "invite.invalid",
    );
    // Another address is not affected; shape errors are not counted.
    assertError(await api(seeded, "POST", "/api/auth/invite/accept", { body: { ...body, code: "x" }, ip: "192.0.2.51" }), 410, "invite.invalid");
    assert.equal(seeded.query("SELECT COUNT(*) AS n FROM users WHERE login = 'guess'")[0].n, 0);
  });
});

// --- blank site: site id creation, expired setup code, WebCrypto -------------------------

describe("blank site", () => {
  const crypto = (body) =>
    fetch(`${blank.url}/__crypto`, { method: "POST", body: JSON.stringify(body) }).then(async (r) => {
      const t = await r.text();
      assert.equal(r.status, 200, t);
      return t;
    });

  test("params create the site id once", async () => {
    const a = await api(blank, "GET", "/api/auth/params");
    const b = await api(blank, "GET", "/api/auth/params");
    assert.match(a.json.site_id, /^[A-Za-z0-9_-]{22}$/);
    assert.equal(b.json.site_id, a.json.site_id);
    assert.equal(blank.query("SELECT COUNT(*) AS n FROM settings WHERE key = 'site_id'")[0].n, 1);
  });

  test("expired setup code is refused", async () => {
    const body = { ...OWNER, code: SETUP_CODE, password: wrong() };
    assertError(await api(blank, "POST", "/api/auth/setup", { body }), 403, "setup.invalid");
    assert.deepEqual(blank.query("SELECT COUNT(*) AS n FROM users"), [{ n: 0 }]);
  });

  test("ended rate-limit rows are swept lazily, a bounded batch at a time; open ones stay", async () => {
    // A first failure from a new address starts its window and sweeps ended rows.
    const body = { ...OWNER, code: SETUP_CODE, password: wrong() };
    assertError(await api(blank, "POST", "/api/auth/setup", { body, ip: "198.51.100.200" }), 403, "setup.invalid");
    const keys = blank.query("SELECT key_hash FROM login_failures ORDER BY key_hash").map((r) => r.key_hash);
    assert.ok(!keys.includes("ended-1") && !keys.includes("ended-2"), keys.join());
    assert.ok(keys.includes("open-1"));
  });

  test("Rust stretch in the Worker equals stretch.ts on a fixed Unicode vector", async () => {
    const v = { site_id: "s1te-ID", login: "Tr\u1ea6N Alice", password: "correct horse \u00e9" };
    const rust = await crypto({ op: "stretch", ...v });
    assert.equal(rust, await stretchJs(v.site_id, v.login, v.password));
  });

  test("WebCrypto PBKDF2, SHA-256 and RNG match node:crypto", async () => {
    const pw = randomBytes(32);
    const salt = randomBytes(16);
    const got = await crypto({ op: "pbkdf2", password: b64u(pw), salt: b64u(salt), iterations: "1000" });
    assert.equal(got, b64u(pbkdf2Sync(pw, salt, 1000, 32, "sha256")));
    const data = randomBytes(100);
    assert.equal(await crypto({ op: "sha256", data: b64u(data) }), b64u(createHash("sha256").update(data).digest()));
    const rnd = Buffer.from(await crypto({ op: "random" }), "base64url");
    assert.equal(rnd.length, 70000, "more than one getRandomValues chunk");
    assert.ok(rnd.subarray(65536).some((b) => b !== 0), "second chunk filled");
  });

  test("ES256, RS256 and Ed25519 verify real signatures and refuse tampered ones", async () => {
    const msg = randomBytes(64);
    const bad = Buffer.from(msg);
    bad[0] ^= 1;
    const ec = generateKeyPairSync("ec", { namedCurve: "P-256" });
    const ecSig = createSign("sha256").update(msg).sign({ key: ec.privateKey, dsaEncoding: "ieee-p1363" });
    const point = ec.publicKey.export({ format: "jwk" });
    const raw = Buffer.concat([Buffer.from([4]), Buffer.from(point.x, "base64url"), Buffer.from(point.y, "base64url")]);
    const es = (m) => crypto({ op: "es256", key: b64u(raw), msg: b64u(m), sig: b64u(ecSig) });
    assert.equal(await es(msg), "true");
    assert.equal(await es(bad), "false");

    const rsa = generateKeyPairSync("rsa", { modulusLength: 2048 });
    const rsSig = createSign("sha256").update(msg).sign(rsa.privateKey);
    const jwk = rsa.publicKey.export({ format: "jwk" });
    const rs = (m) => crypto({ op: "rs256", n: jwk.n, e: jwk.e, msg: b64u(m), sig: b64u(rsSig) });
    assert.equal(await rs(msg), "true");
    assert.equal(await rs(bad), "false");

    const ed = generateKeyPairSync("ed25519");
    const edSig = edSign(null, msg, ed.privateKey);
    const edRaw = Buffer.from(ed.publicKey.export({ format: "jwk" }).x, "base64url");
    const e = (m) => crypto({ op: "ed25519", key: b64u(edRaw), msg: b64u(m), sig: b64u(edSig) });
    assert.equal(await e(msg), "true");
    assert.equal(await e(bad), "false");
  });

  test("server-side PBKDF2 (1 000 iterations) cost in workerd", async () => {
    const N = 200;
    const time = async (n) => {
      const t0 = performance.now();
      await crypto({ op: "pbkdf2_repeat", n: String(n) });
      return performance.now() - t0;
    };
    await time(0);
    await time(N);
    const base = [];
    const load = [];
    for (let i = 0; i < 3; i++) {
      base.push(await time(0));
      load.push(await time(N));
    }
    const per = (Math.min(...load) - Math.min(...base)) / N;
    console.log(`server PBKDF2-SHA256 x1000 in wrangler dev: ${per.toFixed(3)} ms per hash (best of 3, ${N} hashes per request)`);
    // The 10 ms free-plan CPU budget holds a sign-in only if one hash is far below it. Wall time
    // on a shared runner is a poor clock, so this is a backstop about 50x above normal
    // (0.1 ms), not the budget. The cost itself is a count: workerd runs PBKDF2 natively, so
    // it is SERVER_ITERATIONS (1,000) times a fixed step, and a stored hash may not ask for
    // more than MAX_SERVER_ITERATIONS (crates/auth password tests: a stored 10001 is refused before any hashing).
    assert.ok(per < 5, `${per} ms per server-side hash`);
  });
});
