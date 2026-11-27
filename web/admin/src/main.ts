/** Admin entry: reads the shell's boot data, then draws the route for the signed-in user. */

import { type Api, type Me, type Result, httpApi } from "./api/client";
import { mockApi } from "./api/mock";
import { loadText, readBoot } from "./boot";
import { h } from "./dom";
import { initI18n } from "./i18n";
import { current, start } from "./router";
import { screen } from "./screens";
import { shell } from "./shell";
import { applyTheme, storedTheme } from "./theme";
import { closePop, failed, loadingRows, offlineBanner } from "./ui";

const boot = readBoot();
initI18n(boot, await loadText(boot.lang));
applyTheme(storedTheme());

const params = new URLSearchParams(location.search);
const api: Api = MOCK ? mockApi(params) : httpApi();
const forceOffline = MOCK && params.get("mock") === "offline";
const root = document.getElementById("app") ?? document.body.appendChild(h("div", { id: "app" }));

/** `undefined` while the first answer is on its way. */
let me: Result<Me> | undefined;

/** Focus moves to the new page's title on navigation, not on the first load. */
let navigated = false;

function render(): void {
  closePop();
  const name = current();
  const main = shell(root, name, boot.site, me?.ok ? me.value : undefined);
  if (forceOffline || !navigator.onLine) main.append(offlineBanner());
  if (me === undefined) main.append(loadingRows());
  else if (!me.ok) main.append(failed(me.error, () => void load()));
  else {
    main.append(...screen(name, me.value).filter((c): c is HTMLElement => c instanceof HTMLElement));
    if (navigated) main.querySelector<HTMLElement>("h1")?.focus({ preventScroll: true });
  }
}

async function load(): Promise<void> {
  me = undefined;
  render();
  me = await api.me();
  render();
}

start(() => {
  navigated = true;
  render();
});
addEventListener("online", render);
addEventListener("offline", render);
void load();

if ("serviceWorker" in navigator) {
  navigator.serviceWorker.register("/admin/sw.js", { scope: "/admin/" }).catch((e: unknown) => {
    // The admin works without it; only the offline shell is lost.
    console.error("admin: service worker not registered", e);
  });
}
