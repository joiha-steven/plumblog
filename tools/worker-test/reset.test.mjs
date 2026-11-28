// Owner reset (`blog admin reset`, then POST /api/auth/reset) over HTTP against the real
// Worker and local D1. The reset itself is the SQL crates/cli/src/admin.rs sends (its CLI tests
// run it against SQLite); here it runs on the live database, then the code is redeemed.
import assert from "node:assert/strict";
import { createHash, pbkdf2, pbkdf2Sync, randomBytes } from "node:crypto";
import { after, before, describe, test } from "node:test";
import { promisify } from "node:util";

import { startWorker } from "./harness.mjs";

const ITER = 600000;
const COOKIE = "__Host-plumblog_session";
const DAY = 86_400_000;
const HOUR = 3_600_000;
const SITE = "site-reset-1";
const T = Date.now();
const sha256hex = (s) => createHash("sha256").update(s).digest("hex");
const b64u = (buf) => Buffer.from(buf).toString("base64url");
const sq = (s) => `'${String(s).replaceAll("'", "''")}'`;
const stretch = (login, password) =>
  promisify(pbkdf2)(password, `plumblog:${SITE}:${login.toLowerCase()}`, ITER, 32, "sha256").then(b64u);
function serverHash(stretched) {
  const salt = randomBytes(16);
  const h = pbkdf2Sync(Buffer.from(stretched, "base64url"), salt, 1000, 32, "sha256");
  return `p1$1000$${b64u(salt)}$${b64u(h)}`;
}

async function api(w, method, path, { body, cookie, ip = "203.0.113.9" } = {}) {
  const h = { "cf-connecting-ip": ip, origin: w.url };
  if (cookie) h.cookie = `${COOKIE}=${cookie}`;
  if (body !== undefined) h["content-type"] = "application/json";
  const res = await fetch(w.url + path, { method, headers: h, body: body === undefined ? undefined : JSON.stringify(body) });
  const text = await res.text();
  const json = res.headers.get("content-type")?.includes("json") && text ? JSON.parse(text) : null;
  return { status: res.status, text, json, setCookie: res.headers.get("set-cookie") };
}
const tokenOf = (c) => c?.match(/^__Host-plumblog_session=([A-Za-z0-9_-]*);/)?.[1];
const assertError = (r, status, code) => {
  assert.equal(r.status, status, r.text);
  assert.equal(r.json.error, code, r.text);
};

/** What `blog admin reset` writes, in its order (crates/cli/src/admin.rs `run`). */
function resetSql(user, code, expiresAt) {
  const now = Date.now();
  return `UPDATE users SET password_hash = NULL, updated_at = ${now} WHERE id = ${user};
    DELETE FROM passkeys WHERE user_id = ${user};
    DELETE FROM sessions WHERE user_id = ${user};
    UPDATE api_tokens SET revoked_at = ${now} WHERE user_id = ${user} AND revoked_at IS NULL;
    INSERT INTO settings (key, value_json, updated_at) VALUES ('owner_reset_hash', ${sq(JSON.stringify(sha256hex(code)))}, ${now})
      ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at;
    INSERT INTO settings (key, value_json, updated_at) VALUES ('owner_reset_user', '${user}', ${now})
      ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at;
    INSERT INTO settings (key, value_json, updated_at) VALUES ('owner_reset_expires_at', '${expiresAt}', ${now})
      ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json, updated_at = excluded.updated_at;`;
}

const OLD_SESSION = b64u(randomBytes(32));
let w;

before(async () => {
  const [bob, wendy] = await Promise.all([stretch("Bob", "old bob pass"), stretch("wendy", "wendy pass")]);
  w = await startWorker({
    seedSql: `INSERT INTO settings (key, value_json, updated_at) VALUES ('site_id', ${sq(JSON.stringify(SITE))}, ${T});
      INSERT INTO users (id, login, slug, display_name, role, password_hash, created_at, updated_at) VALUES
        (1, 'Bob', 'bob', 'Bob', 'owner', ${sq(serverHash(bob))}, ${T}, ${T}),
        (2, 'wendy', 'wendy', 'Wendy', 'writer', ${sq(serverHash(wendy))}, ${T}, ${T});
      INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES (${sq(sha256hex(OLD_SESSION))}, 1, ${T}, ${T + DAY});`,
  });
});

after(async () => {
  await w?.stop();
});

const failures = () => w.query("SELECT COALESCE(SUM(count), 0) AS n FROM login_failures WHERE key_hash <> 'global'")[0].n;
const resetKeys = () => w.query("SELECT key FROM settings WHERE key LIKE 'owner_reset%' ORDER BY key").map((r) => r.key);

describe("owner reset", () => {
  test("before the reset the old password and session work", async () => {
    const r = await api(w, "POST", "/api/auth/login", { body: { login: "Bob", password: await stretch("Bob", "old bob pass") }, ip: "203.0.113.1" });
    assert.equal(r.status, 200, r.text);
    assert.equal((await api(w, "GET", "/api/auth/me", { cookie: OLD_SESSION })).status, 200);
  });

  test("after the reset: old password, old session refused; wrong and expired codes refused and counted", async () => {
    w.exec(resetSql(1, "expired-code", T - 1));
    assertError(await api(w, "POST", "/api/auth/reset", { body: { code: "expired-code", login: "Bob", password: await stretch("Bob", "x pass") }, ip: "203.0.113.2" }), 403, "setup.invalid");
    assert.deepEqual(resetKeys(), [], "an expired code is deleted when tried");

    w.exec(resetSql(1, "good-code-1", Date.now() + HOUR));
    assertError(await api(w, "POST", "/api/auth/login", { body: { login: "Bob", password: await stretch("Bob", "old bob pass") }, ip: "203.0.113.3" }), 401, "auth.invalid_credentials");
    assertError(await api(w, "GET", "/api/auth/me", { cookie: OLD_SESSION }), 401, "auth.unauthenticated");

    const before = failures();
    assertError(await api(w, "POST", "/api/auth/reset", { body: { code: "wrong-code", login: "Bob", password: await stretch("Bob", "x pass") }, ip: "203.0.113.4" }), 403, "setup.invalid");
    assert.equal(failures(), before + 1, "a wrong code is counted by the limiter");
    assert.deepEqual(resetKeys(), ["owner_reset_expires_at", "owner_reset_hash", "owner_reset_user"], "a wrong code keeps the right one");
  });

  test("reset keys never leave the Worker", async () => {
    for (const path of ["/api/auth/params", "/", "/llms.txt"]) {
      const r = await api(w, "GET", path);
      assert.ok(!r.text.includes("owner_reset"), path);
      assert.ok(!r.text.includes(sha256hex("good-code-1")), path);
    }
  });

  test("the code only works for its own user: a writer's login is refused before the code is spent", async () => {
    const r = await api(w, "POST", "/api/auth/reset", { body: { code: "good-code-1", login: "wendy", password: await stretch("wendy", "stolen pass") }, ip: "203.0.113.5" });
    assertError(r, 409, "store.conflict");
    assert.equal(r.json.field, "login");
    assert.equal(resetKeys().length, 3, "the code is still there");
    const wendy = await api(w, "POST", "/api/auth/login", { body: { login: "wendy", password: await stretch("wendy", "wendy pass") }, ip: "203.0.113.5" });
    assert.equal(wendy.status, 200, "the writer's password is untouched");
  });

  test("redeem: new password and login, signed in, logged, code gone and single use", async () => {
    const r = await api(w, "POST", "/api/auth/reset", { body: { code: "good-code-1", login: "Bobby", password: await stretch("Bobby", "new bob pass") }, ip: "203.0.113.6" });
    assert.equal(r.status, 200, r.text);
    assert.equal(r.json.user.id, 1);
    assert.equal(r.json.user.login, "Bobby");
    assert.equal(r.json.user.role, "owner");
    const cookie = tokenOf(r.setCookie);
    assert.ok(cookie);
    assert.equal((await api(w, "GET", "/api/auth/me", { cookie })).status, 200);
    assert.deepEqual(resetKeys(), []);
    const log = w.query("SELECT actor_id, detail FROM system_log WHERE kind = 'auth.owner_reset_redeemed'");
    assert.equal(log.length, 1);
    assert.equal(log[0].actor_id, 1);
    assert.ok(!log[0].detail.includes("good-code-1"));

    const signIn = await api(w, "POST", "/api/auth/login", { body: { login: "Bobby", password: await stretch("Bobby", "new bob pass") }, ip: "203.0.113.7" });
    assert.equal(signIn.status, 200, signIn.text);
    assertError(await api(w, "GET", "/api/auth/me", { cookie: OLD_SESSION }), 401, "auth.unauthenticated");
    assertError(await api(w, "POST", "/api/auth/reset", { body: { code: "good-code-1", login: "Bobby", password: await stretch("Bobby", "again pass") }, ip: "203.0.113.8" }), 403, "setup.invalid");

    // Settings over the API, as the owner: none of the reset keys, and none writable.
    w.exec(resetSql(1, "good-code-2", Date.now() + HOUR));
    const fresh = tokenOf((await api(w, "POST", "/api/auth/reset", { body: { code: "good-code-2", login: "Bobby", password: await stretch("Bobby", "new bob pass") }, ip: "203.0.113.10" })).setCookie);
    w.exec(resetSql(1, "good-code-3", Date.now() + HOUR));
    const s = await api(w, "GET", "/api/v1/settings", { cookie: fresh });
    assert.equal(s.status, 401, "the reset ended that session too");
    const again = await api(w, "POST", "/api/auth/reset", { body: { code: "good-code-3", login: "Bobby", password: await stretch("Bobby", "new bob pass") }, ip: "203.0.113.11" });
    const owner = tokenOf(again.setCookie);
    w.exec(resetSql(1, "good-code-4", Date.now() + HOUR).split("\n").slice(4).join("\n"));
    const list = await api(w, "GET", "/api/v1/settings", { cookie: owner });
    assert.equal(list.status, 200, list.text);
    assert.ok(!list.text.includes("owner_reset"), list.text);
    for (const k of ["owner_reset_hash", "owner_reset_user", "owner_reset_expires_at"]) {
      assertError(await api(w, "PUT", `/api/v1/settings/${k}`, { cookie: owner, body: { value: "x" } }), 400, "input.invalid");
    }
    assert.equal(resetKeys().length, 3, "untouched by the refused writes");
  });
});
