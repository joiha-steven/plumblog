/**
 * Sign-in, setup, owner reset, invites, sign-out and passkeys over the Worker's auth API
 * (crates/worker/src/api/auth.rs, passkey.rs). Screens call these and show the Result: every
 * function resolves, never throws, and logs what failed. Passwords leave the browser only
 * stretched (stretch.ts), salted with the login they are for.
 */

import { type Api, type ApiError, type Me, type Result, fail } from "../api/client";
import { clearDrafts } from "../editor/drafts";
import { ITERATIONS, stretch } from "./stretch";
import { assertion, attestation, createOptions, getOptions, supported } from "./webauthn";

export interface Params {
  site_id: string;
  stretch_iterations: number;
  turnstile_site_key: string | null;
  /** True while the site has no account at all (setup codes work only then). Missing (an older Worker) reads as false. */
  setup_required?: boolean;
}

/** `passkey_json` in passkey.rs. Times are Unix milliseconds. */
export interface Passkey {
  id: number;
  name: string;
  created_at: number;
  last_used_at: number | null;
}

/** The fields setup and invite acceptance both take (`NEW_ACCOUNT_FIELDS` in auth.rs). */
export interface NewAccount {
  code: string;
  login: string;
  slug: string;
  display_name: string;
  password: string;
}

/** Proof of identity before adding or removing a passkey: the password, or another passkey. */
export type Reauth = { password: string; login: string } | "passkey";

type Body = Record<string, string | boolean>;

const failed = <T>(error: ApiError): Result<T> => ({ ok: false, error });
const ok = <T>(value: T): Result<T> => ({ ok: true, value });

/** Runs a flow; anything thrown (a broken fake, a browser bug) becomes a logged bad_response. */
async function guard<T>(what: string, run: () => Promise<Result<T>>): Promise<Result<T>> {
  try {
    return await run();
  } catch (e) {
    console.error(`auth: ${what} failed`, e);
    return fail("bad_response", 0);
  }
}

const withToken = (body: Body, token: string | undefined): Body =>
  token ? { ...body, turnstile_token: token } : body;

const cache = new WeakMap<Api, Promise<Result<Params>>>();

/** `GET /api/auth/params`, fetched once per client; a failure is not kept, so a retry asks again. */
export function params(api: Api): Promise<Result<Params>> {
  let p = cache.get(api);
  if (!p) {
    p = guard("params", () => api.call<Params>("GET", "/api/auth/params"));
    cache.set(api, p);
    void p.then((r) => {
      if (!r.ok) cache.delete(api);
    });
  }
  return p;
}

/** The stretched `password` for `login` on this site. */
async function stretched(api: Api, login: string, password: string): Promise<Result<string>> {
  const p = await params(api);
  if (!p.ok) return failed(p.error);
  if (p.value.stretch_iterations !== ITERATIONS) {
    // The server would refuse every password: say why once, here, not as wrong credentials.
    console.error(`auth: server stretches with ${p.value.stretch_iterations} iterations, this admin with ${ITERATIONS}`);
    return fail("bad_response", 0);
  }
  try {
    return ok(await stretch(p.value.site_id, login, password));
  } catch (e) {
    // No WebCrypto: only over plain http outside localhost.
    console.error("auth: stretching the password failed", e);
    return fail("unsupported", 0);
  }
}

/** One WebAuthn ceremony; dismissing the prompt is `cancelled`, not an error. */
async function ceremony(what: string, run: () => Promise<Credential | null>): Promise<Result<PublicKeyCredential>> {
  if (!supported()) return fail("unsupported", 0);
  try {
    const cred = await run();
    if (!cred) {
      console.error(`auth: ${what}: the browser returned no credential`);
      return fail("bad_response", 0);
    }
    return ok(cred as PublicKeyCredential);
  } catch (e) {
    if (e instanceof DOMException && (e.name === "NotAllowedError" || e.name === "AbortError")) {
      console.warn(`auth: ${what}: ${e.name} (prompt dismissed or timed out)`);
      return fail("cancelled", 0);
    }
    console.error(`auth: ${what} failed`, e);
    return fail("bad_response", 0);
  }
}

/** An assertion for a fresh login challenge, with its fields under `prefix`. */
async function passkeyAssertion(api: Api, prefix: string, token: string | undefined): Promise<Result<Body>> {
  if (!supported()) return fail("unsupported", 0);
  const opts = await api.call<{ publicKey: unknown }>(
    "POST",
    "/api/auth/passkey/login/options",
    token ? { turnstile_token: token } : undefined,
  );
  if (!opts.ok) return failed(opts.error);
  const cred = await ceremony("passkey prompt", () =>
    navigator.credentials.get({ publicKey: getOptions(opts.value.publicKey) }),
  );
  if (!cred.ok) return failed(cred.error);
  return ok(assertion(cred.value, prefix));
}

export function signIn(api: Api, login: string, password: string, token?: string): Promise<Result<{ user: Me }>> {
  return guard("sign-in", async () => {
    const pw = await stretched(api, login, password);
    if (!pw.ok) return failed(pw.error);
    return api.call<{ user: Me }>("POST", "/api/auth/login", withToken({ login, password: pw.value }, token));
  });
}

/**
 * Passkey sign-in. A Turnstile token goes to options only: the challenge it returns carries
 * the proof to verify (issue #63), because a token is single use.
 */
export function signInPasskey(api: Api, token?: string): Promise<Result<{ user: Me }>> {
  return guard("passkey sign-in", async () => {
    const a = await passkeyAssertion(api, "", token);
    if (!a.ok) return failed(a.error);
    return api.call<{ user: Me }>("POST", "/api/auth/passkey/login/verify", a.value);
  });
}

function newAccount(what: string, path: string, api: Api, f: NewAccount, token: string | undefined) {
  return guard(what, async (): Promise<Result<{ user: Me }>> => {
    const pw = await stretched(api, f.login, f.password);
    if (!pw.ok) return failed(pw.error);
    const body = { code: f.code, login: f.login, slug: f.slug, display_name: f.display_name, password: pw.value };
    return api.call<{ user: Me }>("POST", path, withToken(body, token));
  });
}

/** The first owner, with the code `blog setup` printed. */
export const setup = (api: Api, f: NewAccount, token?: string): Promise<Result<{ user: Me }>> =>
  newAccount("setup", "/api/auth/setup", api, f, token);

export const acceptInvite = (api: Api, f: NewAccount, token?: string): Promise<Result<{ user: Me }>> =>
  newAccount("invite", "/api/auth/invite/accept", api, f, token);

/** An owner's new password (and maybe a new login), with the code `blog admin reset` printed. */
export function redeemReset(api: Api, f: { code: string; login: string; password: string }, token?: string): Promise<Result<{ user: Me }>> {
  return guard("owner reset", async () => {
    const pw = await stretched(api, f.login, f.password);
    if (!pw.ok) return failed(pw.error);
    return api.call<{ user: Me }>("POST", "/api/auth/reset", withToken({ code: f.code, login: f.login, password: pw.value }, token));
  });
}

/**
 * Ends the session, then deletes `user`'s drafts in this browser (a shared computer keeps
 * none). A failed sign-out keeps them: the writer is still signed in.
 */
export function signOut(api: Api, user: number): Promise<Result<void>> {
  return guard("sign-out", async () => {
    const r = await api.call<void>("POST", "/api/auth/logout");
    if (r.ok) await clearDrafts(user);
    return r;
  });
}

async function reauthFields(api: Api, how: Reauth): Promise<Result<Body>> {
  if (how === "passkey") return passkeyAssertion(api, "reauth_", undefined);
  const pw = await stretched(api, how.login, how.password);
  return pw.ok ? ok({ current_password: pw.value }) : failed(pw.error);
}

/**
 * Adds a passkey. Proof first: if the writer drops out of it, no key has been made on the
 * device yet (a key made but never registered would sit there unusable).
 */
export function addPasskey(api: Api, name: string, how: Reauth, token?: string): Promise<Result<{ passkey: Passkey }>> {
  return guard("passkey add", async () => {
    if (!supported()) return fail("unsupported", 0);
    const proof = await reauthFields(api, how);
    if (!proof.ok) return failed(proof.error);
    const opts = await api.call<{ publicKey: unknown }>("POST", "/api/auth/passkey/register/options");
    if (!opts.ok) return failed(opts.error);
    const cred = await ceremony("passkey creation", () =>
      navigator.credentials.create({ publicKey: createOptions(opts.value.publicKey) }),
    );
    if (!cred.ok) return failed(cred.error);
    // The proof's request carries the Turnstile token when wrong proofs have asked for one.
    return api.call<{ passkey: Passkey }>(
      "POST",
      "/api/auth/passkey/register/verify",
      withToken({ name, ...attestation(cred.value), ...proof.value }, token),
    );
  });
}

export function removePasskey(api: Api, id: number, how: Reauth, token?: string): Promise<Result<void>> {
  return guard("passkey remove", async () => {
    const proof = await reauthFields(api, how);
    if (!proof.ok) return failed(proof.error);
    return api.call<void>("DELETE", `/api/auth/passkey/${id}`, withToken(proof.value, token));
  });
}

export function listPasskeys(api: Api): Promise<Result<Passkey[]>> {
  return guard("passkey list", async () => {
    const r = await api.call<{ passkeys: Passkey[] }>("GET", "/api/auth/passkey");
    return r.ok ? ok(r.value.passkeys) : failed(r.error);
  });
}
