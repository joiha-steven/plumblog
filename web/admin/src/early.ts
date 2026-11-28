/**
 * GETs a screen needs, started at boot beside `me` instead of after the screen's chunk has
 * loaded (the account page's lists: Lighthouse mobile LCP). Each answer serves one read.
 */
import type { Api, Result } from "./api/client";

const started = new Map<string, Promise<Result<unknown>>>();

export function startEarly(api: Api, paths: readonly string[]): void {
  for (const p of paths) started.set(p, api.call("GET", p));
}

/** The early answer for `path` if one was started, else a new GET. */
export function getOnce<T>(api: Api, path: string): Promise<Result<T>> {
  const hit = started.get(path) as Promise<Result<T>> | undefined;
  started.delete(path);
  return hit ?? api.call<T>("GET", path);
}
