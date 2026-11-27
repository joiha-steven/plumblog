/**
 * The one way the admin talks to the Worker. Every call resolves to a Result, never throws,
 * and logs what failed, so no error is silent and every screen can show a plain message.
 */

import {
  ERROR_CODES,
  type ErrorCode,
  type Post,
  type PostCreateInput,
  type PostListQuery,
  type PostPage,
  type PostUpdateInput,
  type Role,
} from "./model.gen";

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
  /** The field a 409 `store.conflict` names: `expected_version`, `slug` or `lang`. */
  field?: string;
  /** The server's current version, present on a stale `expected_version`. */
  version?: number;
}

/** Any JSON value; request bodies are built from it. */
export type Json = string | number | boolean | null | Json[] | { [key: string]: Json };
export type JsonBody = { [key: string]: Json };

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
 * A stored picture, as an upload returns it. The media API (#10) defines the real shape;
 * width and height let the editor reserve the picture's box before it loads.
 */
export interface Media {
  id: number;
  url: string;
  width: number;
  height: number;
}

/** Called with the share of an upload sent so far, 0 to 1. */
export type Progress = (sent: number) => void;

export interface Api {
  me(): Promise<Result<Me>>;
  /** One page of summaries; `next_cursor` goes back in as `cursor` for the next. */
  posts(q: PostListQuery): Promise<Result<PostPage>>;
  post(id: number): Promise<Result<Post>>;
  /** 201 with the saved post. A taken slug or lang is 409 `store.conflict` with `field`. */
  createPost(input: PostCreateInput): Promise<Result<Post>>;
  /** Conditional on `expected_version`; a stale one is 409 `store.conflict` with `version`. */
  updatePost(id: number, input: PostUpdateInput): Promise<Result<Post>>;
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
  call<T>(method: Method, path: string, body?: JsonBody): Promise<Result<T>>;
  /** Stores a picture; `progress` follows the upload. */
  upload(file: File, progress: Progress, signal?: AbortSignal): Promise<Result<Media>>;
}

export type Method = "GET" | "POST" | "PATCH" | "DELETE";

/**
 * Set by the sign-in screens: runs when a call outside `/api/auth/` answers 401
 * `auth.unauthenticated`, so an expired session sends the writer to sign in again instead of
 * failing screen by screen. Auth routes answer 401 as part of their own flow.
 */
export const session: { expired: () => void; flush: (() => Promise<unknown>) | undefined } = {
  expired: () => undefined,
  /** Set by an open editor: saves its last keys on this device. Run before the page leaves. */
  flush: undefined,
};

/**
 * Runs the editor's registered flush, waiting at most `ms` (a stuck IndexedDB never keeps the
 * writer from signing in again). Never throws.
 */
export async function flushLocal(ms = 2000): Promise<void> {
  const f = session.flush;
  if (!f) return;
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    await Promise.race([f(), new Promise((ok) => (timer = setTimeout(ok, ms)))]);
  } catch (e) {
    console.error("api: flushing the local draft before sign-in failed", e);
  } finally {
    clearTimeout(timer);
  }
}

/**
 * A request with no answer by then settles as `network`, so a hung one never stalls saving.
 * Not for creates: one that is slow but lands would be sent again as a second post, while a
 * hung create keeps the editor's in-flight guard (sync.ts). A slow PATCH that did land answers
 * the retry with a 409 the editor then resolves (#86).
 */
export const REQUEST_MS = 20_000;

export const fail = <T>(code: ApiError["code"], status: number, extra: Pick<ApiError, "field" | "version"> = {}): Result<T> => ({
  ok: false,
  error: { code, status, ...extra },
});

const isCode = (c: unknown): c is ErrorCode =>
  typeof c === "string" && (ERROR_CODES as readonly string[]).includes(c);

async function request<T>(
  method: Method,
  path: string,
  body: JsonBody | undefined,
  emptyOk: boolean,
  /** The bound, or 0 for none. */
  ms: number,
): Promise<Result<T>> {
  const stop = new AbortController();
  const timer = ms > 0 ? setTimeout(() => stop.abort(), ms) : undefined;
  try {
    return await answer<T>(method, path, body, emptyOk, stop.signal, ms);
  } finally {
    clearTimeout(timer);
  }
}

async function answer<T>(
  method: Method,
  path: string,
  body: JsonBody | undefined,
  emptyOk: boolean,
  signal: AbortSignal,
  ms: number,
): Promise<Result<T>> {
  const headers: Record<string, string> = { accept: "application/json" };
  const init: RequestInit = { method, credentials: "same-origin", headers, signal };
  if (body !== undefined) {
    headers["content-type"] = "application/json";
    init.body = JSON.stringify(body);
  }
  let res: Response;
  try {
    res = await fetch(path, init);
  } catch (e) {
    console.error(`api: ${method} ${path}: ${signal.aborted ? `no answer within ${ms} ms` : "no response"}`, e);
    return fail("network", 0);
  }
  let parsed: unknown;
  try {
    const text = await res.text();
    if (text.trim() === "" && emptyOk && res.ok) return { ok: true, value: undefined as T };
    parsed = JSON.parse(text);
  } catch (e) {
    // The body stalled past the bound: the same as no answer.
    if (signal.aborted) {
      console.error(`api: ${method} ${path}: ${res.status}, body not read within ${ms} ms`, e);
      return fail("network", 0);
    }
    console.error(`api: ${method} ${path}: ${res.status}, body is not JSON`, e);
    return fail("bad_response", res.status);
  }
  if (!res.ok) {
    const err = parsed as { error?: unknown; field?: unknown; version?: unknown } | null;
    const code = err?.error;
    console.warn(`api: ${method} ${path}: ${res.status} ${String(code)}`);
    if (res.status === 401 && code === "auth.unauthenticated" && !path.startsWith("/api/auth/")) {
      try {
        session.expired();
      } catch (e) {
        console.error("api: session.expired hook failed", e);
      }
    }
    const extra: Pick<ApiError, "field" | "version"> = {};
    if (typeof err?.field === "string") extra.field = err.field;
    if (typeof err?.version === "number") extra.version = err.version;
    return fail(isCode(code) ? code : "bad_response", res.status, extra);
  }
  return { ok: true, value: parsed as T };
}

/** A typed input as a JSON body: keys left undefined are not sent. */
function bodyOf(input: PostCreateInput | PostUpdateInput): JsonBody {
  const out: JsonBody = {};
  for (const [k, v] of Object.entries(input) as [string, Json | undefined][]) if (v !== undefined) out[k] = v;
  return out;
}


/** `POST /api/media` with the file as multipart `file`. XHR, not fetch: fetch reports no upload
 * progress. The posts and media API is issue #10; this follows its draft. */
function upload(file: File, progress: Progress, signal?: AbortSignal): Promise<Result<Media>> {
  const path = "/api/media";
  return new Promise((resolve) => {
    const x = new XMLHttpRequest();
    x.open("POST", path);
    x.responseType = "json";
    x.setRequestHeader("accept", "application/json");
    x.upload.onprogress = (e) => {
      if (e.lengthComputable) progress(e.loaded / e.total);
    };
    x.onerror = () => {
      console.error(`api: POST ${path}: no response`);
      resolve(fail("network", 0));
    };
    x.onload = () => {
      const body: unknown = x.response;
      if (x.status >= 200 && x.status < 300 && body && typeof (body as Media).url === "string") {
        resolve({ ok: true, value: body as Media });
        return;
      }
      const code = (body as { error?: unknown } | null)?.error;
      console.warn(`api: POST ${path}: ${x.status} ${String(code)}`);
      resolve(fail(isCode(code) ? code : "bad_response", x.status));
    };
    const form = new FormData();
    form.append("file", file);
    if (signal?.aborted) return resolve(fail("cancelled", 0));
    signal?.addEventListener(
      "abort",
      () => {
        x.abort();
        resolve(fail("cancelled", 0));
      },
      { once: true },
    );
    x.send(form);
  });
}

/** `ms`: the request bound (REQUEST_MS); tests pass a short one. */
export function httpApi(ms = REQUEST_MS): Api {
  const getJson = <T>(path: string): Promise<Result<T>> => request<T>("GET", path, undefined, false, ms);
  return {
    async me() {
      const r = await getJson<{ user: Me }>("/api/auth/me");
      return r.ok ? { ok: true, value: r.value.user } : r;
    },
    posts(q) {
      const qs = new URLSearchParams();
      for (const [k, v] of Object.entries(q)) if (v !== undefined) qs.set(k, String(v));
      const s = qs.toString();
      return getJson<PostPage>(`/api/v1/posts${s ? `?${s}` : ""}`);
    },
    post: (id) => getJson<Post>(`/api/v1/posts/${id}`),
    createPost: (input) => request<Post>("POST", "/api/v1/posts", bodyOf(input), false, 0),
    updatePost: (id, input) => request<Post>("PATCH", `/api/v1/posts/${id}`, bodyOf(input), false, ms),
    previewUrl: undefined,
    call: (method, path, body) => request(method, path, body, true, ms),
    upload,
  };
}
