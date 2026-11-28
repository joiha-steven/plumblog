// API tokens over HTTP against the real Worker (issue #10): management routes (session only),
// Bearer resolution, scopes capped by the user's current role, and the gate's boundaries
// (Origin skipped for Bearer, cookie ignored, /api/auth/* refused to tokens).
// Permissions are checked on the test-only probes (/api/__probe/read|writer|owner).
import assert from "node:assert/strict";
import { createHash, randomBytes } from "node:crypto";
import { after, before, describe, test } from "node:test";

import { startWorker } from "./harness.mjs";

const COOKIE = "__Host-plumblog_session";
const DAY = 86_400_000;
const T = Date.now();
const sha256hex = (s) => createHash("sha256").update(s).digest("hex");
const b64u = (buf) => Buffer.from(buf).toString("base64url");
const sq = (s) => `'${String(s).replaceAll("'", "''")}'`;
const newSecret = () => `plb_${b64u(randomBytes(32))}`;
const TOKEN_RE = /^plb_[A-Za-z0-9_-]{43}$/;

/** One API call. `origin: null` omits the header (default: the Worker's own origin). */
async function api(w, method, path, { body, cookie, bearer, origin, headers = {} } = {}) {
  const h = { ...headers };
  if (origin !== null) h.origin = origin ?? w.url;
  if (cookie) h.cookie = `${COOKIE}=${cookie}`;
  if (bearer) h.authorization = `Bearer ${bearer}`;
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

// Users: 1 owner, 2 writer, 3 owner (demoted during a test), 4 disabled writer.
const S = { owner: b64u(randomBytes(32)), writer: b64u(randomBytes(32)), renew: b64u(randomBytes(32)) };
const SEEDED = { demoted: newSecret(), disabled: newSecret(), expired: newSecret(), revoked: newSecret() };

let w;
before(async () => {
  const seed = [
    `INSERT INTO users (id, login, slug, display_name, role, password_hash, disabled_at, created_at, updated_at) VALUES
      (1, 'olga', 'olga', 'Olga', 'owner', NULL, NULL, ${T}, ${T}),
      (2, 'will', 'will', 'Will', 'writer', NULL, NULL, ${T}, ${T}),
      (3, 'dana', 'dana', 'Dana', 'owner', NULL, NULL, ${T}, ${T}),
      (4, 'dis', 'dis', 'Dis', 'writer', NULL, ${T - 1000}, ${T}, ${T});`,
    // `renew` is the writer's session past half its life: a cookie request would renew it.
    `INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES
      (${sq(sha256hex(S.owner))}, 1, ${T}, ${T + 29 * DAY}),
      (${sq(sha256hex(S.writer))}, 2, ${T}, ${T + 29 * DAY}),
      (${sq(sha256hex(S.renew))}, 2, ${T - 20 * DAY}, ${T + 10 * DAY});`,
    `INSERT INTO api_tokens (user_id, name, token_hash, scope, created_at, expires_at, revoked_at) VALUES
      (3, 'dana owner', ${sq(sha256hex(SEEDED.demoted))}, 'owner', ${T}, NULL, NULL),
      (4, 'disabled', ${sq(sha256hex(SEEDED.disabled))}, 'writer', ${T}, NULL, NULL),
      (1, 'expired', ${sq(sha256hex(SEEDED.expired))}, 'owner', ${T - 2 * DAY}, ${T - DAY}, NULL),
      (1, 'revoked', ${sq(sha256hex(SEEDED.revoked))}, 'owner', ${T - 2 * DAY}, NULL, ${T - DAY});`,
  ].join("\n");
  w = await startWorker({ seedSql: seed });
});

after(async () => {
  await w?.stop();
});

const create = (cookie, body, opts = {}) => api(w, "POST", "/api/auth/tokens", { cookie, body, ...opts });
const probes = async (bearer, opts = {}) => ({
  read: (await api(w, "GET", "/api/__probe/read", { bearer, ...opts })).status,
  writer: (await api(w, "POST", "/api/__probe/writer", { bearer, ...opts })).status,
  owner: (await api(w, "GET", "/api/__probe/owner", { bearer, ...opts })).status,
});

describe("tokens", () => {
  const made = {}; // name -> create answer
  const secrets = [];

  test("create: 201, the token shown once, its hash stored, metadata back", async () => {
    for (const [cookie, name, scope] of [
      [S.owner, "CI read", "read"],
      [S.owner, "CI writer", "writer"],
      [S.owner, "CI owner", "owner"],
      [S.writer, "Will read", "read"],
      [S.writer, "Will writer", "writer"],
    ]) {
      const r = await create(cookie, { name, scope });
      assert.equal(r.status, 201, r.text);
      assert.match(r.json.token, TOKEN_RE);
      assert.deepEqual(Object.keys(r.json), ["id", "name", "scope", "created_at", "expires_at", "revoked_at", "token"]);
      assert.equal(r.json.name, name);
      assert.equal(r.json.scope, scope);
      assert.equal(r.json.expires_at, null);
      assert.equal(r.json.revoked_at, null);
      assert.equal(r.setCookie, null);
      made[name] = r.json;
      secrets.push(r.json.token);
      const rows = w.query(`SELECT name, scope, token_hash FROM api_tokens WHERE id = ${r.json.id}`);
      assert.deepEqual(rows, [{ name, scope, token_hash: sha256hex(r.json.token) }]);
    }
    const until = T + 30 * DAY;
    const r = await create(S.owner, { name: "Claude \u0111\u1ecdc", scope: "read", expires_at: String(until) });
    assert.equal(r.status, 201, r.text);
    assert.equal(r.json.expires_at, until);
    secrets.push(r.json.token);
  });

  test("create: two at once are two different tokens", async () => {
    const [a, b] = await Promise.all([
      create(S.owner, { name: "twin", scope: "read" }),
      create(S.owner, { name: "twin", scope: "read" }),
    ]);
    assert.equal(a.status, 201, a.text);
    assert.equal(b.status, 201, b.text);
    assert.notEqual(a.json.token, b.json.token);
    assert.notEqual(a.json.id, b.json.id);
    secrets.push(a.json.token, b.json.token);
  });

  test("create: bad input 400, scope above the role 403, anonymous 401", async () => {
    for (const body of [
      {},
      { name: "x" },
      { scope: "read" },
      { name: "", scope: "read" },
      { name: "   ", scope: "read" },
      { name: "x".repeat(65), scope: "read" },
      { name: "a\nb", scope: "read" },
      { name: "a\u0000b", scope: "read" },
      { name: "x", scope: "admin" },
      { name: "x", scope: "Owner" },
      { name: "x", scope: "read", expires_at: String(T - 1) },
      { name: "x", scope: "read", expires_at: "tomorrow" },
      { name: "x", scope: "read", expires_at: "-1" },
      { name: "x", scope: "read", expires_at: "99999999999999999999" },
      { name: "x", scope: "read", user_id: "1" },
      `{"name":"x","scope":"read","expires_at":${T + DAY}}`,
    ]) {
      assertError(await create(S.owner, body), 400, "input.invalid");
    }
    assertError(await create(S.writer, { name: "x", scope: "owner" }), 403, "auth.forbidden");
    assertError(await create(null, { name: "x", scope: "read" }), 401, "auth.unauthenticated");
    // Nothing was stored by any refused attempt.
    assert.equal(w.query("SELECT COUNT(*) AS n FROM api_tokens WHERE name IN ('x', '')")[0].n, 0);
  });

  test("list: own tokens only, never the token or its hash", async () => {
    const r = await api(w, "GET", "/api/auth/tokens", { cookie: S.writer });
    assert.equal(r.status, 200, r.text);
    assert.deepEqual(r.json.tokens.map((t) => t.name), ["Will read", "Will writer"]);
    for (const t of r.json.tokens) {
      assert.deepEqual(Object.keys(t), ["id", "name", "scope", "created_at", "expires_at", "revoked_at"]);
    }
    const all = await api(w, "GET", "/api/auth/tokens", { cookie: S.owner });
    assert.equal(all.status, 200);
    assert.ok(all.json.tokens.some((t) => t.name === "expired") && all.json.tokens.some((t) => t.name === "revoked"));
    assert.ok(!all.json.tokens.some((t) => t.name.startsWith("Will")));
    for (const s of [...secrets, ...Object.values(SEEDED)]) {
      for (const text of [r.text, all.text]) {
        assert.ok(!text.includes(s) && !text.includes(sha256hex(s)), "secret or hash in a list");
      }
    }
    assertError(await api(w, "GET", "/api/auth/tokens"), 401, "auth.unauthenticated");
  });

  test("Bearer: read < writer < owner on the probes; scope never above the role", async () => {
    assert.deepEqual(await probes(made["CI read"].token), { read: 200, writer: 403, owner: 403 });
    assert.deepEqual(await probes(made["CI writer"].token), { read: 200, writer: 204, owner: 403 });
    assert.deepEqual(await probes(made["CI owner"].token), { read: 200, writer: 204, owner: 204 });
    assert.deepEqual(await probes(made["Will read"].token), { read: 200, writer: 403, owner: 403 });
    assert.deepEqual(await probes(made["Will writer"].token), { read: 200, writer: 204, owner: 403 });
    const r = await api(w, "GET", "/api/__probe/read", { bearer: made["Will read"].token });
    assert.deepEqual(r.json, { user: 2 });
    assert.equal(r.setCookie, null);
  });

  test("writes say how they came: via token (name and id) or session", async () => {
    const t = made["CI writer"];
    assert.equal((await api(w, "POST", "/api/__probe/writer", { bearer: t.token })).status, 204);
    assert.equal((await api(w, "POST", "/api/__probe/writer", { cookie: S.owner })).status, 204);
    const rows = w.query("SELECT actor_id, detail FROM system_log WHERE kind = 'content.changed' ORDER BY id");
    assert.ok(rows.some((r) => r.actor_id === 1 && r.detail.includes("via\ttoken") && r.detail.includes(`client\tCI writer #${t.id}`)), JSON.stringify(rows));
    assert.ok(rows.some((r) => r.actor_id === 1 && r.detail.includes("via\tsession")), JSON.stringify(rows));
  });

  test("a demoted owner's owner token acts as writer", async () => {
    assert.deepEqual(await probes(SEEDED.demoted), { read: 200, writer: 204, owner: 204 });
    w.exec("UPDATE users SET role = 'writer' WHERE id = 3");
    assert.deepEqual(await probes(SEEDED.demoted), { read: 200, writer: 204, owner: 403 });
  });

  test("disabled user, expired, revoked, unknown: 401", async () => {
    for (const bearer of [SEEDED.disabled, SEEDED.expired, SEEDED.revoked, newSecret()]) {
      for (const [method, path] of [["GET", "/api/__probe/read"], ["POST", "/api/__probe/writer"]]) {
        const r = await api(w, method, path, { bearer });
        assertError(r, 401, "auth.unauthenticated");
        assert.equal(r.setCookie, null);
      }
    }
  });

  test("malformed Authorization: 401, never a server error, never the cookie instead", async () => {
    const good = made["CI owner"].token;
    const body = good.slice(4);
    // (An empty header never reaches the Worker: it is the same as none.)
    for (const value of [
      "Bearer",
      "Bearer x",
      good,
      `Basic ${good}`,
      `Bearer  ${good}`,
      `Bearer ${good}=`,
      `Bearer ${good}, Bearer ${good}`,
      `Bearer PLB_${body}`,
      `Bearer plb-${body}`,
      `Bearer ${body}`,
      `Bearer plb_${body.slice(1)}`,
      `Bearer plb_${body}A`,
      `Bearer plb_+${body.slice(1)}`,
      `Bearer plb_${"\u00e9".repeat(43)}`,
    ]) {
      const r = await api(w, "GET", "/api/__probe/read", { headers: { authorization: value }, cookie: S.owner });
      assert.equal(r.status, 401, `${JSON.stringify(value)}: ${r.text}`);
      assertError(r, 401, "auth.unauthenticated");
      assert.equal(r.setCookie, null, value);
    }
    // Two headers arrive joined, which is malformed too.
    const h = new Headers();
    h.append("authorization", `Bearer ${good}`);
    h.append("authorization", `Bearer ${good}`);
    const r = await fetch(`${w.url}/api/__probe/read`, { headers: h });
    assert.equal(r.status, 401);
    // The scheme is case-insensitive, as HTTP says.
    const lower = await api(w, "GET", "/api/__probe/read", { headers: { authorization: `bearer ${good}` } });
    assert.equal(lower.status, 200, lower.text);
  });

  test("Bearer skips the Origin check; the session does not", async () => {
    const bearer = made["CI writer"].token;
    assert.equal((await api(w, "POST", "/api/__probe/writer", { bearer, origin: null })).status, 204);
    assert.equal((await api(w, "POST", "/api/__probe/writer", { bearer, origin: "https://evil.example" })).status, 204);
    assertError(await api(w, "POST", "/api/__probe/writer", { cookie: S.owner, origin: "https://evil.example" }), 403, "auth.forbidden");
    assertError(await api(w, "POST", "/api/__probe/writer", { cookie: S.owner, origin: null }), 403, "auth.forbidden");
    assertError(await create(S.owner, { name: "x", scope: "read" }, { origin: "https://evil.example" }), 403, "auth.forbidden");
    assertError(await create(S.owner, { name: "x", scope: "read" }, { origin: null }), 403, "auth.forbidden");
  });

  test("Bearer + cookie: the token wins; the cookie is not read, renewed or cleared", async () => {
    const bearer = made["Will read"].token;
    for (const cookie of ["garbage", b64u(randomBytes(32)), S.owner, S.renew]) {
      const r = await api(w, "GET", "/api/__probe/read", { bearer, cookie });
      assert.equal(r.status, 200, r.text);
      assert.deepEqual(r.json, { user: 2 });
      assert.equal(r.setCookie, null);
    }
    // The owner's session cookie does not lift a read token to owner.
    assertError(await api(w, "GET", "/api/__probe/owner", { bearer, cookie: S.owner }), 403, "auth.forbidden");
    // The renewable session was not renewed.
    const [row] = w.query(`SELECT expires_at FROM sessions WHERE token_hash = ${sq(sha256hex(S.renew))}`);
    assert.equal(row.expires_at, T + 10 * DAY);
  });

  test("tokens cannot use /api/auth/*: 403, even an owner token with a session cookie", async () => {
    const bearer = made["CI owner"].token;
    const target = made["CI read"].id;
    for (const [method, path, body] of [
      ["GET", "/api/auth/me"],
      ["PATCH", "/api/auth/me", { display_name: "Hacked" }],
      ["POST", "/api/auth/password", { current_password: "x", new_password: "y" }],
      ["GET", "/api/auth/passkey"],
      ["POST", "/api/auth/passkey/register/options", {}],
      ["DELETE", "/api/auth/passkey/1"],
      ["GET", "/api/auth/tokens"],
      ["POST", "/api/auth/tokens", { name: "minted", scope: "owner" }],
      ["DELETE", `/api/auth/tokens/${target}`],
      ["POST", "/api/auth/logout"],
      ["GET", "/api/auth/params"],
    ]) {
      for (const cookie of [undefined, S.owner]) {
        const r = await api(w, method, path, { bearer, body, cookie });
        assertError(r, 403, "auth.forbidden");
        assert.equal(r.setCookie, null, `${method} ${path}`);
      }
    }
    assert.equal(w.query("SELECT COUNT(*) AS n FROM api_tokens WHERE name = 'minted'")[0].n, 0);
    assert.equal(w.query(`SELECT revoked_at FROM api_tokens WHERE id = ${target}`)[0].revoked_at, null);
    assert.equal(w.query("SELECT display_name FROM users WHERE id = 1")[0].display_name, "Olga");
    assert.equal((await api(w, "GET", "/api/auth/me", { cookie: S.owner })).status, 200);
  });

  test("revoke: own token at once; another user's or a missing one is 404", async () => {
    const t = made["CI read"];
    assertError(await api(w, "DELETE", `/api/auth/tokens/${t.id}`, { cookie: S.writer }), 404, "store.not_found");
    assert.equal((await api(w, "GET", "/api/__probe/read", { bearer: t.token })).status, 200);
    for (const id of ["999999", "abc", "-1", "1.0", "", "99999999999999999999"]) {
      assertError(await api(w, "DELETE", `/api/auth/tokens/${id}`, { cookie: S.owner }), 404, "store.not_found");
    }
    assertError(await api(w, "DELETE", `/api/auth/tokens/${t.id}`), 401, "auth.unauthenticated");
    assert.equal((await api(w, "DELETE", `/api/auth/tokens/${t.id}`, { cookie: S.owner })).status, 204);
    assertError(await api(w, "GET", "/api/__probe/read", { bearer: t.token }), 401, "auth.unauthenticated");
    const [{ revoked_at }] = w.query(`SELECT revoked_at FROM api_tokens WHERE id = ${t.id}`);
    assert.ok(revoked_at > 0);
    assert.equal((await api(w, "DELETE", `/api/auth/tokens/${t.id}`, { cookie: S.owner })).status, 204);
    assert.equal(w.query(`SELECT revoked_at FROM api_tokens WHERE id = ${t.id}`)[0].revoked_at, revoked_at);
    const listed = (await api(w, "GET", "/api/auth/tokens", { cookie: S.owner })).json.tokens.find((x) => x.id === t.id);
    assert.equal(listed.revoked_at, revoked_at);
    // Other tokens keep working.
    assert.equal((await api(w, "GET", "/api/__probe/read", { bearer: made["CI owner"].token })).status, 200);
  });

  test("system log: create and revoke recorded by name, via session; no token or hash anywhere", async () => {
    const rows = w.query("SELECT kind, actor_id, detail FROM system_log WHERE kind LIKE 'auth.token_%' ORDER BY id");
    const created = rows.filter((r) => r.kind === "auth.token_created");
    assert.equal(created.length, 8, JSON.stringify(rows));
    assert.ok(created.some((r) => r.actor_id === 2 && r.detail.startsWith("Will writer\n") && r.detail.includes("scope\twriter")));
    assert.ok(created.every((r) => r.detail.includes("via\tsession")));
    const revoked = rows.filter((r) => r.kind === "auth.token_revoked");
    assert.equal(revoked.length, 1, "revoking twice logs once");
    assert.ok(revoked[0].detail.startsWith("CI read\n"));
    const everything = JSON.stringify(w.query("SELECT * FROM system_log"));
    for (const s of [...secrets, ...Object.values(SEEDED)]) {
      assert.ok(!everything.includes(s), "token in the system log");
      assert.ok(!everything.includes(sha256hex(s)), "token hash in the system log");
      assert.ok(!everything.includes(s.slice(4)), "token body in the system log");
    }
    const log = w.log();
    for (const s of [...secrets, ...Object.values(SEEDED)]) assert.ok(!log.includes(s.slice(4)), "token in the Worker log");
  });
});
