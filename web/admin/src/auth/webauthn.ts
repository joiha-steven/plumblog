/**
 * WebAuthn between the browser API (ArrayBuffers) and the Worker's JSON (base64url strings,
 * crates/worker/src/api/passkey.rs). No imports: tests load it straight into Node.
 */

const ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/** Base64url without padding (RFC 4648 section 5). */
export function b64u(bytes: ArrayBuffer | Uint8Array): string {
  const u = ArrayBuffer.isView(bytes) ? bytes : new Uint8Array(bytes);
  let bin = "";
  for (const b of u) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/**
 * Strict like crates/auth/src/base64url.rs: one spelling per value, so no padding, no
 * whitespace, no impossible length and no non-zero unused bits. Throws on anything else.
 */
export function fromB64u(s: string): Uint8Array<ArrayBuffer> {
  if (s.length % 4 === 1) throw new Error(`base64url: impossible length ${s.length}`);
  const out = new Uint8Array(new ArrayBuffer(Math.floor((s.length * 3) / 4)));
  let o = 0;
  for (let i = 0; i < s.length; i += 4) {
    const chunk = s.slice(i, i + 4);
    let n = 0;
    for (let j = 0; j < chunk.length; j++) {
      const v = ALPHABET.indexOf(chunk.charAt(j));
      if (v < 0) throw new Error(`base64url: bad character at ${i + j}`);
      n |= v << (18 - 6 * j);
    }
    const keep = chunk.length - 1;
    const bytes = [(n >>> 16) & 0xff, (n >>> 8) & 0xff, n & 0xff];
    if (bytes.slice(keep).some((b) => b !== 0)) throw new Error("base64url: non-zero trailing bits");
    for (const b of bytes.slice(0, keep)) out[o++] = b;
  }
  return out;
}

type Obj = Record<string, unknown>;

function obj(v: unknown, what: string): Obj {
  if (typeof v !== "object" || v === null || Array.isArray(v)) throw new Error(`webauthn: ${what} is not an object`);
  return v as Obj;
}

function str(v: unknown, what: string): string {
  if (typeof v !== "string") throw new Error(`webauthn: ${what} is not a string`);
  return v;
}

/** Credential descriptors with their ids decoded; undefined stays undefined. */
function credentials(v: unknown, what: string): Obj[] | undefined {
  if (v === undefined) return undefined;
  if (!Array.isArray(v)) throw new Error(`webauthn: ${what} is not a list`);
  return v.map((c, i) => {
    const d = obj(c, `${what}[${i}]`);
    return { ...d, id: fromB64u(str(d["id"], `${what}[${i}].id`)) };
  });
}

/** The Worker's `publicKey` (register/options) as `navigator.credentials.create` takes it. */
export function createOptions(publicKey: unknown): PublicKeyCredentialCreationOptions {
  const pk = obj(publicKey, "publicKey");
  const user = obj(pk["user"], "publicKey.user");
  const out: Obj = {
    ...pk,
    challenge: fromB64u(str(pk["challenge"], "publicKey.challenge")),
    user: { ...user, id: fromB64u(str(user["id"], "publicKey.user.id")) },
  };
  const exclude = credentials(pk["excludeCredentials"], "publicKey.excludeCredentials");
  if (exclude) out["excludeCredentials"] = exclude;
  return out as unknown as PublicKeyCredentialCreationOptions;
}

/** The Worker's `publicKey` (login/options) as `navigator.credentials.get` takes it. */
export function getOptions(publicKey: unknown): PublicKeyCredentialRequestOptions {
  const pk = obj(publicKey, "publicKey");
  const out: Obj = { ...pk, challenge: fromB64u(str(pk["challenge"], "publicKey.challenge")) };
  const allow = credentials(pk["allowCredentials"], "publicKey.allowCredentials");
  if (allow) out["allowCredentials"] = allow;
  return out as unknown as PublicKeyCredentialRequestOptions;
}

/** The fields register/verify reads from a new credential. */
export function attestation(cred: PublicKeyCredential): { client_data_json: string; attestation_object: string } {
  const r = cred.response as AuthenticatorAttestationResponse;
  return { client_data_json: b64u(r.clientDataJSON), attestation_object: b64u(r.attestationObject) };
}

/**
 * The fields login/verify reads from an assertion (`ASSERTION_FIELDS` in passkey.rs), or
 * with `prefix` "reauth_" the re-authentication fields (`REAUTH_FIELDS`).
 */
export function assertion(cred: PublicKeyCredential, prefix = ""): Record<string, string> {
  const r = cred.response as AuthenticatorAssertionResponse;
  const out: Record<string, string> = {
    [`${prefix}credential_id`]: cred.id,
    [`${prefix}client_data_json`]: b64u(r.clientDataJSON),
    [`${prefix}authenticator_data`]: b64u(r.authenticatorData),
    [`${prefix}signature`]: b64u(r.signature),
  };
  if (r.userHandle && r.userHandle.byteLength > 0) out[`${prefix}user_handle`] = b64u(r.userHandle);
  return out;
}

/** This browser can create and use passkeys. */
export function supported(): boolean {
  return (
    typeof globalThis.PublicKeyCredential === "function" &&
    typeof navigator !== "undefined" &&
    typeof navigator.credentials?.get === "function" &&
    typeof navigator.credentials.create === "function"
  );
}
