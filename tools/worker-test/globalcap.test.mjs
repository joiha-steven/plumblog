// The site-wide sign-in cap (crates/auth ratelimit::GLOBAL_CAP), lowered for tests through
// PLUMBLOG_TEST_GLOBAL_CAP. Two Workers: Turnstile off (refuse with no write) and Turnstile on
// (a valid token lets the attempt through). Every attempt comes from its own address, like a
// flood from a /48, so only the site-wide key can stop it.
import assert from "node:assert/strict";
import { createHash, pbkdf2, pbkdf2Sync, randomBytes } from "node:crypto";
import { createServer } from "node:http";
import { after, before, describe, test } from "node:test";
import { promisify } from "node:util";

import { startWorker } from "./harness.mjs";

const CAP = 3;
const COOKIE = "__Host-plumblog_session";
const DAY = 86_400_000;
const T = Date.now();
const SITE = "site-global-cap";
const TS_SITE_KEY = "1x00000000000000000000AA";
const b64u = (buf) => Buffer.from(buf).toString("base64url");
const sha256hex = (s) => createHash("sha256").update(s).digest("hex");
const sq = (s) => `'${String(s).replaceAll("'", "''")}'`;
const wrong = () => b64u(randomBytes(32));
const SESSION = b64u(randomBytes(32));

function serverHash(stretched) {
  const salt = randomBytes(16);
  const h = pbkdf2Sync(Buffer.from(stretched, "base64url"), salt, 1000, 32, "sha256");
  return `p1$1000$${b64u(salt)}$${b64u(h)}`;
}

let ipSeq = 0;
const nextIp = () => `2001:db8:${(++ipSeq).toString(16)}::1`;

async function api(w, method, path, { body, cookie, ip } = {}) {
  const h = { "cf-connecting-ip": ip ?? nextIp(), origin: w.url };
  if (cookie) h.cookie = `${COOKIE}=${cookie}`;
  if (body !== undefined) h["content-type"] = "application/json";
  const res = await fetch(w.url + path, { method, headers: h, body: body === undefined ? undefined : JSON.stringify(body) });
  const text = await res.text();
  return { status: res.status, text, json: text ? JSON.parse(text) : null };
}

function assertError(r, status, code) {
  assert.equal(r.status, status, r.text);
  assert.deepEqual(r.json, { error: code });
}

/** Everything a limited attempt could write. */
const snapshot = (w) => ({
  failures: w.query("SELECT key_hash, count, window_start FROM login_failures ORDER BY key_hash"),
  challenges: w.query("SELECT COUNT(*) AS n FROM auth_challenges")[0].n,
  log: w.query("SELECT COUNT(*) AS n FROM system_log")[0].n,
});
const globalCount = (w) => w.query("SELECT count FROM login_failures WHERE key_hash = 'global'")[0].count;
const capLogs = (w) => w.query("SELECT COUNT(*) AS n FROM system_log WHERE kind = 'auth.global_cap'")[0].n;

let off, on, stub, bobPw;

before(async () => {
  stub = createServer((req, res) => {
    let data = "";
    req.on("data", (d) => (data += d));
    req.on("end", () => {
      const response = new URLSearchParams(data).get("response");
      res.setHeader("content-type", "application/json");
      res.end(JSON.stringify({ success: response === "pass", hostname: "127.0.0.1", "error-codes": [] }));
    });
  });
  await new Promise((ok) => stub.listen(0, "127.0.0.1", ok));
  bobPw = await promisify(pbkdf2)("bob pass", `plumblog:${SITE}:bob`, 600000, 32, "sha256").then(b64u);
  const seed = (url, turnstile) => `
    INSERT INTO settings (key, value_json, updated_at) VALUES
      ('site_id', ${sq(JSON.stringify(SITE))}, ${T}), ('origin', ${sq(JSON.stringify(url))}, ${T})
      ${turnstile ? `, ('turnstile_site_key', ${sq(JSON.stringify(TS_SITE_KEY))}, ${T})` : ""};
    INSERT INTO users (id, login, slug, display_name, role, password_hash, disabled_at, created_at, updated_at) VALUES
      (1, 'Bob', 'bob', 'Bob', 'owner', ${sq(serverHash(bobPw))}, NULL, ${T}, ${T});
    INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES
      (${sq(sha256hex(SESSION))}, 1, ${T}, ${T + 29 * DAY});`;
  [off, on] = await Promise.all([
    startWorker({ seedSql: (url) => seed(url, false), vars: { PLUMBLOG_TEST_GLOBAL_CAP: String(CAP) } }),
    startWorker({
      seedSql: (url) => seed(url, true),
      vars: {
        PLUMBLOG_TEST_GLOBAL_CAP: String(CAP),
        TURNSTILE_SECRET: "stub-secret",
        TURNSTILE_VERIFY_URL: `http://127.0.0.1:${stub.address().port}/verify`,
      },
    }),
  ]);
});

after(async () => {
  await Promise.all([off?.stop(), on?.stop()]);
  stub?.close();
});

const login = (w, password, extra = {}) =>
  api(w, "POST", "/api/auth/login", { body: { login: "bob", password, ...extra } });

/** Re-authentication by a signed-in user: current password, or a passkey assertion. */
const changePassword = (w, current, ip) =>
  api(w, "POST", "/api/auth/password", { cookie: SESSION, ip, body: { current_password: current, new_password: wrong() } });
// Reaches the challenge only once re-authentication passed; this one is never valid.
const addPasskey = (w, reauth, ip) =>
  api(w, "POST", "/api/auth/passkey/register/verify", {
    cookie: SESSION,
    ip,
    body: { name: "k", client_data_json: b64u("{}"), attestation_object: b64u("x"), ...reauth },
  });
const unknownKey = {
  reauth_credential_id: "AAAA",
  reauth_client_data_json: b64u("{}"),
  reauth_authenticator_data: b64u("x"),
  reauth_signature: b64u("x"),
};

/** One of every limited attempt, each from a new address. */
const flood = (w, extra = {}) =>
  Promise.all([
    login(w, wrong(), extra),
    login(w, bobPw, extra),
    api(w, "POST", "/api/auth/passkey/login/options", { body: extra }),
    api(w, "POST", "/api/auth/passkey/login/verify", { body: { credential_id: "AAAA", ...extra } }),
    api(w, "POST", "/api/auth/setup", { body: { code: "guess", login: "x", slug: "x", display_name: "x", password: wrong(), ...extra } }),
    api(w, "POST", "/api/auth/invite/accept", { body: { code: "guess", login: "x", slug: "x", display_name: "x", password: wrong(), ...extra } }),
  ]);

describe("Turnstile off", () => {
  test("up to the cap: counted as usual, one log entry when it trips", async () => {
    for (let i = 0; i < CAP; i++) assertError(await login(off, wrong()), 401, "auth.invalid_credentials");
    assert.deepEqual(off.query("SELECT count FROM login_failures WHERE key_hash = 'global'"), [{ count: CAP }]);
    assert.equal(capLogs(off), 1);
  });

  test("at the cap: every limited attempt is 429 and writes nothing", async () => {
    const before = snapshot(off);
    const replies = [...(await flood(off)), ...(await flood(off)), ...(await flood(off))];
    assert.deepEqual(snapshot(off), before, "refused attempts wrote rows");
    for (const r of replies) assertError(r, 429, "auth.locked");
    assert.equal(capLogs(off), 1, "one log entry per window");
  });

  test("a signed-in session still works", async () => {
    const r = await api(off, "GET", "/api/auth/me", { cookie: SESSION });
    assert.equal(r.status, 200, r.text);
  });

  test("signed-in re-auth skips the cap and keeps its own per-address and per-login limits", async () => {
    const ip = "2001:db8:ffff::1";
    // The right password passes re-auth and fails later, at the (absent) challenge.
    assertError(await addPasskey(off, { current_password: bobPw }, ip), 400, "auth.passkey_invalid");
    assertError(await changePassword(off, wrong(), ip), 401, "auth.invalid_credentials");
    assertError(await addPasskey(off, unknownKey, ip), 400, "auth.passkey_invalid");
    for (let i = 0; i < 3; i++) assertError(await changePassword(off, wrong(), ip), 401, "auth.invalid_credentials");
    // 5 failures from this address: locked, as without the cap.
    assertError(await changePassword(off, wrong(), ip), 429, "auth.locked");
    assertError(await addPasskey(off, unknownKey, ip), 429, "auth.locked");
    // A 5th wrong current password, from another address, locks his login too.
    assertError(await changePassword(off, wrong()), 401, "auth.invalid_credentials");
    assertError(await changePassword(off, bobPw), 429, "auth.locked");
    assert.equal(globalCount(off), CAP, "signed-in attempts moved the site-wide count");
    assert.equal(capLogs(off), 1);
  });
});

describe("Turnstile on", () => {
  test("fill the cap", async () => {
    for (let i = 0; i < CAP; i++) assertError(await login(on, wrong()), 401, "auth.invalid_credentials");
    assert.equal(capLogs(on), 1);
  });

  test("at the cap: no token or a rejected one is turnstile_required, with no write", async () => {
    const before = snapshot(on);
    const replies = [...(await flood(on)), ...(await flood(on, { turnstile_token: "fail" }))];
    assert.deepEqual(snapshot(on), before, "refused attempts wrote rows");
    for (const r of replies) assertError(r, 403, "auth.turnstile_required");
  });

  test("a valid token goes through the usual limits", async () => {
    const ok = await login(on, bobPw, { turnstile_token: "pass" });
    assert.equal(ok.status, 200, ok.text);
    const before = snapshot(on);
    assertError(await login(on, wrong(), { turnstile_token: "pass" }), 401, "auth.invalid_credentials");
    assert.ok(snapshot(on).failures.length > before.failures.length, "the per-address key is counted");
    const opts = await api(on, "POST", "/api/auth/passkey/login/options", { body: { turnstile_token: "pass" } });
    assert.equal(opts.status, 200, opts.text);
    assert.equal(snapshot(on).challenges, before.challenges + 1);
    assert.equal(capLogs(on), 1);
  });

  test("a signed-in session still works", async () => {
    const r = await api(on, "GET", "/api/auth/me", { cookie: SESSION });
    assert.equal(r.status, 200, r.text);
  });

  test("signed-in re-auth needs no token at the cap", async () => {
    const before = globalCount(on);
    assertError(await addPasskey(on, { current_password: bobPw }), 400, "auth.passkey_invalid");
    assertError(await changePassword(on, wrong()), 401, "auth.invalid_credentials");
    assertError(await addPasskey(on, unknownKey), 400, "auth.passkey_invalid");
    assert.equal(globalCount(on), before);
  });
});
