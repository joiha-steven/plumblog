/**
 * Keeps the admin shell available offline (docs/ARCHITECTURE.md, Installable web apps).
 * Network first, so a deploy is picked up on the next load; the cache is only the fallback.
 * Never touches /api/: data must be live or fail visibly.
 */

const sw = self as unknown as ServiceWorkerGlobalScope;
const CACHE = "plumblog-admin-1";
const SHELL = ["/admin/", "/admin/app.js", "/admin/app.css", "/admin/icon.svg", "/admin/manifest.webmanifest"];

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
  e.respondWith(fresh(e.request));
});

async function fresh(req: Request): Promise<Response> {
  const cache = await caches.open(CACHE);
  try {
    const res = await fetch(req);
    if (res.ok) await cache.put(req.mode === "navigate" ? "/admin/" : req, res.clone());
    return res;
  } catch (err) {
    const hit = await cache.match(req.mode === "navigate" ? "/admin/" : req);
    if (hit) return hit;
    console.error(`admin sw: ${req.url} offline and not cached`, err);
    return Response.error();
  }
}
