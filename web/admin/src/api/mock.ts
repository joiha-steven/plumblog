/**
 * The same interface as client.ts over web/admin/fixtures, for building screens before their
 * API exists (`scripts/admin-dev`). Query flags force each state for review and screenshots:
 * `?mock=loading|error|offline|empty`, `?role=writer`. The auth endpoints answer from
 * `?auth=`: `out` (signed out), `wrong|locked|turnstile|origin|setup_invalid|invite_invalid|
 * offline|cancelled` force that failure (`turnstile_hold`: like `turnstile`, then a call with a
 * token never answers, to hold the busy state for a screenshot); otherwise a call succeeds after a short wait.
 * Adding or removing a passkey (account screen): `reauth_wrong|reauth|conflict|origin|unsupported|turnstile`
 * fail as the server does (`turnstile`: removing only; adding sends its token after the browser prompt), before any browser prompt (no real WebAuthn runs in shots); `?passkeys=0`
 * starts with none. Removing works on a list kept in memory. `?upload=fail` makes uploads fail
 * halfway and `?upload=slow` takes 20 s.
 */

import me from "../../fixtures/me.json";
import posts from "../../fixtures/posts.json";
import longPost from "../../fixtures/post-1.txt";
import { type Api, type Me, type Media, type Method, type PostDraft, type PostSummary, type Result, fail } from "./client";

/** Cloudflare's public always-pass Turnstile key: the real widget works in dev. */
const TEST_SITE_KEY = "1x00000000000000000000AA";
/** Set by a mock sign-in, so `?auth=out` (still in the page's own address) lets the writer back in. */
const SIGNED_IN = "pb.mock.signed-in";

type Body = Record<string, string | boolean> | undefined;

interface Key {
  id: number;
  name: string;
  created_at: number;
  last_used_at: number | null;
}
/** The mock's passkeys; reset by a page load, like the rest of the mock. */
let keys: Key[] | undefined;
const keyList = (params: URLSearchParams): Key[] =>
  (keys ??=
    params.get("passkeys") === "0"
      ? []
      : [
          { id: 1, name: "MacBook Pro", created_at: Date.UTC(2026, 8, 3), last_used_at: Date.UTC(2026, 10, 24) },
          { id: 2, name: "iPhone 16", created_at: Date.UTC(2026, 10, 12), last_used_at: null },
        ]);

/** The mock's auth routes; the failure each `?auth=` value forces is the one the real route gives. */
async function auth<T>(params: URLSearchParams, method: Method, path: string, body: Body): Promise<Result<T>> {
  const flag = params.get("auth");
  await new Promise((r) => setTimeout(r, 400));
  const done = (value: unknown = {}): Result<T> => ({ ok: true, value: value as T });
  if (flag === "offline") return fail("network", 0);
  if (path === "/api/auth/params") {
    return done({ site_id: "mock", stretch_iterations: 600000, turnstile_site_key: TEST_SITE_KEY });
  }
  const user = { user: me.user };
  const signedIn = () => {
    sessionStorage.setItem(SIGNED_IN, "1");
    return done(user);
  };
  if (path === "/api/auth/login") {
    if (flag === "locked") return fail("auth.locked", 429);
    if (flag === "wrong") return fail("auth.invalid_credentials", 401);
    if (flag === "turnstile" && !body?.["turnstile_token"]) return fail("auth.turnstile_required", 403);
    if (flag === "turnstile_hold") {
      if (!body?.["turnstile_token"]) return fail("auth.turnstile_required", 403);
      return new Promise(() => undefined);
    }
    return signedIn();
  }
  if (path === "/api/auth/setup") {
    return flag === "setup_invalid" ? fail("setup.invalid", 403) : signedIn();
  }
  if (path === "/api/auth/invite/accept") {
    return flag === "invite_invalid" ? fail("invite.invalid", 410) : signedIn();
  }
  if (path === "/api/auth/passkey/login/options") {
    if (flag === "origin") return fail("settings.origin_missing", 503);
    if (flag === "cancelled") return fail("cancelled", 0);
    if (flag === "turnstile" && !body?.["turnstile_token"]) return fail("auth.turnstile_required", 403);
    // The real browser prompt follows; screenshots use the failures above instead.
    return done({ publicKey: { challenge: "AAAAAAAAAAAAAAAAAAAAAA", rpId: location.hostname, timeout: 60000, userVerification: "preferred" } });
  }
  if (path === "/api/auth/passkey/login/verify") return signedIn();
  if (path === "/api/auth/logout") return done();
  if (path === "/api/auth/me") return done({ ...user, passkeys: keyList(params) });
  if (path === "/api/auth/passkey" && method === "GET") return done({ passkeys: keyList(params) });
  if (path === "/api/auth/passkey/register/options") {
    // The real server checks the proof when the key comes back; the mock fails here instead, so no prompt opens.
    if (flag === "origin") return fail("settings.origin_missing", 503);
    if (flag === "unsupported") return fail("unsupported", 0);
    if (flag === "reauth_wrong") return fail("auth.invalid_credentials", 401);
    if (flag === "reauth") return fail("auth.reauth_required", 403);
    return done({ publicKey: { challenge: "AAAAAAAAAAAAAAAAAAAAAA", rp: { id: location.hostname, name: "mock" }, user: { id: "AQ", name: "lan", displayName: "Lan" }, pubKeyCredParams: [{ type: "public-key", alg: -7 }], timeout: 60000 } });
  }
  if (path === "/api/auth/passkey/register/verify") {
    const list = keyList(params);
    const key = { id: Math.max(0, ...list.map((k) => k.id)) + 1, name: String(body?.["name"] ?? ""), created_at: Date.now(), last_used_at: null };
    list.push(key);
    return done({ passkey: key });
  }
  const del = /^\/api\/auth\/passkey\/(\d+)$/.exec(path);
  if (del && method === "DELETE") {
    if (flag === "reauth_wrong") return fail("auth.invalid_credentials", 401);
    if (flag === "reauth") return fail("auth.reauth_required", 403);
    if (flag === "conflict") return fail("store.conflict", 409);
    if (flag === "turnstile" && !body?.["turnstile_token"]) return fail("auth.turnstile_required", 403);
    keys = keyList(params).filter((k) => k.id !== Number(del[1]));
    return done();
  }
  console.warn(`mock: ${method} ${path} is not served`);
  return fail("bad_response", 501);
}

export function mockApi(params: URLSearchParams): Api {
  const mode = params.get("mock");
  const up = params.get("upload");
  let mediaId = 100;
  const role = params.get("role") === "writer" ? "writer" : "owner";
  const settle = async () => {
    if (mode === "loading") await new Promise(() => undefined);
    await new Promise((r) => setTimeout(r, 150));
  };
  return {
    async me() {
      await settle();
      if (params.get("auth") === "out" && !sessionStorage.getItem(SIGNED_IN)) return fail("auth.unauthenticated", 401);
      if (mode === "error") return fail("internal", 500);
      if (mode === "offline") return fail("network", 0);
      const user: Me = { ...(me.user as Me), role };
      return { ok: true, value: user };
    },
    async posts() {
      await settle();
      if (mode === "error") return fail("internal", 500);
      if (mode === "offline") return fail("network", 0);
      return { ok: true, value: mode === "empty" ? [] : (posts as PostSummary[]) };
    },
    async post(id) {
      await settle();
      if (mode === "offline") return fail("network", 0);
      const s = (posts as PostSummary[]).find((p) => p.id === id);
      if (!s) return fail("post.not_found", 404);
      const body = id === 1 ? longPost : `${s.excerpt}\n`;
      const post: PostDraft = { id: s.id, title: s.title, body, lang: s.lang, status: s.status, updated_at: s.updated_at };
      return { ok: true, value: post };
    },
    // The dev Worker's real renderer (crates/worker/src/contract.rs, test builds only).
    previewUrl: "/__preview",
    call: <T>(method: Method, path: string, body?: Body) => auth<T>(params, method, path, body),
    // Sends nothing: progress over a second or two, then the picture from this browser.
    async upload(file, progress, signal) {
      if (mode === "offline") return fail("network", 0);
      const steps = 12;
      for (let i = 1; i <= steps; i++) {
        await new Promise((r) => {
          const id = setTimeout(r, up === "slow" ? 1700 : 120);
          signal?.addEventListener("abort", () => (clearTimeout(id), r(undefined)), { once: true });
        });
        if (signal?.aborted) return fail("cancelled", 0);
        if (up === "fail" && i > steps / 2) return fail("internal", 500);
        progress(i / steps);
      }
      const url = URL.createObjectURL(file);
      const size = await new Promise<[number, number]>((resolve) => {
        const img = new Image();
        img.onload = () => resolve([img.naturalWidth, img.naturalHeight]);
        img.onerror = () => resolve([0, 0]);
        img.src = url;
      });
      const media: Media = { id: ++mediaId, url, width: size[0], height: size[1] };
      return { ok: true, value: media };
    },
  };
}
