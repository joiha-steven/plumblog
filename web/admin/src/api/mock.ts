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
 *
 * Publishing: a post update with `status` follows the server's rules (a draft carries no time, a
 * scheduled time must be in the future); `?slug=taken` makes the first publish fail once with a
 * taken slug; trash and restore work on the same list.
 *
 * Posts live in memory, from fixtures/posts.json, and reset on page load; the long post (id 1)
 * keeps its body in post-1.txt, so its `markdown` in the JSON is empty and is filled in here.
 * They answer as `/api/v1/posts` does: paged lists (opaque cursor), `post.not_found`, 409
 * `store.conflict` on a stale `expected_version` (`?conflict=1` makes the first update
 * conflict, as if another device saved first) or a taken slug. Rendering is not simulated:
 * `html` stays as the fixture has it, and the only warning computed is `markdown.math_invalid`.
 */

import me from "../../fixtures/me.json";
import posts from "../../fixtures/posts.json";
import longPost from "../../fixtures/post-1.txt";
import { type Api, type JsonBody, type Me, type Media, type Method, type Result, fail } from "./client";
import type { Post, PostCreateInput, PostPage, PostUpdateInput, RenderWarning } from "./model.gen";

/** Cloudflare's public always-pass Turnstile key: the real widget works in dev. */
const TEST_SITE_KEY = "1x00000000000000000000AA";
/** Set by a mock sign-in, so `?auth=out` (still in the page's own address) lets the writer back in. */
const SIGNED_IN = "pb.mock.signed-in";

type Body = JsonBody | undefined;

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

const PAGE = 20;

/** An approximation: the server warns on TeX that does not render (`$\notacommand$`); the mock
 * only knows a `$$` that never closes, enough to show a warning. */
const warningsOf = (markdown: string): RenderWarning[] =>
  (markdown.match(/\$\$/g)?.length ?? 0) % 2 === 1 ? ["markdown.math_invalid"] : [];

const wordsOf = (markdown: string): number => markdown.split(/\s+/).filter(Boolean).length;

export function mockApi(params: URLSearchParams): Api {
  const mode = params.get("mock");
  const up = params.get("upload");
  let mediaId = 100;
  const role = params.get("role") === "writer" ? "writer" : "owner";
  const store: Post[] = (posts as Post[]).map((p) => structuredClone(p));
  const first = store.find((p) => p.id === 1);
  if (first) first.markdown = longPost;
  /** The first update fails once with the server's newer version when `?conflict=1`. */
  let conflictPending = params.get("conflict") === "1";
  /** The first publish fails once with a taken slug when `?slug=taken`. */
  let slugPending = params.get("slug") === "taken";
  const down = <T>(): Result<T> | undefined => {
    if (mode === "error") return fail("internal", 500);
    if (mode === "offline") return fail("network", 0);
    return undefined;
  };
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
    async posts(q) {
      await settle();
      const bad = down<PostPage>();
      if (bad) return bad;
      if (q.order !== undefined && q.order !== "updated") return fail("input.invalid", 400, { field: "order" });
      let offset = 0;
      if (q.cursor !== undefined) {
        offset = Number(atob(q.cursor));
        if (!Number.isInteger(offset) || offset < 0) return fail("input.invalid", 400, { field: "cursor" });
      }
      const limit = q.limit ?? PAGE;
      const all = mode === "empty" ? [] : store.filter((p) => (q.kind === undefined || p.kind === q.kind) && (q.status === undefined || p.status === q.status) && (q.lang === undefined || p.lang === q.lang));
      all.sort((a, b) => b.updated_at - a.updated_at || b.id - a.id);
      const page = all.slice(offset, offset + limit).map((p) => {
        const { markdown: _m, html: _h, media_ids: _i, warnings: _w, ...summary } = p;
        return summary;
      });
      const next = offset + limit < all.length ? btoa(String(offset + limit)) : null;
      return { ok: true, value: { posts: page, next_cursor: next } };
    },
    async post(id) {
      await settle();
      if (mode === "offline") return fail("network", 0);
      const p = store.find((x) => x.id === id);
      return p ? { ok: true, value: structuredClone(p) } : fail("post.not_found", 404);
    },
    async createPost(input: PostCreateInput) {
      await settle();
      const bad = down<Post>();
      if (bad) return bad;
      if (store.some((p) => p.slug === input.slug && p.lang === input.lang)) return fail("store.conflict", 409, { field: "slug" });
      const now = Date.now();
      const id = Math.max(0, ...store.map((p) => p.id)) + 1;
      const status = input.status ?? "draft";
      const words = wordsOf(input.markdown);
      const post: Post = {
        id,
        kind: input.kind ?? "post",
        translation_group: input.translation_of ?? id,
        lang: input.lang,
        slug: input.slug,
        status,
        trashed_from: null,
        published_at: input.published_at ?? (status === "published" ? now : null),
        author_id: 1,
        title: input.title,
        excerpt: input.excerpt ?? "",
        word_count: words,
        reading_minutes: Math.max(1, Math.round(words / 200)),
        pinned: input.pinned ?? false,
        comments_enabled: input.comments_enabled ?? true,
        cover_media_id: input.cover_media_id ?? null,
        series_id: input.series_id ?? null,
        series_position: input.series_position ?? null,
        created_at: now,
        updated_at: now,
        version: 1,
        tags: input.tags ?? [],
        markdown: input.markdown,
        html: "",
        media_ids: [],
        warnings: warningsOf(input.markdown),
      };
      store.push(post);
      return { ok: true, value: structuredClone(post) };
    },
    async updatePost(id, input: PostUpdateInput) {
      await settle();
      const bad = down<Post>();
      if (bad) return bad;
      const p = store.find((x) => x.id === id);
      if (!p) return fail("post.not_found", 404);
      if (conflictPending) {
        conflictPending = false;
        p.version += 1;
        p.updated_at = Date.now();
      }
      if (input.expected_version !== p.version) return fail("store.conflict", 409, { field: "expected_version", version: p.version });
      const { expected_version: _v, ...fields } = input;
      if (fields.status === "published" && p.status !== "published" && slugPending) {
        slugPending = false;
        return fail("store.conflict", 409, { field: "slug" });
      }
      if (fields.status === "scheduled" && (fields.published_at ?? 0) <= Date.now()) return fail("input.invalid", 400, { field: "published_at" });
      if (fields.status === "draft" && fields.published_at !== null && fields.published_at !== undefined) return fail("input.invalid", 400, { field: "published_at" });
      if (fields.slug !== undefined && store.some((x) => x.id !== id && x.slug === fields.slug && x.lang === (fields.lang ?? p.lang))) {
        return fail("store.conflict", 409, { field: "slug" });
      }
      for (const k of Object.keys(fields) as (keyof typeof fields)[]) if (fields[k] === undefined) delete fields[k];
      Object.assign(p, fields);
      if (fields.markdown !== undefined) {
        p.word_count = wordsOf(p.markdown);
        p.reading_minutes = Math.max(1, Math.round(p.word_count / 200));
        p.warnings = warningsOf(p.markdown);
      }
      p.version += 1;
      p.updated_at = Date.now();
      if (p.status === "published" && p.published_at === null) p.published_at = p.updated_at;
      return { ok: true, value: structuredClone(p) };
    },
    // The dev Worker's real renderer (crates/worker/src/contract.rs, test builds only).
    previewUrl: "/__preview",
    async call<T>(method: Method, path: string, body?: Body): Promise<Result<T>> {
      const m = /^\/api\/v1\/posts\/(\d+)\/(trash|restore)$/.exec(path);
      if (method !== "POST" || !m) return auth<T>(params, method, path, body);
      await settle();
      const bad = down<T>();
      if (bad) return bad;
      const p = store.find((x) => x.id === Number(m[1]));
      if (!p) return fail("post.not_found", 404);
      if (m[2] === "trash") {
        p.trashed_from = p.status === "trashed" ? p.trashed_from : p.status;
        p.status = "trashed";
      } else p.status = p.trashed_from ?? "draft";
      p.version += 1;
      p.updated_at = Date.now();
      return { ok: true, value: structuredClone(p) as T };
    },
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
