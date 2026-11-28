// Owner reset (`blog admin reset`, then POST /api/auth/reset) over HTTP against the real
// Worker and local D1. The reset itself is the SQL crates/cli/src/admin.rs sends (its CLI tests
// run it against SQLite); here it runs on the live database, then the code is redeemed.
import assert from "node:assert/strict";
import { createHash, pbkdf2, pbkdf2Sync, randomBytes } from "node:crypto";
import { after, before, describe, test } from "node:test";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

import { d1Batch, startWorker } from "./harness.mjs";

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

/** What `blog admin reset` writes, in its order: the SQL crates/cli/src/admin.rs generates (its test keeps the fixture fresh). */
const RESET = JSON.parse(readFileSync(join(dirname(fileURLToPath(import.meta.url)), "fixtures/admin-reset.json"), "utf8"));
function reset(user, code, expiresAt, { keepTokens = false, codeOnly = false } = {}) {
  const values = { $user: String(user), $now: String(Date.now()), $hash_json: JSON.stringify(sha256hex(code)), $expires: String(expiresAt) };
  const fill = (p) => p.replace(/\$(user|now|hash_json|expires)/g, (k) => values[k]);
  const stmts = RESET[keepTokens ? "keep_tokens" : "revoke_tokens"]
    .filter((s) => !codeOnly || s.sql.startsWith("INSERT INTO settings"))
    .map((s) => ({ sql: s.sql, params: s.params.map(fill) }));
  d1Batch(w.state, stmts);
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
    reset(1, "expired-code", T - 1);
    assertError(await api(w, "POST", "/api/auth/reset", { body: { code: "expired-code", login: "Bob", password: await stretch("Bob", "x pass") }, ip: "203.0.113.2" }), 403, "setup.invalid");
    assert.deepEqual(resetKeys(), [], "an expired code is deleted when tried");

    reset(1, "good-code-1", Date.now() + HOUR);
    assertError(await api(w, "POST", "/api/auth/login", { body: { login: "Bob", password: await stretch("Bob", "old bob pass") }, ip: "203.0.113.3" }), 401, "auth.invalid_credentials");
    assertError(await api(w, "GET", "/api/auth/me", { cookie: OLD_SESSION }), 401, "auth.unauthenticated");

    const before = failures();
    assertError(await api(w, "POST", "/api/auth/reset", { body: { code: "wrong-code", login: "Bob", password: await stretch("Bob", "x pass") }, ip: "203.0.113.4" }), 403, "setup.invalid");
    assert.equal(failures(), before + 1, "a wrong code is counted by the limiter");
    assert.deepEqual(resetKeys(), ["owner_reset_at", "owner_reset_expires_at", "owner_reset_hash", "owner_reset_keep_tokens", "owner_reset_user"], "a wrong code keeps the right one");
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
    assert.equal(resetKeys().length, 5, "the code is still there");
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
    reset(1, "good-code-2", Date.now() + HOUR);
    const fresh = tokenOf((await api(w, "POST", "/api/auth/reset", { body: { code: "good-code-2", login: "Bobby", password: await stretch("Bobby", "new bob pass") }, ip: "203.0.113.10" })).setCookie);
    reset(1, "good-code-3", Date.now() + HOUR);
    const s = await api(w, "GET", "/api/v1/settings", { cookie: fresh });
    assert.equal(s.status, 401, "the reset ended that session too");
    const again = await api(w, "POST", "/api/auth/reset", { body: { code: "good-code-3", login: "Bobby", password: await stretch("Bobby", "new bob pass") }, ip: "203.0.113.11" });
    const owner = tokenOf(again.setCookie);
    reset(1, "good-code-4", Date.now() + HOUR, { codeOnly: true });
    const list = await api(w, "GET", "/api/v1/settings", { cookie: owner });
    assert.equal(list.status, 200, list.text);
    assert.ok(!list.text.includes("owner_reset"), list.text);
    for (const k of ["owner_reset_hash", "owner_reset_user", "owner_reset_expires_at", "owner_reset_at", "owner_reset_keep_tokens"]) {
      assertError(await api(w, "PUT", `/api/v1/settings/${k}`, { cookie: owner, body: { value: "x" } }), 400, "input.invalid");
    }
    assert.equal(resetKeys().length, 5, "untouched by the refused writes");
  });

  test("the redeem removes what a request in flight across the reset left: passkeys, tokens and owner invites since", async () => {
    reset(1, "flight-code", Date.now() + HOUR, { keepTokens: true });
    const at = Number(w.query("SELECT value_json AS v FROM settings WHERE key = 'owner_reset_at'")[0].v);
    // Written after the reset by requests that authenticated before it (what the Worker now refuses).
    // 'kept' predates the reset by more than OWNER_RESET_SINCE_MARGIN_MS (10 min), so --keep-tokens keeps it.
    w.exec(`INSERT INTO passkeys (user_id, credential_id, public_key, created_at) VALUES (1, 'late-cred', 'k', ${at + 1});
      INSERT INTO api_tokens (user_id, name, token_hash, scope, created_at) VALUES (1, 'kept', 'kept-hash', 'owner', ${at - 11 * 60 * 1000}), (1, 'late', 'late-hash', 'owner', ${at + 1});
      INSERT INTO invites (code_hash, role, created_by, created_at, expires_at) VALUES ('late-invite', 'owner', 1, ${at + 1}, ${at + DAY});`);
    const r = await api(w, "POST", "/api/auth/reset", { body: { code: "flight-code", login: "Bobby", password: await stretch("Bobby", "new bob pass") }, ip: "203.0.113.20" });
    assert.equal(r.status, 200, r.text);
    assert.equal(w.query("SELECT count(*) AS n FROM passkeys WHERE user_id = 1")[0].n, 0);
    const tokens = w.query("SELECT token_hash, revoked_at IS NOT NULL AS revoked FROM api_tokens WHERE token_hash IN ('kept-hash', 'late-hash') ORDER BY token_hash");
    assert.deepEqual(tokens, [{ token_hash: "kept-hash", revoked: 0 }, { token_hash: "late-hash", revoked: 1 }], "--keep-tokens keeps only tokens from before the reset");
    assert.equal(w.query("SELECT count(*) AS n FROM invites WHERE code_hash = 'late-invite'")[0].n, 0);
    assert.deepEqual(resetKeys(), []);
  });

  test("a code whose owner was demoted or disabled since the reset gives nothing, changes nothing, and is spent", async () => {
    w.exec(`INSERT INTO users (id, login, slug, display_name, role, created_at, updated_at) VALUES (3, 'root', 'root', 'Root', 'owner', ${T}, ${T})`);
    for (const [what, change, undo] of [
      ["demoted", "UPDATE users SET role = 'writer' WHERE id = 1", "UPDATE users SET role = 'owner' WHERE id = 1"],
      ["disabled", `UPDATE users SET disabled_at = ${T} WHERE id = 1`, "UPDATE users SET disabled_at = NULL WHERE id = 1"],
    ]) {
      reset(1, `${what}-code`, Date.now() + HOUR);
      w.exec(change);
      const r = await api(w, "POST", "/api/auth/reset", { body: { code: `${what}-code`, login: "Bobby", password: await stretch("Bobby", "taken over") }, ip: "203.0.113.21" });
      assertError(r, 403, "setup.invalid");
      assert.equal(r.setCookie, null, what);
      assert.equal(w.query("SELECT password_hash FROM users WHERE id = 1")[0].password_hash, null, `${what}: password untouched`);
      assert.equal(w.query("SELECT count(*) AS n FROM sessions WHERE user_id = 1")[0].n, 0, `${what}: no session`);
      assert.deepEqual(resetKeys(), [], `${what}: the code is spent`);
      w.exec(undo);
    }
  });
});
