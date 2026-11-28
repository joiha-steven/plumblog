// Worker entry: answers /healthz and page-cache hits itself and loads the wasm Worker
// (./index.js) only for everything else. workers-rs's index.js instantiates the whole wasm at
// import time, which costs a fresh isolate 8-40 ms CPU (Free plan limit: 10 ms). Any doubt
// or failure here hands the request to wasm, which does its own lookup, rendering and logging.
// After a Cache API miss (it is per data centre) it reads the page wasm stored in R2. The key
// formats and the theme tag rule duplicate crates/worker pages/cache.rs and theme_runtime.rs;
// tools/worker-test/entry.test.mjs and r2pages.test.mjs pin them to the Rust side.
import { WorkerEntrypoint } from "cloudflare:workers";

// A failed import is not retried: ES modules cache a failed evaluation, so a retry would get
// the same error. Non-fast-path requests of this isolate fail (the runtime logs why) until
// Cloudflare replaces the isolate; /healthz and cache hits keep working meanwhile.
let app;
// Test builds only (PLUMBLOG_CONTRACT_TESTS): responses say whether index.js has been evaluated
// in this isolate, so entry.test.mjs can prove the fast paths never load wasm. The test build's
// index.js sets the flag itself (`scripts/worker-entry --mark-loaded`), so any way of loading it,
// a static import included, shows.
const load = () => (app ??= import("./index.js"));
const withLoadedMark = (res, env) => {
  if (globalThis.__plumblogWasmLoaded !== true || !env.PLUMBLOG_CONTRACT_TESTS) return res;
  const out = new Response(res.body, res);
  out.headers.set("x-plumblog-wasm", "loaded");
  return out;
};
const wasm = async () => (await load()).default;

const MARK = "x-plumblog-entry";

// What lib.rs routes to pages::serve (media::serve owns /media/) besides search (/search and
// /<lang>/search), which is never cached.
// First segments the router owns (pages::routes::RESERVED): never a language, so /tag/search is a page.
const OWNED = new Set(["page", "tag", "archive", "search", "theme", "plugin", "media", "admin", "api", "healthz"]);
const isLangSearch = (p) => {
  const m = /^\/([a-z]{2,3}(?:-[a-z0-9]{2,8})*)\/search$/.exec(p);
  return m !== null && !OWNED.has(m[1]);
};
const isReaderPath = (p) =>
  !(p.startsWith("/api/") || p === "/mcp" || p.startsWith("/media/") || p === "/admin" || p.startsWith("/admin/") || p === "/healthz" || p === "/search" || isLangSearch(p));

/** "" (embedded theme), "-t<16 hex>" (pushed theme with its tag binding), or null (ask wasm). */
function themeTag(env) {
  const name = env.PLUMBLOG_THEME;
  if (name === undefined || name === null) return "";
  if (typeof name !== "string") return null;
  if (name.trim() === "") return "";
  const tag = env.PLUMBLOG_THEME_TAG;
  return typeof tag === "string" && /^[0-9a-f]{16}$/.test(tag) ? `-t${tag}` : null;
}

function healthz(env) {
  // Same content-type as workers-rs's Response::ok; the JS default would differ in spelling.
  const headers = new Headers({ "content-type": "text/plain; charset=utf-8" });
  const id = env.CF_VERSION_METADATA?.id;
  if (typeof id === "string") headers.set("x-plumblog-version", id);
  headers.set(MARK, "js");
  return new Response("ok", { headers });
}

const notModified = (etag) => new Response(null, { status: 304, headers: { etag, [MARK]: "js" } });

/** The cached or R2-stored page (a 304 when its ETag matches), or null to let wasm handle it. */
async function cached(req, url, env, ctx) {
  const tag = themeTag(env);
  if (tag === null) return null;
  const row = await env.DB.prepare("SELECT content_version FROM site WHERE id = 1").first();
  const version = row?.content_version;
  if (!Number.isSafeInteger(version) || version < 0) throw new Error(`content_version is ${JSON.stringify(version)}`);
  const id = env.CF_VERSION_METADATA?.id;
  const page = `/v${version}${tag}${url.pathname}`;
  const cacheKey = `${url.origin}/__page${typeof id === "string" ? `/${id}` : ""}${page}`;
  const hit = await caches.default.match(cacheKey);
  if (!hit) return typeof id === "string" ? stored(req, env, ctx, `pages/${id}${page}`, cacheKey) : null;
  const etag = hit.headers.get("etag");
  if (etag !== null && req.headers.get("if-none-match") === etag) return notModified(etag);
  const headers = new Headers();
  for (const [k, v] of hit.headers) if (k !== "x-plumblog-cache" && k !== "server-timing") headers.set(k, v);
  headers.set("x-plumblog-cache", "hit");
  headers.set(MARK, "js");
  return new Response(hit.body, { status: hit.status, headers });
}

/**
 * The page from R2 (Rust's pages::cache::store_key), kept in this colo's cache too; null if absent.
 * A revalidation costs one head, not a get. Keys over R2's 1,024 bytes (Rust never stores them)
 * are not looked up: R2 would throw, and any long random path would log a warning.
 */
async function stored(req, env, ctx, key, cacheKey) {
  if (req.method !== "GET" || new TextEncoder().encode(key).length > 1024) return null;
  const inm = req.headers.get("if-none-match");
  if (inm !== null) {
    const etag = (await env.MEDIA.head(key))?.customMetadata?.etag;
    if (etag === undefined) return null;
    if (inm === etag) return notModified(etag);
  }
  const obj = await env.MEDIA.get(key);
  if (!obj) return null;
  const m = obj.customMetadata ?? {};
  const res = new Response(obj.body, { headers: { "content-type": obj.httpMetadata?.contentType ?? "" } });
  for (const k of ["etag", "cache-control", "x-plumblog-theme"]) if (m[k] !== undefined) res.headers.set(k, m[k]);
  ctx.waitUntil(caches.default.put(cacheKey, res.clone()).catch((e) => console.warn(`entry: cache store failed: ${e}`)));
  res.headers.set("x-plumblog-cache", "store");
  res.headers.set(MARK, "js");
  return res;
}

export default class extends WorkerEntrypoint {
  async fetch(req) {
    const url = new URL(req.url);
    if (req.method === "GET" && url.pathname === "/healthz") return withLoadedMark(healthz(this.env), this.env);
    if ((req.method === "GET" || req.method === "HEAD") && isReaderPath(url.pathname)) {
      try {
        const res = await cached(req, url, this.env, this.ctx);
        if (res) return withLoadedMark(res, this.env);
      } catch (e) {
        console.warn(`entry: cache or R2 fast path failed, wasm takes over: ${e}`);
      }
    }
    return withLoadedMark(await new (await wasm())(this.ctx, this.env).fetch(req), this.env);
  }

  async scheduled(event) {
    return new (await wasm())(this.ctx, this.env).scheduled(event);
  }
}
