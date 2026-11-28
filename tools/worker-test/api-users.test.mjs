// User admin over /api/v1 (owner, session only): invites, role changes, disable/enable,
// delete. Who is refused (writer, read and owner tokens, anonymous), the invite code shown
// once and stored hashed, the last active owner kept even when two owners demote each other
// at the same moment, disabling ending sessions and tokens, and the system log entries.
import assert from "node:assert/strict";
import { createHash, randomBytes } from "node:crypto";
import { after, before, describe, test } from "node:test";

import * as gen from "../../web/admin/src/api/model.gen.ts";
import { checkShape } from "../shape-check.mjs";
import { startWorker } from "./harness.mjs";

const { SHAPES, ...rest } = gen;
const lists = Object.fromEntries(Object.entries(rest).filter(([, v]) => Array.isArray(v)));
const COOKIE = "__Host-plumblog_session";
const DAY = 86_400_000;
const T = Date.now();
const sha = (s) => createHash("sha256").update(s).digest("hex");
const rnd = () => randomBytes(32).toString("base64url");
const plb = () => `plb_${rnd()}`;

const S = { olga: rnd(), otto: rnd(), wendy: rnd(), vic: rnd() };
const TOK = { ownerRead: plb(), ownerOwner: plb(), vic: plb() };
const as = {
  anonymous: {},
  olga: { cookie: `${COOKIE}=${S.olga}` },
  otto: { cookie: `${COOKIE}=${S.otto}` },
  wendy: { cookie: `${COOKIE}=${S.wendy}` },
  vic: { cookie: `${COOKIE}=${S.vic}` },
  readToken: { authorization: `Bearer ${TOK.ownerRead}` },
  ownerToken: { authorization: `Bearer ${TOK.ownerOwner}` },
  vicToken: { authorization: `Bearer ${TOK.vic}` },
};

const users = (rows) =>
  `INSERT INTO users (id, login, slug, display_name, role, password_hash, disabled_at, created_at, updated_at) VALUES ${rows
    .map(([id, login, role]) => `(${id}, '${login}', '${login}', '${login}', '${role}', NULL, NULL, ${T}, ${T})`)
    .join(", ")};`;
const sessions = (rows) =>
  `INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES ${rows
    .map(([secret, id]) => `('${sha(secret)}', ${id}, ${T}, ${T + 29 * DAY})`)
    .join(", ")};`;

// Test-only triggers record the acting owner's role at the moment each of ben's writes lands
// (inside the write's own transaction), so a race can be judged by what the database saw.
const AUDIT = `CREATE TABLE t_audit (what TEXT NOT NULL, role TEXT, disabled_at INTEGER);
CREATE TRIGGER t_invite AFTER INSERT ON invites BEGIN
  INSERT INTO t_audit SELECT 'invite', role, disabled_at FROM users WHERE id = NEW.created_by; END;
CREATE TRIGGER t_disable AFTER UPDATE OF disabled_at ON users WHEN NEW.id = 3 AND NEW.disabled_at IS NOT NULL BEGIN
  INSERT INTO t_audit SELECT 'disable', role, disabled_at FROM users WHERE id = 2; END;`;

let w;
let race;
let trio;
before(async () => {
  [w, race, trio] = await Promise.all([
    startWorker({
      seedSql: [
        users([
          [1, "olga", "owner"],
          [2, "otto", "owner"],
          [3, "wendy", "writer"],
          [4, "vic", "writer"],
        ]),
        sessions([
          [S.olga, 1],
          [S.otto, 2],
          [S.wendy, 3],
          [S.vic, 4],
        ]),
        `INSERT INTO api_tokens (user_id, name, token_hash, scope, created_at, expires_at, revoked_at) VALUES
          (1, 'look', '${sha(TOK.ownerRead)}', 'read', ${T}, NULL, NULL),
          (1, 'all', '${sha(TOK.ownerOwner)}', 'owner', ${T}, NULL, NULL),
          (4, 'ci', '${sha(TOK.vic)}', 'writer', ${T}, NULL, NULL);`,
      ].join("\n"),
    }),
    // Exactly two active owners, for the race and the last-owner refusals: ann (1) signs in
    // with S.olga, ben (2) with S.otto (another database, so the same secrets are fine).
    startWorker({
      seedSql: [
        users([
          [1, "ann", "owner"],
          [2, "ben", "owner"],
        ]),
        sessions([
          [S.olga, 1],
          [S.otto, 2],
        ]),
      ].join("\n"),
    }),
    // Three active owners: ann (S.olga) demotes ben (S.otto) while ben acts on cat.
    startWorker({
      seedSql: [
        users([
          [1, "ann", "owner"],
          [2, "ben", "owner"],
          [3, "cat", "owner"],
        ]),
        sessions([
          [S.olga, 1],
          [S.otto, 2],
          [S.wendy, 3],
        ]),
        AUDIT,
      ].join("\n"),
    }),
  ]);
});
after(async () => {
  await Promise.all([w?.stop(), race?.stop(), trio?.stop()]);
});

async function api(worker, method, path, { as: who = {}, body } = {}) {
  const headers = { origin: worker.url, ...who };
  if (body !== undefined) headers["content-type"] = "application/json";
  const res = await fetch(worker.url + path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) });
  const text = await res.text();
  return { status: res.status, text, json: text ? JSON.parse(text) : null, setCookie: res.headers.get("set-cookie") };
}
function assertError(r, status, code, field) {
  assert.equal(r.status, status, r.text);
  assert.deepEqual(r.json, field ? { error: code, field } : { error: code });
}
const shaped = (name, r, status) => {
  assert.equal(r.status, status, r.text);
  assert.deepEqual(checkShape(SHAPES, lists, name, r.json), [], r.text);
  return r.json;
};
const accept = (worker, code, login) =>
  api(worker, "POST", "/api/auth/invite/accept", {
    body: { code, login, slug: login, display_name: login, password: rnd() },
  });
const ROUTES = [
  ["GET", "/api/v1/invites"],
  ["POST", "/api/v1/invites", { role: "writer" }],
  ["DELETE", "/api/v1/invites/1"],
  ["PUT", "/api/v1/users/4/role", { role: "owner" }],
  ["PUT", "/api/v1/users/4/disabled", { disabled: true }],
  ["DELETE", "/api/v1/users/4"],
];

describe("who may manage users", () => {
  test("the reference marks every user-admin route session only", async () => {
    const ref = (await api(w, "GET", "/api/v1/reference", { as: as.olga })).json;
    for (const [method, path] of ROUTES) {
      const pattern = path.replace(/\/\d+(?=\/|$)/, "/{id}");
      const r = ref.routes.find((x) => x.method === method && x.path === pattern);
      assert.ok(r, `${method} ${pattern}`);
      assert.equal(r.access, "user.manage");
      assert.equal(r.session_only, true, pattern);
    }
    assert.equal(ref.routes.find((x) => x.path === "/api/v1/users").session_only, false);
  });

  test("anonymous 401; writer session, read token and even an owner token 403; nothing changes", async () => {
    for (const [method, path, body] of ROUTES) {
      assertError(await api(w, method, path, { as: as.anonymous, body }), 401, "auth.unauthenticated");
      for (const who of [as.wendy, as.readToken, as.ownerToken, as.vicToken]) {
        assertError(await api(w, method, path, { as: who, body }), 403, "auth.forbidden");
      }
    }
    assert.deepEqual(w.query("SELECT id, role, disabled_at FROM users WHERE id = 4"), [{ id: 4, role: "writer", disabled_at: null }]);
    assert.equal(w.query("SELECT COUNT(*) AS n FROM invites")[0].n, 0);
  });

  test("unknown ids are 404; bad input is 400 naming the field", async () => {
    assertError(await api(w, "PUT", "/api/v1/users/999/role", { as: as.olga, body: { role: "writer" } }), 404, "user.not_found");
    assertError(await api(w, "PUT", "/api/v1/users/999/disabled", { as: as.olga, body: { disabled: true } }), 404, "user.not_found");
    assertError(await api(w, "DELETE", "/api/v1/users/999", { as: as.olga }), 404, "user.not_found");
    assertError(await api(w, "DELETE", "/api/v1/invites/999", { as: as.olga }), 404, "store.not_found");
    assertError(await api(w, "PUT", "/api/v1/users/4/role", { as: as.olga, body: { role: "admin" } }), 400, "input.invalid", "role");
    assertError(await api(w, "PUT", "/api/v1/users/4/disabled", { as: as.olga, body: { disabled: "yes" } }), 400, "input.invalid", "disabled");
    assertError(await api(w, "POST", "/api/v1/invites", { as: as.olga, body: { role: "editor" } }), 400, "input.invalid", "role");
    assertError(await api(w, "POST", "/api/v1/invites", { as: as.olga, body: { role: "writer", expires_at: T - 1 } }), 400, "input.invalid", "expires_at");
    assertError(await api(w, "POST", "/api/v1/invites", { as: as.olga, body: { role: "writer", expires_at: Date.now() + 31 * DAY } }), 400, "input.invalid", "expires_at");
  });
});

describe("invites", () => {
  test("the code is shown once, stored hashed, never listed or logged; it works once", async () => {
    const made = shaped("invite_created", await api(w, "POST", "/api/v1/invites", { as: as.olga, body: { role: "writer" } }), 201);
    assert.match(made.code, /^[A-Za-z0-9_-]{43}$/);
    assert.equal(made.role, "writer");
    assert.equal(made.created_by, 1);
    // Default expiry: 7 days.
    assert.ok(Math.abs(made.expires_at - made.created_at - 7 * DAY) < 1000, String(made.expires_at - made.created_at));
    const [row] = w.query(`SELECT code_hash FROM invites WHERE id = ${made.id}`);
    assert.equal(row.code_hash, sha(made.code));

    const listed = shaped("invite_list", await api(w, "GET", "/api/v1/invites", { as: as.olga }), 200);
    const mine = listed.invites.find((i) => i.id === made.id);
    assert.deepEqual(mine, { id: made.id, role: "writer", created_by: 1, created_at: made.created_at, expires_at: made.expires_at });
    const text = JSON.stringify(listed);
    assert.ok(!text.includes(made.code) && !text.includes(row.code_hash), text);

    const joined = await accept(w, made.code, "ida");
    assert.equal(joined.status, 201, joined.text);
    assert.equal(joined.json.user.role, "writer");
    assertError(await accept(w, made.code, "ida2"), 410, "invite.invalid");
    // A used invite leaves the list and cannot be revoked (it records how ida joined).
    const after = (await api(w, "GET", "/api/v1/invites", { as: as.olga })).json;
    assert.ok(!after.invites.some((i) => i.id === made.id));
    assertError(await api(w, "DELETE", `/api/v1/invites/${made.id}`, { as: as.olga }), 404, "store.not_found");
    const [kept] = w.query(`SELECT used_at, used_by FROM invites WHERE id = ${made.id}`);
    assert.equal(kept.used_by, joined.json.user.id, "the used invite row stays");
    assert.equal(typeof kept.used_at, "number");

    const logged = w.query("SELECT detail FROM system_log").map((r) => r.detail).join("\n");
    assert.ok(!logged.includes(made.code) && !logged.includes(row.code_hash), "invite code in the log");
  });

  test("an owner invite makes an owner; revoked and expired invites are refused", async () => {
    const owner = (await api(w, "POST", "/api/v1/invites", { as: as.olga, body: { role: "owner", expires_at: Date.now() + DAY } })).json;
    const joined = await accept(w, owner.code, "oona");
    assert.equal(joined.json.user.role, "owner", joined.text);

    const revoked = (await api(w, "POST", "/api/v1/invites", { as: as.olga, body: { role: "writer" } })).json;
    const del = await api(w, "DELETE", `/api/v1/invites/${revoked.id}`, { as: as.olga });
    assert.equal(del.status, 204, del.text);
    assertError(await api(w, "DELETE", `/api/v1/invites/${revoked.id}`, { as: as.olga }), 404, "store.not_found");
    assertError(await accept(w, revoked.code, "rev"), 410, "invite.invalid");

    const expired = (await api(w, "POST", "/api/v1/invites", { as: as.olga, body: { role: "writer" } })).json;
    w.exec(`UPDATE invites SET expires_at = ${T - 1} WHERE id = ${expired.id}`);
    // Listed until revoked, so the admin can show it as expired.
    const listed = (await api(w, "GET", "/api/v1/invites", { as: as.olga })).json;
    assert.ok(listed.invites.some((i) => i.id === expired.id));
    assertError(await accept(w, expired.code, "late"), 410, "invite.invalid");
    assert.equal(w.query("SELECT COUNT(*) AS n FROM users WHERE login IN ('rev', 'late')")[0].n, 0);
  });
});

describe("roles, disabling, deleting", () => {
  test("role change answers the user; writer to owner and back", async () => {
    const up = shaped("user", await api(w, "PUT", "/api/v1/users/3/role", { as: as.olga, body: { role: "owner" } }), 200);
    assert.equal(up.role, "owner");
    assert.ok(!("password_hash" in up));
    const down = shaped("user", await api(w, "PUT", "/api/v1/users/3/role", { as: as.olga, body: { role: "writer" } }), 200);
    assert.equal(down.role, "writer");
    // Demoted, the writer's next request has no user admin.
    assertError(await api(w, "GET", "/api/v1/invites", { as: as.wendy }), 403, "auth.forbidden");
  });

  test("disabling ends the user's sessions and revokes their tokens; enabling brings neither back", async () => {
    assert.equal((await api(w, "GET", "/api/v1/me", { as: as.vic })).status, 200);
    assert.equal((await api(w, "GET", "/api/v1/me", { as: as.vicToken })).status, 200);
    const off = shaped("user", await api(w, "PUT", "/api/v1/users/4/disabled", { as: as.olga, body: { disabled: true } }), 200);
    assert.equal(typeof off.disabled_at, "number");
    assertError(await api(w, "GET", "/api/v1/me", { as: as.vic }), 401, "auth.unauthenticated");
    assertError(await api(w, "GET", "/api/v1/me", { as: as.vicToken }), 401, "auth.unauthenticated");
    assert.equal(w.query("SELECT COUNT(*) AS n FROM sessions WHERE user_id = 4")[0].n, 0);
    assert.equal(w.query("SELECT COUNT(*) AS n FROM api_tokens WHERE user_id = 4 AND revoked_at IS NULL")[0].n, 0);

    const on = shaped("user", await api(w, "PUT", "/api/v1/users/4/disabled", { as: as.olga, body: { disabled: false } }), 200);
    assert.equal(on.disabled_at, null);
    assertError(await api(w, "GET", "/api/v1/me", { as: as.vicToken }), 401, "auth.unauthenticated");
    // Other users keep theirs.
    assert.equal((await api(w, "GET", "/api/v1/me", { as: as.readToken })).status, 200);
  });

  test("delete: 204, their session ends, the list no longer has them", async () => {
    const made = (await api(w, "POST", "/api/v1/invites", { as: as.olga, body: { role: "writer" } })).json;
    const joined = await accept(w, made.code, "dee");
    const id = joined.json.user.id;
    const session = /^__Host-plumblog_session=([A-Za-z0-9_-]+);/.exec(joined.setCookie)?.[1];
    assert.ok(session, joined.setCookie);
    const dee = { cookie: `${COOKIE}=${session}` };
    assert.equal((await api(w, "GET", "/api/v1/me", { as: dee })).status, 200);
    // While they author a post, the delete is refused and nothing goes.
    const post = await api(w, "POST", "/api/v1/posts", { as: dee, body: { lang: "en", slug: "by-dee", title: "By Dee", markdown: "Text." } });
    assert.equal(post.status, 201, post.text);
    w.exec(`INSERT INTO passkeys (user_id, credential_id, public_key, sign_count, name, created_at) VALUES (${id}, 'cred-dee', 'pk', 0, 'key', ${T});
      INSERT INTO api_tokens (user_id, name, token_hash, scope, created_at) VALUES (${id}, 'dee', '${sha(plb())}', 'writer', ${T});
      INSERT INTO invites (code_hash, role, created_by, created_at, expires_at) VALUES ('${sha(rnd())}', 'writer', ${id}, ${T}, ${T + DAY});`);
    const rows = (table, col) => w.query(`SELECT COUNT(*) AS n FROM ${table} WHERE ${col} = ${id}`)[0].n;
    assertError(await api(w, "DELETE", `/api/v1/users/${id}`, { as: as.olga }), 409, "store.conflict");
    assert.deepEqual([rows("passkeys", "user_id"), rows("api_tokens", "user_id"), rows("invites", "created_by")], [1, 1, 1]);
    assert.equal((await api(w, "GET", "/api/v1/me", { as: dee })).status, 200);
    w.exec(`UPDATE posts SET author_id = 1 WHERE id = ${post.json.id}`);
    const del = await api(w, "DELETE", `/api/v1/users/${id}`, { as: as.olga });
    assert.equal(del.status, 204, del.text);
    assert.deepEqual([rows("passkeys", "user_id"), rows("api_tokens", "user_id"), rows("invites", "created_by"), rows("sessions", "user_id")], [0, 0, 0, 0]);
    assertError(await api(w, "GET", "/api/v1/me", { as: dee }), 401, "auth.unauthenticated");
    const list = (await api(w, "GET", "/api/v1/users", { as: as.olga })).json;
    assert.ok(!list.users.some((u) => u.id === id));
    assertError(await api(w, "DELETE", `/api/v1/users/${id}`, { as: as.olga }), 404, "user.not_found");
  });

  test("an owner may demote themself while another active owner exists", async () => {
    const self = shaped("user", await api(w, "PUT", "/api/v1/users/2/role", { as: as.otto, body: { role: "writer" } }), 200);
    assert.equal(self.role, "writer");
    assertError(await api(w, "GET", "/api/v1/invites", { as: as.otto }), 403, "auth.forbidden");
  });

  test("every action is in the system log, with who did it and no secret", async () => {
    const rows = w.query("SELECT kind, actor_id, detail FROM system_log WHERE kind = 'user.changed' ORDER BY id");
    const actions = rows.map((r) => /(?:^|\n)action\t([a-z_]+)/.exec(r.detail)?.[1]);
    for (const a of ["invite_create", "invite_revoke", "role", "disable", "enable", "delete"]) {
      assert.ok(actions.includes(a), `${a} in ${actions}`);
    }
    assert.ok(rows.every((r) => r.actor_id === 1 || r.actor_id === 2), JSON.stringify(rows));
    assert.ok(rows.every((r) => /(?:^|\n)via\tsession/.test(r.detail)), JSON.stringify(rows));
  });

  test("an owner deleting themself (another owner remains) is logged, the actor id in a field", async () => {
    const secret = rnd();
    w.exec(`INSERT INTO users (id, login, slug, display_name, role, created_at, updated_at) VALUES (40, 'zed', 'zed', 'zed', 'owner', ${T}, ${T});
      INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES ('${sha(secret)}', 40, ${T}, ${T + DAY});`);
    const zed = { cookie: `${COOKIE}=${secret}` };
    const del = await api(w, "DELETE", "/api/v1/users/40", { as: zed });
    assert.equal(del.status, 204, del.text);
    const rows = w.query("SELECT actor_id, detail FROM system_log WHERE kind = 'user.changed' AND detail LIKE '%user 40%'");
    assert.equal(rows.length, 1, JSON.stringify(rows));
    assert.equal(rows[0].actor_id, null);
    assert.match(rows[0].detail, /(?:^|\n)action\tdelete(?:\n|$)/);
    assert.match(rows[0].detail, /(?:^|\n)actor\t40(?:\n|$)/);
  });
});

describe("the last active owner", () => {
  test("two owners demoting each other at once: exactly one wins, one owner remains", async () => {
    for (let round = 0; round < 5; round++) {
      race.exec("UPDATE users SET role = 'owner' WHERE id IN (1, 2)");
      const [a, b] = await Promise.all([
        api(race, "PUT", "/api/v1/users/2/role", { as: as.olga, body: { role: "writer" } }),
        api(race, "PUT", "/api/v1/users/1/role", { as: as.otto, body: { role: "writer" } }),
      ]);
      const won = [a, b].filter((r) => r.status === 200);
      assert.equal(won.length, 1, `round ${round}: ${a.text} / ${b.text}`);
      // The loser either saw itself demoted first (403) or hit the guard in the write (409).
      const lost = [a, b].find((r) => r.status !== 200);
      assert.ok(
        (lost.status === 409 && lost.json.error === "user.last_owner") || (lost.status === 403 && lost.json.error === "auth.forbidden"),
        lost.text,
      );
      const owners = race.query("SELECT COUNT(*) AS n FROM users WHERE role = 'owner' AND disabled_at IS NULL")[0].n;
      assert.equal(owners, 1, `round ${round}`);
    }
  });

  test("the only owner cannot demote, disable or delete themself; nothing changes", async () => {
    race.exec("UPDATE users SET role = 'owner' WHERE id = 1; UPDATE users SET role = 'writer' WHERE id = 2");
    const before = race.query("SELECT * FROM users WHERE id = 1");
    assertError(await api(race, "PUT", "/api/v1/users/1/role", { as: as.olga, body: { role: "writer" } }), 409, "user.last_owner");
    assertError(await api(race, "PUT", "/api/v1/users/1/disabled", { as: as.olga, body: { disabled: true } }), 409, "user.last_owner");
    assertError(await api(race, "DELETE", "/api/v1/users/1", { as: as.olga }), 409, "user.last_owner");
    assert.deepEqual(race.query("SELECT * FROM users WHERE id = 1"), before);
    // The refused disable kept the session.
    assert.equal((await api(race, "GET", "/api/v1/me", { as: as.olga })).status, 200);
    // A disabled owner does not count: with ben a disabled owner, ann is still the last one.
    race.exec(`UPDATE users SET role = 'owner', disabled_at = ${T} WHERE id = 2`);
    assertError(await api(race, "PUT", "/api/v1/users/1/role", { as: as.olga, body: { role: "writer" } }), 409, "user.last_owner");
    // Re-enabling ben makes two again; then ann may step down.
    shaped("user", await api(race, "PUT", "/api/v1/users/2/disabled", { as: as.olga, body: { disabled: false } }), 200);
    shaped("user", await api(race, "PUT", "/api/v1/users/1/role", { as: as.olga, body: { role: "writer" } }), 200);
  });

  test("an owner demoted mid-request writes nothing: every write lands only while its actor is an active owner", async () => {
    for (let round = 0; round < 30; round++) {
      trio.exec("UPDATE users SET role = 'owner', disabled_at = NULL; DELETE FROM invites; DELETE FROM t_audit");
      const [demote, invite, disable] = await Promise.all([
        api(trio, "PUT", "/api/v1/users/2/role", { as: as.olga, body: { role: "writer" } }),
        api(trio, "POST", "/api/v1/invites", { as: as.otto, body: { role: "owner" } }),
        api(trio, "PUT", "/api/v1/users/3/disabled", { as: as.otto, body: { disabled: true } }),
      ]);
      const where = `round ${round}: ${demote.text} / ${invite.text} / ${disable.text}`;
      assert.equal(demote.status, 200, where);
      // What the database saw when each of ben's writes landed: ben still an active owner.
      const audit = trio.query("SELECT what, role, disabled_at FROM t_audit");
      for (const a of audit) assert.deepEqual({ role: a.role, disabled_at: a.disabled_at }, { role: "owner", disabled_at: null }, `${where} ${JSON.stringify(a)}`);
      const invites = trio.query("SELECT created_by, role FROM invites");
      const cat = trio.query("SELECT disabled_at FROM users WHERE id = 3")[0];
      // Each of ben's requests either landed (and was audited) or was refused and wrote nothing.
      if (invite.status === 201) assert.ok(audit.some((a) => a.what === "invite"), where);
      else assertError(invite, 403, "auth.forbidden");
      // An invite that landed first died with the demotion: ben has no unused invite left.
      assert.deepEqual(invites, [], where);
      if (disable.status === 200) assert.ok(audit.some((a) => a.what === "disable"), where);
      else {
        assertError(disable, 403, "auth.forbidden");
        assert.equal(cat.disabled_at, null, where);
      }
      assert.equal(audit.filter((a) => a.what === "disable").length, cat.disabled_at === null ? 0 : 1, where);
      assert.equal(trio.query("SELECT role FROM users WHERE id = 2")[0].role, "writer", where);
      assert.ok(trio.query("SELECT COUNT(*) AS n FROM users WHERE role = 'owner' AND disabled_at IS NULL")[0].n >= 1, where);
    }
  });

  test("an owner's unused invite dies when that owner is disabled: accepting it creates no one", async () => {
    trio.exec("UPDATE users SET role = 'owner', disabled_at = NULL; DELETE FROM invites");
    const made = shaped("invite_created", await api(trio, "POST", "/api/v1/invites", { as: as.otto, body: { role: "owner" } }), 201);
    shaped("user", await api(trio, "PUT", "/api/v1/users/2/disabled", { as: as.olga, body: { disabled: true } }), 200);
    const users = trio.query("SELECT COUNT(*) AS n FROM users")[0].n;
    assertError(await accept(trio, made.code, "thief"), 410, "invite.invalid");
    assert.equal(trio.query("SELECT COUNT(*) AS n FROM users")[0].n, users);
    assert.equal(trio.query("SELECT COUNT(*) AS n FROM invites WHERE created_by = 2")[0].n, 0);
  });
});

describe("owner tokens, reset codes, no-ops and self-removal", () => {
  const BEA = { session: rnd(), owner: plb(), writer: plb() };
  const bea = { cookie: `${COOKIE}=${BEA.session}` };
  const beaOwnerToken = { authorization: `Bearer ${BEA.owner}` };
  const beaWriterToken = { authorization: `Bearer ${BEA.writer}` };
  const logCount = () => w.query("SELECT COUNT(*) AS n FROM system_log WHERE kind = 'user.changed'")[0].n;
  before(() => {
    w.exec(`INSERT INTO users (id, login, slug, display_name, role, created_at, updated_at) VALUES (50, 'bea', 'bea', 'bea', 'owner', ${T}, ${T});
      ${sessions([[BEA.session, 50]])}
      INSERT INTO api_tokens (user_id, name, token_hash, scope, created_at) VALUES
        (50, 'all', '${sha(BEA.owner)}', 'owner', ${T}), (50, 'ci', '${sha(BEA.writer)}', 'writer', ${T});`);
  });

  test("demoting an owner revokes their owner tokens; promoting again does not bring them back", async () => {
    assert.equal((await api(w, "GET", "/api/v1/me", { as: beaOwnerToken })).status, 200);
    shaped("user", await api(w, "PUT", "/api/v1/users/50/role", { as: as.olga, body: { role: "writer" } }), 200);
    assertError(await api(w, "GET", "/api/v1/me", { as: beaOwnerToken }), 401, "auth.unauthenticated");
    assert.equal((await api(w, "GET", "/api/v1/me", { as: beaWriterToken })).status, 200, "the writer token stays");
    shaped("user", await api(w, "PUT", "/api/v1/users/50/role", { as: as.olga, body: { role: "owner" } }), 200);
    assertError(await api(w, "GET", "/api/v1/me", { as: beaOwnerToken }), 401, "auth.unauthenticated");
    assertError(await api(w, "GET", "/api/v1/invites", { as: beaOwnerToken }), 401, "auth.unauthenticated");
    const [row] = w.query(`SELECT revoked_at FROM api_tokens WHERE token_hash = '${sha(BEA.owner)}'`);
    assert.equal(typeof row.revoked_at, "number");
  });

  test("a role change logs the old and the new role", async () => {
    shaped("user", await api(w, "PUT", "/api/v1/users/4/role", { as: as.olga, body: { role: "owner" } }), 200);
    const [last] = w.query("SELECT detail FROM system_log WHERE kind = 'user.changed' AND detail LIKE '%user 4%' ORDER BY id DESC LIMIT 1");
    assert.match(last.detail, /(?:^|\n)from\twriter(?:\n|$)/, last.detail);
    assert.match(last.detail, /(?:^|\n)to\towner(?:\n|$)/, last.detail);
    shaped("user", await api(w, "PUT", "/api/v1/users/4/role", { as: as.olga, body: { role: "writer" } }), 200);
  });

  test("a no-op role change or disable is 204: nothing written, nothing logged", async () => {
    const before = w.query("SELECT id, role, disabled_at, updated_at FROM users WHERE id IN (1, 3, 4) ORDER BY id");
    const n = logCount();
    for (const [path, body] of [
      ["/api/v1/users/1/role", { role: "owner" }],
      ["/api/v1/users/4/role", { role: "writer" }],
      ["/api/v1/users/4/disabled", { disabled: false }],
    ]) {
      const r = await api(w, "PUT", path, { as: as.olga, body });
      assert.equal(r.status, 204, `${path} ${r.text}`);
      assert.equal(r.text, "");
    }
    assert.deepEqual(w.query("SELECT id, role, disabled_at, updated_at FROM users WHERE id IN (1, 3, 4) ORDER BY id"), before);
    assert.equal(logCount(), n, "no-op logged");
    // Disabling a disabled user: the first disable time stays.
    shaped("user", await api(w, "PUT", "/api/v1/users/3/disabled", { as: as.olga, body: { disabled: true } }), 200);
    const [off] = w.query("SELECT disabled_at, updated_at FROM users WHERE id = 3");
    const m = logCount();
    const again = await api(w, "PUT", "/api/v1/users/3/disabled", { as: as.olga, body: { disabled: true } });
    assert.equal(again.status, 204, again.text);
    assert.deepEqual(w.query("SELECT disabled_at, updated_at FROM users WHERE id = 3"), [off]);
    assert.equal(logCount(), m, "no-op disable logged");
    shaped("user", await api(w, "PUT", "/api/v1/users/3/disabled", { as: as.olga, body: { disabled: false } }), 200);
    // A no-op on a missing user is still 404.
    assertError(await api(w, "PUT", "/api/v1/users/999/role", { as: as.olga, body: { role: "owner" } }), 404, "user.not_found");
  });

  test("disabling an owner deletes their pending reset code; re-enabled, the code is refused", async () => {
    const code = rnd();
    const now = Date.now();
    w.exec(`INSERT INTO settings (key, value_json, updated_at) VALUES
      ('owner_reset_hash', '${JSON.stringify(sha(code))}', ${now}), ('owner_reset_user', '50', ${now}),
      ('owner_reset_expires_at', '${now + 3_600_000}', ${now}), ('owner_reset_at', '${now}', ${now}),
      ('owner_reset_keep_tokens', 'false', ${now});`);
    shaped("user", await api(w, "PUT", "/api/v1/users/50/disabled", { as: as.olga, body: { disabled: true } }), 200);
    assert.deepEqual(w.query("SELECT key FROM settings WHERE key LIKE 'owner_reset%'"), []);
    shaped("user", await api(w, "PUT", "/api/v1/users/50/disabled", { as: as.olga, body: { disabled: false } }), 200);
    const r = await api(w, "POST", "/api/auth/reset", { as: { "cf-connecting-ip": "203.0.113.50" }, body: { code, login: "bea", password: rnd() } });
    assertError(r, 403, "setup.invalid");
    assert.equal(w.query("SELECT password_hash FROM users WHERE id = 50")[0].password_hash, null);
  });

  test("demoting an owner deletes their pending reset code; promoted again, the code is refused", async () => {
    const code = rnd();
    const now = Date.now();
    w.exec(`INSERT INTO settings (key, value_json, updated_at) VALUES
      ('owner_reset_hash', '${JSON.stringify(sha(code))}', ${now}), ('owner_reset_user', '50', ${now}),
      ('owner_reset_expires_at', '${now + 3_600_000}', ${now}), ('owner_reset_at', '${now}', ${now}),
      ('owner_reset_keep_tokens', 'false', ${now});`);
    shaped("user", await api(w, "PUT", "/api/v1/users/50/role", { as: as.olga, body: { role: "writer" } }), 200);
    assert.deepEqual(w.query("SELECT key FROM settings WHERE key LIKE 'owner_reset%'"), []);
    shaped("user", await api(w, "PUT", "/api/v1/users/50/role", { as: as.olga, body: { role: "owner" } }), 200);
    const r = await api(w, "POST", "/api/auth/reset", { as: { "cf-connecting-ip": "203.0.113.51" }, body: { code, login: "bea", password: rnd() } });
    assertError(r, 403, "setup.invalid");
    assert.equal(w.query("SELECT password_hash FROM users WHERE id = 50")[0].password_hash, null);
  });

  test("an owner disabling or deleting themself gets the sign-out cookie; acting on another does not", async () => {
    const clear = (await api(w, "POST", "/api/auth/logout")).setCookie;
    assert.match(clear, /^__Host-plumblog_session=;/, clear);
    const [s1, s2] = [rnd(), rnd()];
    w.exec(`INSERT INTO users (id, login, slug, display_name, role, created_at, updated_at) VALUES
        (51, 'sid', 'sid', 'sid', 'owner', ${T}, ${T}), (52, 'sue', 'sue', 'sue', 'owner', ${T}, ${T});
      ${sessions([[s1, 51], [s2, 52]])}`);
    const other = await api(w, "PUT", "/api/v1/users/4/disabled", { as: as.olga, body: { disabled: true } });
    assert.equal(other.status, 200, other.text);
    assert.equal(other.setCookie, null);
    await api(w, "PUT", "/api/v1/users/4/disabled", { as: as.olga, body: { disabled: false } });
    const off = await api(w, "PUT", "/api/v1/users/51/disabled", { as: { cookie: `${COOKIE}=${s1}` }, body: { disabled: true } });
    shaped("user", off, 200);
    assert.equal(off.setCookie, clear);
    const del = await api(w, "DELETE", "/api/v1/users/52", { as: { cookie: `${COOKIE}=${s2}` } });
    assert.equal(del.status, 204, del.text);
    assert.equal(del.setCookie, clear);
  });

  test("the invite list holds at most 500 unused invites, newest first", async () => {
    // Later than every invite the earlier tests made, so these are the newest.
    const at = Date.now() + DAY;
    const rows = Array.from({ length: 501 }, (_, i) => `('bulk-${i}', 'writer', 1, ${at + i}, ${at + DAY})`);
    w.exec(`INSERT INTO invites (code_hash, role, created_by, created_at, expires_at) VALUES ${rows.join(", ")};
      INSERT INTO invites (code_hash, role, created_by, created_at, expires_at, used_at, used_by) VALUES ('bulk-used', 'writer', 1, ${at + 1000}, ${at + DAY}, ${at + 1000}, 1);`);
    const list = shaped("invite_list", await api(w, "GET", "/api/v1/invites", { as: as.olga }), 200);
    assert.equal(list.invites.length, 500);
    // The used one, though newest, is left out; the oldest bulk one is past the bound.
    assert.equal(list.invites[0].created_at, at + 500);
    assert.equal(list.invites.at(-1).created_at, at + 1);
    assert.ok(list.invites.every((i, k) => k === 0 || list.invites[k - 1].created_at >= i.created_at));
    w.exec("DELETE FROM invites WHERE code_hash LIKE 'bulk-%'");
  });
});
