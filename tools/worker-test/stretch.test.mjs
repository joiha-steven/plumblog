// web/admin/js/stretch.js against node:crypto and the constants in crates/auth (no Worker
// needed). The in-Worker Rust side of the same vector is checked in auth.test.mjs.
import assert from "node:assert/strict";
import { pbkdf2Sync } from "node:crypto";
import { readFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { ITERATIONS, LENGTH_BYTES, salt, stretch } from "../../web/admin/js/stretch.js";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../..");

// Fixed vector, also in auth.test.mjs; the login has a non-ASCII capital to pin Unicode
// lowercasing on both sides.
const VECTOR = {
  siteId: "s1te-ID",
  login: "Tr\u1ea6N Alice",
  password: "correct horse \u00e9",
  salt: "plumblog:s1te-ID:tr\u1ea7n alice",
};

function rustConst(name) {
  const src = readFileSync(join(ROOT, "crates/auth/src/password.rs"), "utf8");
  const m = src.match(new RegExp(`pub const ${name}: \\w+ = ([0-9_]+);`));
  assert.ok(m, `${name} not found in crates/auth/src/password.rs`);
  return Number(m[1].replaceAll("_", ""));
}

test("stretch.js parameters equal the Rust constants", () => {
  assert.equal(ITERATIONS, rustConst("STRETCH_ITERATIONS"));
  assert.equal(LENGTH_BYTES, rustConst("STRETCHED_LEN"));
  assert.equal(salt(VECTOR.siteId, VECTOR.login), VECTOR.salt);
});

test("stretch.js equals node:crypto PBKDF2 on the fixed vector (and timing)", async () => {
  const t0 = performance.now();
  const got = await stretch(VECTOR.siteId, VECTOR.login, VECTOR.password);
  const ms = performance.now() - t0;
  const want = pbkdf2Sync(VECTOR.password, VECTOR.salt, ITERATIONS, LENGTH_BYTES, "sha256").toString("base64url");
  assert.equal(got, want);
  assert.match(got, /^[A-Za-z0-9_-]{43}$/);
  console.log(`stretch.js: ${ITERATIONS} PBKDF2-SHA256 iterations in Node took ${ms.toFixed(0)} ms`);
});
