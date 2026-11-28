/** The frame around every screen: sidebar, icon rail on tablets, glass bar on phones. */

import type { Me } from "./api/client";
import { PERMISSION_ROLES } from "./api/model.gen";
import { h } from "./dom";
import { type IconName, icon } from "./icons";
import { t } from "./i18n";
import { HREF, type Screen } from "./router";
import { langSwitch, seg } from "./seg";
import { THEMES, type ThemeChoice, applyTheme, storedTheme } from "./theme";
import { closePop, openPop } from "./ui";

/** Renders the frame and returns the element screens draw into. */
export function shell(root: HTMLElement, screen: Screen, site: string, me: Me | undefined, signOut?: (anchor: HTMLElement) => void): HTMLElement {
  const owner = me !== undefined && PERMISSION_ROLES["settings.manage"].includes(me.role);
  const link = (to: keyof typeof HREF, ic: IconName, label: string) =>
    h(
      "a",
      { href: HREF[to], "aria-current": screen === to || (to === "posts" && (screen === "new" || screen === "edit")) ? "page" : null },
      icon(ic),
      h("span", { class: "lbl" }, label),
    );
  const meButton = h(
    "button",
    {
      class: "me",
      type: "button",
      "aria-haspopup": "dialog",
      "aria-expanded": "false",
      // The visible name and role are the accessible name; the label only stands in before
      // the user is known (the phone label is hidden on wider screens).
      "aria-label": me ? null : t("admin.nav.account"),
      title: t("admin.nav.account"),
      onclick: () => openPop(meButton, meMenu(me && signOut ? () => signOut(meButton) : undefined), t("admin.nav.account")),
    },
    icon("user"),
    me
      ? h(
          "span",
          { class: "who" },
          h("b", {}, me.display_name),
          // i18n: admin.role.owner admin.role.writer
          h("span", {}, t(`admin.role.${me.role}`)),
        )
      : null,
    h("span", { class: "me-lbl" }, t("admin.nav.me")),
  );
  const main = h("main", { class: "main", id: "main" });
  root.replaceChildren(
    h(
      "div",
      { class: "shell" },
      h(
        "nav",
        { class: "side", "aria-label": t("admin.nav.main") },
        h("div", { class: "site" }, h("div", { class: "mark" }, icon("plum")), h("span", {}, site)),
        h(
          "div",
          { class: "nav" },
          link("posts", "posts", t("admin.nav.posts")),
          link("pages", "pages", t("admin.nav.pages")),
          link("media", "media", t("admin.nav.media")),
          owner ? link("settings", "settings", t("admin.nav.settings")) : null,
        ),
        h("div", { class: "spacer" }),
        meButton,
      ),
      main,
    ),
  );
  return main;
}

function meMenu(signOut: (() => void) | undefined): HTMLElement {
  const themeName = (v: ThemeChoice) =>
    // i18n: admin.theme.light admin.theme.dark admin.theme.system
    t(`admin.theme.${v}`);
  return h(
    "div",
    {},
    signOut ? h("a", { class: "menu-item", href: HREF.account, "aria-current": location.pathname === HREF.account ? "page" : null }, icon("user"), t("admin.account.title")) : null,
    signOut ? h("div", { class: "menu-sep" }) : null,
    h("div", { class: "menu-head cap" }, t("admin.me.colors")),
    h("div", { class: "menu-pad" }, seg(t("admin.me.colors"), THEMES, storedTheme(), themeName, applyTheme)),
    h("div", { class: "menu-head cap" }, t("admin.me.language")),
    h("div", { class: "menu-pad" }, langSwitch()),
    signOut ? h("div", { class: "menu-sep" }) : null,
    signOut
      ? h("button", { class: "menu-item", type: "button", "data-act": "signout", onclick: () => (closePop(false), signOut()) }, t("admin.account.signout"))
      : null,
  );
}
