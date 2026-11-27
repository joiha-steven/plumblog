// Stretches a password in the browser so the server never sees it (docs/ARCHITECTURE.md,
// Users and auth). Every parameter mirrors crates/auth/src/password.rs: STRETCH_ITERATIONS,
// STRETCHED_LEN and stretch_salt. tools/worker-test/stretch.test.mjs fails if they drift.

export const ITERATIONS = 600000;
export const LENGTH_BYTES = 32;

/** "plumblog:" + siteId + ":" + lowercase(login), like stretch_salt in Rust. */
export function salt(siteId, login) {
  return `plumblog:${siteId}:${login.toLowerCase()}`;
}

function base64url(bytes) {
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/** PBKDF2-SHA256 of the password; resolves to base64url of 32 bytes, the API's `password`. */
export async function stretch(siteId, login, password) {
  const enc = new TextEncoder();
  const key = await crypto.subtle.importKey("raw", enc.encode(password), "PBKDF2", false, ["deriveBits"]);
  const bits = await crypto.subtle.deriveBits(
    { name: "PBKDF2", hash: "SHA-256", salt: enc.encode(salt(siteId, login)), iterations: ITERATIONS },
    key,
    LENGTH_BYTES * 8,
  );
  return base64url(new Uint8Array(bits));
}
