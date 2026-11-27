/**
 * The one way the admin talks to the Worker. Every call resolves to a Result, never throws,
 * and logs what failed, so no error is silent and every screen can show a plain message.
 */

import { ERROR_CODES, type ErrorCode, type Role } from "./model.gen";

/** Failures the server cannot name: no answer at all, or an answer that is not ours. */
export type ClientCode = "network" | "bad_response";

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

export interface Api {
  me(): Promise<Result<Me>>;
}

export const fail = <T>(code: ApiError["code"], status: number): Result<T> => ({
  ok: false,
  error: { code, status },
});

const isCode = (c: unknown): c is ErrorCode =>
  typeof c === "string" && (ERROR_CODES as readonly string[]).includes(c);

async function getJson<T>(path: string): Promise<Result<T>> {
  let res: Response;
  try {
    res = await fetch(path, { credentials: "same-origin", headers: { accept: "application/json" } });
  } catch (e) {
    console.error(`api: GET ${path}: no response`, e);
    return fail("network", 0);
  }
  let body: unknown;
  try {
    body = await res.json();
  } catch (e) {
    console.error(`api: GET ${path}: ${res.status}, body is not JSON`, e);
    return fail("bad_response", res.status);
  }
  if (!res.ok) {
    const code = (body as { error?: unknown } | null)?.error;
    console.warn(`api: GET ${path}: ${res.status} ${String(code)}`);
    return fail(isCode(code) ? code : "bad_response", res.status);
  }
  return { ok: true, value: body as T };
}

export function httpApi(): Api {
  return {
    async me() {
      const r = await getJson<{ user: Me }>("/api/auth/me");
      return r.ok ? { ok: true, value: r.value.user } : r;
    },
  };
}
