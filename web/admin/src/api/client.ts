/**
 * The one way the admin talks to the Worker. Every call resolves to a Result, never throws,
 * and logs what failed, so no error is silent and every screen can show a plain message.
 */

import { ERROR_CODES, type ErrorCode, type Role } from "./model.gen";

/**
 * Failures the server cannot name: no answer at all, or an answer that is not ours; the
 * writer dismissed a passkey prompt (`cancelled`), or this browser has no passkeys
 * (`unsupported`).
 */
export type ClientCode = "network" | "bad_response" | "cancelled" | "unsupported";

export interface ApiError {
  code: ErrorCode | ClientCode;
  /** HTTP status; 0 when there was no response. */
  status: number;
}

export type Result<T> = { ok: true; value: T } | { ok: false; error: ApiError };

/**
 * `GET /api/auth/me` (crates/worker/src/api/auth.rs `user_json`). The Worker writes this JSON
 * by hand today; it moves into the generated types when the API types live in crates/model.
 */
export interface Me {
  id: number;
  login: string;
  slug: string;
  display_name: string;
  bio: string;
  role: Role;
}

/**
 * A post as the editor and lists use it. The posts API (#10) defines the real shape; until
 * then only the mock serves posts.
 */
export interface PostDraft {
  id: number;
  title: string;
  body: string;
  lang: string;
  status: "draft" | "scheduled" | "published";
  /** Unix milliseconds of the last server save. */
  updated_at: number;
}

export interface PostSummary {
  id: number;
  title: string;
  excerpt: string;
  lang: string;
  status: PostDraft["status"];
  updated_at: number;
}

export interface Api {
  me(): Promise<Result<Me>>;
  posts(): Promise<Result<PostSummary[]>>;
  post(id: number): Promise<Result<PostDraft>>;
  /**
   * Where the editor posts `title`, `body` and `lang` as a form to get the page the draft
   * would publish as, rendered by the Worker with the site's theme; undefined while the Worker
   * has no preview route for this build.
   */
  previewUrl: string | undefined;
  /**
   * Any API route: a flat JSON body (the Worker refuses unknown keys), a Result back. A 204
   * or empty answer resolves to `undefined`.
   */
  call<T>(method: Method, path: string, body?: Record<string, string | boolean>): Promise<Result<T>>;
}

export type Method = "GET" | "POST" | "PATCH" | "DELETE";

/**
 * Set by the sign-in screens: runs when a call outside `/api/auth/` answers 401
 * `auth.unauthenticated`, so an expired session sends the writer to sign in again instead of
 * failing screen by screen. Auth routes answer 401 as part of their own flow.
 */
export const session = { expired: (): void => undefined };

export const fail = <T>(code: ApiError["code"], status: number): Result<T> => ({
  ok: false,
  error: { code, status },
});

const isCode = (c: unknown): c is ErrorCode =>
  typeof c === "string" && (ERROR_CODES as readonly string[]).includes(c);

async function request<T>(
  method: Method,
  path: string,
  body: Record<string, string | boolean> | undefined,
  emptyOk: boolean,
): Promise<Result<T>> {
  const headers: Record<string, string> = { accept: "application/json" };
  const init: RequestInit = { method, credentials: "same-origin", headers };
  if (body !== undefined) {
    headers["content-type"] = "application/json";
    init.body = JSON.stringify(body);
  }
  let res: Response;
  try {
    res = await fetch(path, init);
  } catch (e) {
    console.error(`api: ${method} ${path}: no response`, e);
    return fail("network", 0);
  }
  let parsed: unknown;
  try {
    const text = await res.text();
    if (text.trim() === "" && emptyOk && res.ok) return { ok: true, value: undefined as T };
    parsed = JSON.parse(text);
  } catch (e) {
    console.error(`api: ${method} ${path}: ${res.status}, body is not JSON`, e);
    return fail("bad_response", res.status);
  }
  if (!res.ok) {
    const code = (parsed as { error?: unknown } | null)?.error;
    console.warn(`api: ${method} ${path}: ${res.status} ${String(code)}`);
    if (res.status === 401 && code === "auth.unauthenticated" && !path.startsWith("/api/auth/")) {
      try {
        session.expired();
      } catch (e) {
        console.error("api: session.expired hook failed", e);
      }
    }
    return fail(isCode(code) ? code : "bad_response", res.status);
  }
  return { ok: true, value: parsed as T };
}

const getJson = <T>(path: string): Promise<Result<T>> => request<T>("GET", path, undefined, false);

export function httpApi(): Api {
  return {
    async me() {
      const r = await getJson<{ user: Me }>("/api/auth/me");
      return r.ok ? { ok: true, value: r.value.user } : r;
    },
    // The posts API is issue #10; these answer as the Worker does today.
    posts: () => getJson<PostSummary[]>("/api/posts"),
    post: (id) => getJson<PostDraft>(`/api/posts/${id}`),
    previewUrl: undefined,
    call: (method, path, body) => request(method, path, body, true),
  };
}
