/** History routing under /admin/; the Worker answers every admin path with the same shell. */

export type Screen = "posts" | "pages" | "media" | "settings" | "new" | "missing";

export const HREF = {
  posts: "/admin/",
  pages: "/admin/pages",
  media: "/admin/media",
  settings: "/admin/settings",
  new: "/admin/posts/new",
} as const satisfies Record<Exclude<Screen, "missing">, string>;

export function current(): Screen {
  const path = location.pathname.replace(/\/+$/, "") || "/";
  if (path === "/admin") return "posts";
  const hit = Object.entries(HREF).find(([, href]) => href === path);
  return hit ? (hit[0] as Screen) : "missing";
}

let listener: (() => void) | undefined;

export function start(onChange: () => void): void {
  listener = onChange;
  addEventListener("popstate", onChange);
  document.addEventListener("click", follow);
}

export function go(path: string): void {
  if (path !== location.pathname + location.search) history.pushState(null, "", path);
  listener?.();
}

/** Same-origin admin links change the route without a page load. */
function follow(e: MouseEvent): void {
  if (e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
  const a = (e.target as Element | null)?.closest("a");
  if (!a || a.target || a.hasAttribute("download")) return;
  const url = new URL(a.href);
  if (url.origin !== location.origin || !url.pathname.startsWith("/admin/")) return;
  e.preventDefault();
  go(url.pathname + url.search);
}
