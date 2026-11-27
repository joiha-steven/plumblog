/**
 * Keeps the admin shell available offline (docs/ARCHITECTURE.md, Installable web apps).
 * Network first, so a deploy is picked up on the next load; the cache is only the fallback.
 * Never touches /api/: data must be live or fail visibly.
 */

/** Hash of this build's files and their URLs, set by scripts/admin-build. */
declare const BUILD: string;
declare const ASSETS: string[];

const sw = self as unknown as ServiceWorkerGlobalScope;
// One cache per build: activating a new build deletes the old cache and the chunks in it.
const CACHE = `plumblog-admin-${BUILD}`;
const SHELL = ["/admin/", "/admin/icon.svg", "/admin/manifest.webmanifest", ...ASSETS];
const KEEP = new Set(SHELL);

sw.addEventListener("install", (e) => {
  e.waitUntil(
    caches
      .open(CACHE)
      .then((c) => c.addAll(SHELL))
      .then(() => sw.skipWaiting()),
  );
});

sw.addEventListener("activate", (e) => {
  e.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((k) => k !== CACHE).map((k) => caches.delete(k))))
      .then(() => sw.clients.claim()),
  );
});

sw.addEventListener("fetch", (e) => {
  const url = new URL(e.request.url);
  if (e.request.method !== "GET" || url.origin !== location.origin || !url.pathname.startsWith("/admin/")) return;
  e.respondWith(fresh(e.request, url.pathname));
});

async function fresh(req: Request, path: string): Promise<Response> {
  const cache = await caches.open(CACHE);
  // Every client route is the same shell; other URLs are kept only if this build lists them.
  // The sign-in pages are not kept: their CSP admits Turnstile, and an offline fallback must
  // never hand that looser policy to another page. Offline they get the strict shell, which
  // says the admin is offline.
  const signin = /^\/admin\/(signin|setup|invite)\/?$/.test(path);
  const key = req.mode === "navigate" ? "/admin/" : KEEP.has(path) ? path : undefined;
  try {
    const res = await fetch(req);
    if (res.ok && key && !signin) await cache.put(key, res.clone());
    return res;
  } catch (err) {
    const hit = key ? await cache.match(key) : undefined;
    if (hit) return hit;
    console.error(`admin sw: ${req.url} offline and not cached`, err);
    return Response.error();
  }
}
