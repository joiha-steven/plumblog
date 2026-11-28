/**
 * The screens of the admin shell. The posts and pages lists are in lists.ts; the media library
 * (media/) and the account page (auth/) are lazy chunks.
 */

import type { Api, Me } from "./api/client";
import { PERMISSION_ROLES } from "./api/model.gen";
import { type Child, h } from "./dom";
import { icon } from "./icons";
import { t } from "./i18n";
import { pagesList, postsList } from "./lists";
import { HREF, type Screen } from "./router";
import { empty, failed, head, loadingRows } from "./ui";

/** In the header the label folds away on phones (`.new-label`); in an empty state it stays. */
const newPost = (folding: boolean): HTMLElement =>
  h(
    "a",
    { class: "btn primary", href: HREF.new, "aria-label": folding ? t("admin.posts.new") : null },
    icon("plus"),
    folding ? h("span", { class: "new-label" }, t("admin.posts.new")) : t("admin.posts.new"),
  );

/** A screen that lives in its own chunk, which the shell never carries; `mount` loads it into the box. */
function lazyBox(cls: string, what: string, mount: (box: HTMLElement) => Promise<void>): HTMLElement {
  const wait = (): HTMLElement => {
    const rows = loadingRows(2);
    rows.classList.add("flush");
    return rows;
  };
  const box = h("div", { class: cls }, wait());
  const load = (): void => {
    box.replaceChildren(wait());
    mount(box).catch((e: unknown) => {
      // A chunk that failed to load (offline, new deploy): say so instead of a blank screen.
      console.error(`admin: ${what} failed to load`, e);
      box.replaceChildren(failed({ code: "network", status: 0 }, load));
    });
  };
  load();
  return box;
}

const accountBox = (me: Me, api: Api): HTMLElement =>
  lazyBox("acct", "account page", async (box) => (await import("./auth/account")).mountAccount(box, me, api));

const mediaBox = (me: Me, api: Api): HTMLElement =>
  lazyBox("media", "media library", async (box) => {
    const { mountMedia } = await import("./media/index");
    box.replaceChildren();
    mountMedia(box, me, api);
  });

export function screen(name: Screen, me: Me, api: Api): Child[] {
  switch (name) {
    case "posts":
      return [head(t("admin.nav.posts"), newPost(true)), postsList(api, { user: me.id, newPost: () => newPost(false) })];
    case "pages":
      return [head(t("admin.nav.pages")), pagesList(api)];
    case "media":
      return [head(t("admin.nav.media")), mediaBox(me, api)];
    case "settings":
      if (!PERMISSION_ROLES["settings.manage"].includes(me.role)) {
        return [
          head(t("admin.nav.settings")),
          empty({
            icon: "lock",
            title: t("admin.state.no_permission_title"),
            hint: t("admin.state.no_permission_hint"),
            action: h("a", { class: "btn", href: HREF.posts }, t("admin.state.back_to_posts")),
            full: true,
          }),
        ];
      }
      return [head(t("admin.nav.settings")), building()];
    case "account":
      return [head(t("admin.account.title")), accountBox(me, api)];
    case "new":
    case "edit":
    case "signin":
    case "setup":
    case "invite":
    case "reset":
      // Drawn by the editor chunk and the sign-in chunk (main.ts); never reached.
      return [];
    case "missing":
      return [
        head(t("admin.state.not_found_title")),
        empty({
          icon: "compass",
          title: t("admin.state.not_found_title"),
          hint: t("admin.state.not_found_hint"),
          action: h("a", { class: "btn", href: HREF.posts }, t("admin.state.back_to_posts")),
          full: true,
        }),
      ];
  }
}

const building = (): HTMLElement =>
  empty({ icon: "settings", title: t("admin.state.building_title"), hint: t("admin.state.building_hint"), full: true });
