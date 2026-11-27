/** History routing under /admin/; the Worker answers every admin path with the same shell. */

export type Screen = "posts" | "pages" | "media" | "settings" | "new" | "edit" | "missing";

export const HREF = {
  posts: "/admin/",
  pages: "/admin/pages",
  media: "/admin/media",
  settings: "/admin/settings",
  new: "/admin/posts/new",
} as const satisfies Record<Exclude<Screen, "missing" | "edit">, string>;

export const editHref = (id: number): string => `/admin/posts/${id}`;

export interface Route {
  screen: Screen;
  /** The post id on `edit`. */
  id?: number;
}

export function current(): Route {
  const path = location.pathname.replace(/\/+$/, "") || "/";
  if (path === "/admin") return { screen: "posts" };
  const edit = /^\/admin\/posts\/(\d{1,15})$/.exec(path);
  if (edit) return { screen: "edit", id: Number(edit[1]) };
  const hit = Object.entries(HREF).find(([, href]) => href === path);
  return { screen: hit ? (hit[0] as Screen) : "missing" };
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
