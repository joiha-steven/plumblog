/** History routing under /admin/; the Worker answers every admin path with the same shell. */

export type Screen = "posts" | "pages" | "media" | "settings" | "syslog" | "account" | "new" | "edit" | "missing" | AuthScreen;

/** The signed-out pages: a full page load each (their CSP differs), never client-side routing. */
export type AuthScreen = "signin" | "setup" | "invite" | "reset";
export const isAuth = (s: Screen): s is AuthScreen => s === "signin" || s === "setup" || s === "invite" || s === "reset";

export const HREF = {
  posts: "/admin/",
  pages: "/admin/pages",
  media: "/admin/media",
  settings: "/admin/settings",
  syslog: "/admin/settings/log",
  account: "/admin/account",
  new: "/admin/posts/new",
} as const satisfies Record<Exclude<Screen, "missing" | "edit" | AuthScreen>, string>;

/** The screens that live under Settings: the top-nav item stays current on each. */
const SETTINGS_SCREENS: readonly Screen[] = ["settings", "syslog"];
export const inSettings = (s: Screen): boolean => SETTINGS_SCREENS.includes(s);

/** Sections drawn by the settings screen itself, at /admin/settings/<id> ("general" is /admin/settings). */
export const SETTINGS_SECTIONS: readonly string[] = ["appearance", "plugins"];

export const editHref = (id: number): string => `/admin/posts/${id}`;

export interface Route {
  screen: Screen;
  /** The post id on `edit`. */
  id?: number;
  /** The settings section on `settings`; absent is General. */
  section?: string;
}

export function current(): Route {
  // At most one trailing slash, as the Worker (is_auth_page) and sw.ts read paths: anything
  // else is not a page, so a sign-in page never renders under the signed-in CSP.
  const path = location.pathname.replace(/\/$/, "") || "/";
  if (path === "/admin") return { screen: "posts" };
  const auth = /^\/admin\/(signin|setup|invite|reset)$/.exec(path);
  if (auth) return { screen: auth[1] as AuthScreen };
  const edit = /^\/admin\/posts\/(\d{1,15})$/.exec(path);
  if (edit) return { screen: "edit", id: Number(edit[1]) };
  const sec = /^\/admin\/settings\/([a-z]+)$/.exec(path);
  if (sec?.[1] && SETTINGS_SECTIONS.includes(sec[1])) return { screen: "settings", section: sec[1] };
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
