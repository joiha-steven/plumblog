// The credential reset at the end of `blog restore`, over HTTP against the real Worker and local
// D1. The import itself needs Cloudflare (its CLI tests run it against SQLite); here the state a
// restore leaves (an old backup's session and token, live again) gets the exact SQL
// crates/cli/src/restore.rs sends, from the fixture its test keeps fresh.
import assert from "node:assert/strict";
import { createHash, randomBytes } from "node:crypto";
import { after, before, describe, test } from "node:test";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { d1Batch, startWorker } from "./harness.mjs";

const COOKIE = "__Host-plumblog_session";
const DAY = 86_400_000;
const T = Date.now();
const sha256hex = (s) => createHash("sha256").update(s).digest("hex");
const b64u = (buf) => Buffer.from(buf).toString("base64url");
const sq = (s) => `'${String(s).replaceAll("'", "''")}'`;
const SESSION = b64u(randomBytes(32));
const TOKEN = `plb_${b64u(randomBytes(32))}`;

const RESET = JSON.parse(readFileSync(join(dirname(fileURLToPath(import.meta.url)), "fixtures/restore-reset.json"), "utf8"));

let w;
before(async () => {
  w = await startWorker({
    seedSql: `INSERT INTO users (id, login, slug, display_name, role, password_hash, created_at, updated_at) VALUES
        (1, 'olga', 'olga', 'Olga', 'owner', NULL, ${T}, ${T});
      INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES (${sq(sha256hex(SESSION))}, 1, ${T}, ${T + DAY});
      INSERT INTO api_tokens (id, user_id, name, token_hash, scope, created_at, expires_at, revoked_at) VALUES
        (1, 1, 'agent', ${sq(sha256hex(TOKEN))}, 'owner', ${T}, NULL, NULL);
      INSERT INTO invites (code_hash, role, created_by, created_at, expires_at, used_at, used_by) VALUES
        ('pending', 'writer', 1, ${T}, ${T + DAY}, NULL, NULL);`,
  });
});

after(async () => {
  await w?.stop();
});

async function get(path, headers) {
  const res = await fetch(w.url + path, { headers });
  return { status: res.status, text: await res.text() };
}
const asSession = () => get("/api/auth/me", { cookie: `${COOKIE}=${SESSION}` });
const asToken = () => get("/api/v1/me", { authorization: `Bearer ${TOKEN}` });

describe("restore credential reset", () => {
  test("a session and a token from the backup work until the reset", async () => {
    assert.equal((await asSession()).status, 200);
    assert.equal((await asToken()).status, 200);
  });

  test("after the reset both are 401, the token is kept as revoked, and a second run changes nothing", async () => {
    const now = String(Date.now());
    const stmts = RESET.map((s) => ({ sql: s.sql, params: s.params.map((p) => (p === "$now" ? now : p)) }));
    d1Batch(w.state, stmts);
    const s = await asSession();
    assert.equal(s.status, 401, s.text);
    const t = await asToken();
    assert.equal(t.status, 401, t.text);
    assert.deepEqual(w.query("SELECT id, revoked_at FROM api_tokens"), [{ id: 1, revoked_at: Number(now) }]);
    assert.deepEqual(w.query("SELECT code_hash FROM invites"), []);
    d1Batch(w.state, stmts);
    assert.deepEqual(w.query("SELECT id, revoked_at FROM api_tokens"), [{ id: 1, revoked_at: Number(now) }]);
  });
});
