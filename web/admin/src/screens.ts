/**
 * The screens of the first admin build. Lists stay empty until the posts and media APIs land
 * (issue #10); each screen already has its real header, empty and no-permission states.
 */

import type { Api, Me, PostSummary } from "./api/client";
import { PERMISSION_ROLES } from "./api/model.gen";
import { type Child, h } from "./dom";
import { icon } from "./icons";
import { t } from "./i18n";
import { HREF, type Screen, editHref } from "./router";
import { empty, failed, head, loadingRows } from "./ui";

/** In the header the label folds away on phones (`.new-label`); in an empty state it stays. */
const newPost = (folding: boolean): HTMLElement =>
  h(
    "a",
    { class: "btn primary", href: HREF.new, "aria-label": folding ? t("admin.posts.new") : null },
    icon("plus"),
    folding ? h("span", { class: "new-label" }, t("admin.posts.new")) : t("admin.posts.new"),
  );

const statusName = (s: PostSummary["status"]): string =>
  // i18n: admin.status.draft admin.status.scheduled admin.status.published
  t(`admin.status.${s}`);

/** Title / status / date / language, aligned so the eye can run down each column. */
function row(p: PostSummary): HTMLElement {
  const when = new Intl.DateTimeFormat(document.documentElement.lang, { dateStyle: "medium" }).format(p.updated_at);
  return h(
    "a",
    { class: "row", href: editHref(p.id) },
    h("div", { class: "row-main" }, h("span", { class: "row-title", lang: p.lang }, p.title || t("admin.posts.untitled")), p.excerpt ? h("span", { class: "row-ex", lang: p.lang }, p.excerpt) : null),
    h("div", { class: "row-meta" }, h("span", {}, h("span", { class: `st ${p.status}` }, statusName(p.status))), h("span", { class: "row-date" }, when)),
    h("div", { class: "row-lang" }, h("span", { class: "lang" }, p.lang)),
  );
}

/** The posts list: loads, then shows rows, the empty state or a plain error with retry. */
function postsList(api: Api): HTMLElement {
  const box = h("div", {}, loadingRows());
  const load = async () => {
    box.replaceChildren(loadingRows());
    const r = await api.posts();
    if (!r.ok) box.replaceChildren(failed(r.error, () => void load()));
    else if (!r.value.length) box.replaceChildren(empty({ icon: "posts", title: t("admin.posts.empty"), hint: t("admin.posts.empty_hint"), action: newPost(false) }));
    else box.replaceChildren(h("div", { class: "rows" }, ...r.value.map(row)));
  };
  void load();
  return box;
}

export function screen(name: Screen, me: Me, api: Api): Child[] {
  switch (name) {
    case "posts":
      return [head(t("admin.nav.posts"), newPost(true)), postsList(api)];
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
    case "edit":
      // Drawn by the editor chunk (main.ts); never reached.
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
