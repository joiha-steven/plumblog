/** Admin entry: reads the shell's boot data, then draws the signed-out page or the route for the signed-in user. */

import { type Api, type Me, type Result, httpApi, session } from "./api/client";
import type { Post } from "./api/model.gen";
import { mockApi } from "./api/mock";
import { loadText, readBoot } from "./boot";
import { h } from "./dom";
import { initI18n, t } from "./i18n";
import { type Route, current, isAuth, start } from "./router";
import { screen } from "./screens";
import { shell } from "./shell";
import { applyTheme, storedTheme } from "./theme";
import { closePop, failed, loadingRows, offlineBanner, toast } from "./ui";

const boot = readBoot();
initI18n(boot, await loadText(boot.lang));
applyTheme(storedTheme());

const params = new URLSearchParams(location.search);
const api: Api = MOCK ? mockApi(params) : httpApi();
const forceOffline = MOCK && params.get("mock") === "offline";
const root = document.getElementById("app") ?? document.body.appendChild(h("div", { id: "app" }));

/**
 * Always a full page load: the sign-in pages run under their own CSP (Turnstile), so client-side
 * routing would draw them under the wrong one.
 */
function toSignIn(): void {
  location.assign(`/admin/signin?next=${encodeURIComponent(location.pathname + location.search)}`);
}

/** Sign-out (and the confirm before it deletes drafts) is in the auth chunk, loaded on first click. */
function signOut(anchor: HTMLElement): void {
  if (!me?.ok) return;
  const user = me.value;
  import("./auth/account").then(
    (m) => m.signOutFlow({ anchor, api, me: user }),
    (e: unknown) => {
      console.error("admin: sign-out failed to load", e);
      toast(t("admin.account.err_signout"), 5000);
    },
  );
}

/** `undefined` while the first answer is on its way. */
let me: Result<Me> | undefined;

/** Focus moves to the new page's title on navigation, not on the first load. */
let navigated = false;

/** The editor is its own chunk: lists and settings never load CodeMirror. */
async function editor(route: Route): Promise<void> {
  root.replaceChildren(loadingRows());
  let post: Post | undefined;
  if (route.id !== undefined) {
    const r = await api.post(route.id);
    if (!r.ok) {
      const main = shell(root, "posts", boot.site, me?.ok ? me.value : undefined, signOut);
      main.append(failed(r.error, () => void editor(route)));
      return;
    }
    post = r.value;
  }
  try {
    const { mountEditor } = await import("./editor/index");
    const user = me?.ok ? me.value.id : 0;
    await mountEditor({ root, api, post, user, lang: boot.lang, nonce: boot.nonce, offline: () => forceOffline || !navigator.onLine });
  } catch (e) {
    // A chunk that failed to load (offline, new deploy): say so instead of a blank screen.
    console.error("admin: editor failed to load", e);
    const main = shell(root, "posts", boot.site, me?.ok ? me.value : undefined, signOut);
    main.append(failed({ code: "network", status: 0 }, () => void editor(route)));
  }
}

function render(): void {
  closePop();
  const route = current();
  const name = route.screen;
  if ((name === "new" || name === "edit") && me?.ok) {
    void editor(route);
    return;
  }
  const main = shell(root, name, boot.site, me?.ok ? me.value : undefined, signOut);
  if (forceOffline || !navigator.onLine) main.append(offlineBanner());
  if (me === undefined) main.append(loadingRows());
  else if (!me.ok) main.append(failed(me.error, () => void load()));
  else {
    main.append(...screen(name, me.value, api).filter((c): c is HTMLElement => c instanceof HTMLElement));
    if (navigated) main.querySelector<HTMLElement>("h1")?.focus({ preventScroll: true });
  }
}

async function load(): Promise<void> {
  me = undefined;
  render();
  me = await api.me();
  if (!me.ok && me.error.code === "auth.unauthenticated") {
    toSignIn();
    return;
  }
  render();
}

/** Sign-in, setup and invite load their own chunk and never ask `me()` first. */
async function signedOut(kind: "signin" | "setup" | "invite"): Promise<void> {
  try {
    const { mountAuth } = await import("./auth/screens");
    mountAuth({ root, api, kind, site: boot.site, query: params });
  } catch (e) {
    // A chunk that failed to load (offline, new deploy): say so instead of a blank screen.
    console.error("admin: sign-in page failed to load", e);
    root.replaceChildren(failed({ code: "network", status: 0 }, () => location.reload()));
  }
}

const first = current().screen;
if (isAuth(first)) {
  void signedOut(first);
} else {
  session.expired = toSignIn;
  start(() => {
    navigated = true;
    render();
  });
  addEventListener("online", render);
  addEventListener("offline", render);
  void load();
}

if ("serviceWorker" in navigator) {
  navigator.serviceWorker.register("/admin/sw.js", { scope: "/admin/" }).catch((e: unknown) => {
    // The admin works without it; only the offline shell is lost.
    console.error("admin: service worker not registered", e);
  });
}
