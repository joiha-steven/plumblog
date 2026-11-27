/**
 * The same interface as client.ts over web/admin/fixtures, for building screens before their
 * API exists (`scripts/admin-dev`). Query flags force each state for review and screenshots:
 * `?mock=loading|error|offline|empty`, `?role=writer`. The auth endpoints answer from
 * `?auth=`: `out` (signed out), `wrong|locked|turnstile|origin|setup_invalid|invite_invalid|
 * offline|cancelled` force that failure (`turnstile_hold`: like `turnstile`, then a call with a
 * token never answers, to hold the busy state for a screenshot); otherwise a call succeeds after a short wait.
 */

import me from "../../fixtures/me.json";
import posts from "../../fixtures/posts.json";
import longPost from "../../fixtures/post-1.txt";
import { type Api, type Me, type Method, type PostDraft, type PostSummary, type Result, fail } from "./client";

/** Cloudflare's public always-pass Turnstile key: the real widget works in dev. */
const TEST_SITE_KEY = "1x00000000000000000000AA";
/** Set by a mock sign-in, so `?auth=out` (still in the page's own address) lets the writer back in. */
const SIGNED_IN = "pb.mock.signed-in";

type Body = Record<string, string | boolean> | undefined;

/** The mock's auth routes; the failure each `?auth=` value forces is the one the real route gives. */
async function auth<T>(flag: string | null, method: Method, path: string, body: Body): Promise<Result<T>> {
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
  console.warn(`mock: ${method} ${path} is not served`);
  return fail("bad_response", 501);
}

export function mockApi(params: URLSearchParams): Api {
  const mode = params.get("mock");
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
    call: <T>(method: Method, path: string, body?: Body) => auth<T>(params.get("auth"), method, path, body),
  };
}
