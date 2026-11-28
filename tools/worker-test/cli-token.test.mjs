// `blog token create` end to end: the exact SQL crates/cli/src/token.rs sends (its test keeps
// fixtures/token-create.json fresh), replayed on the real Worker's D1, makes a token that reads
// /api/v1/posts; the guard makes none for a disabled user.
import assert from "node:assert/strict";
import { createHash, randomBytes } from "node:crypto";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { after, before, describe, test } from "node:test";
import { fileURLToPath } from "node:url";

import { d1Batch, startWorker } from "./harness.mjs";

const T = Date.now();
const sha256hex = (s) => createHash("sha256").update(s).digest("hex");
const CREATE = JSON.parse(readFileSync(join(dirname(fileURLToPath(import.meta.url)), "fixtures/token-create.json"), "utf8"));

/** What `blog token create` writes for `user`, with a token made as the CLI makes it. */
function cliCreate(user) {
  const token = `plb_${randomBytes(32).toString("base64url")}`;
  const values = { $user: String(user), $hash: sha256hex(token), $now: String(Date.now()) };
  const fill = (p) => p.replace(/\$(user|hash|now)/g, (k) => values[k]);
  d1Batch(
    w.state,
    CREATE.map((s) => ({ sql: s.sql, params: s.params.map(fill) })),
  );
  return token;
}

let w;

before(async () => {
  w = await startWorker({
    seedSql: `INSERT INTO users (id, login, slug, display_name, role, password_hash, disabled_at, created_at, updated_at) VALUES
      (1, 'will', 'will', 'Will', 'writer', NULL, NULL, ${T}, ${T}),
      (2, 'dis', 'dis', 'Dis', 'writer', NULL, ${T - 1000}, ${T}, ${T});`,
  });
});

after(async () => {
  await w?.stop();
});

describe("blog token create", () => {
  test("a CLI-made token reads /api/v1/posts; the log row has no token", async () => {
    const token = cliCreate(1);
    const r = await fetch(`${w.url}/api/v1/posts`, { headers: { authorization: `Bearer ${token}` } });
    assert.equal(r.status, 200, await r.text());
    const other = await fetch(`${w.url}/api/v1/posts`, { headers: { authorization: `Bearer plb_${randomBytes(32).toString("base64url")}` } });
    assert.equal(other.status, 401, "only the stored hash authenticates");
    const log = w.query("SELECT kind, detail FROM system_log WHERE kind = 'auth.token_created'");
    assert.equal(log.length, 1);
    assert.ok(!log[0].detail.includes(token));
  });

  test("the guard creates nothing for a disabled user", async () => {
    const before = w.query("SELECT count(*) AS n FROM api_tokens")[0].n;
    const token = cliCreate(2);
    assert.equal(w.query("SELECT count(*) AS n FROM api_tokens")[0].n, before);
    const r = await fetch(`${w.url}/api/v1/posts`, { headers: { authorization: `Bearer ${token}` } });
    assert.equal(r.status, 401, await r.text());
  });
});
