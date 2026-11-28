// Worker entry: answers /healthz and page-cache hits itself and loads the wasm Worker
// (./index.js) only for everything else. workers-rs's index.js instantiates the whole wasm at
// import time, which costs a fresh isolate 8-40 ms CPU (Free plan limit: 10 ms). Any doubt
// or failure here hands the request to wasm, which does its own lookup, rendering and logging.
// The key format and the theme tag rule duplicate crates/worker pages/cache.rs and
// theme_runtime.rs; tools/worker-test/entry.test.mjs pins them to the Rust side.
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

// What lib.rs routes to pages::serve (media::serve owns /media/) besides /search, which is never cached.
const isReaderPath = (p) =>
  !(p.startsWith("/api/") || p === "/mcp" || p.startsWith("/media/") || p === "/admin" || p.startsWith("/admin/") || p === "/healthz" || p === "/search");

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

/** The cached page for this request (a 304 when its ETag matches), or null to let wasm handle it. */
async function cached(req, url, env) {
  const tag = themeTag(env);
  if (tag === null) return null;
  const row = await env.DB.prepare("SELECT content_version FROM site WHERE id = 1").first();
  const version = row?.content_version;
  if (!Number.isSafeInteger(version) || version < 0) throw new Error(`content_version is ${JSON.stringify(version)}`);
  const hit = await caches.default.match(`${url.origin}/__page/v${version}${tag}${url.pathname}`);
  if (!hit) return null;
  const etag = hit.headers.get("etag");
  if (etag !== null && req.headers.get("if-none-match") === etag) {
    return new Response(null, { status: 304, headers: { etag, [MARK]: "js" } });
  }
  const headers = new Headers();
  for (const [k, v] of hit.headers) if (k !== "x-plumblog-cache" && k !== "server-timing") headers.set(k, v);
  headers.set("x-plumblog-cache", "hit");
  headers.set(MARK, "js");
  return new Response(hit.body, { status: hit.status, headers });
}

export default class extends WorkerEntrypoint {
  async fetch(req) {
    const url = new URL(req.url);
    if (req.method === "GET" && url.pathname === "/healthz") return withLoadedMark(healthz(this.env), this.env);
    if ((req.method === "GET" || req.method === "HEAD") && isReaderPath(url.pathname)) {
      try {
        const res = await cached(req, url, this.env);
        if (res) return withLoadedMark(res, this.env);
      } catch (e) {
        console.warn(`entry: cache fast path failed, wasm takes over: ${e}`);
      }
    }
    return withLoadedMark(await new (await wasm())(this.ctx, this.env).fetch(req), this.env);
  }

  async scheduled(event) {
    return new (await wasm())(this.ctx, this.env).scheduled(event);
  }
}
