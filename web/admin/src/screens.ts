/**
 * The screens of the first admin build. Lists stay empty until the posts and media APIs land
 * (issue #10); each screen already has its real header, empty and no-permission states.
 */

import type { Me } from "./api/client";
import { PERMISSION_ROLES } from "./api/model.gen";
import { type Child, h } from "./dom";
import { icon } from "./icons";
import { t } from "./i18n";
import { HREF, type Screen } from "./router";
import { empty, head } from "./ui";

/** In the header the label folds away on phones (`.new-label`); in an empty state it stays. */
const newPost = (folding: boolean): HTMLElement =>
  h(
    "a",
    { class: "btn primary", href: HREF.new, "aria-label": folding ? t("admin.posts.new") : null },
    icon("plus"),
    folding ? h("span", { class: "new-label" }, t("admin.posts.new")) : t("admin.posts.new"),
  );

export function screen(name: Screen, me: Me): Child[] {
  switch (name) {
    case "posts":
      return [
        head(t("admin.nav.posts"), newPost(true)),
        empty({ icon: "posts", title: t("admin.posts.empty"), hint: t("admin.posts.empty_hint"), action: newPost(false) }),
      ];
    case "pages":
      return [head(t("admin.nav.pages")), empty({ icon: "pages", title: t("admin.pages.empty"), hint: t("admin.pages.empty_hint") })];
    case "media":
      return [head(t("admin.nav.media")), empty({ icon: "media", title: t("admin.media.empty"), hint: t("admin.media.empty_hint") })];
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
    case "new":
      return [head(t("admin.posts.new")), building()];
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
