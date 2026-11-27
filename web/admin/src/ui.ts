/** Shared pieces every screen composes (styles: web/admin/css/components.css). */

import type { ApiError } from "./api/client";
import { type Child, h } from "./dom";
import { type IconName, icon } from "./icons";
import { t } from "./i18n";

/** Page header: title left, actions right (search, when a page has it, last). */
export function head(title: string, ...actions: Child[]): HTMLElement {
  // Focus lands on the title after a route change, so screen readers announce the page.
  return h("header", { class: "head" }, h("h1", { tabindex: -1 }, title), ...actions);
}

export function empty(o: {
  icon?: IconName;
  title: string;
  hint?: string;
  action?: Child;
  full?: boolean;
}): HTMLElement {
  return h(
    "div",
    { class: o.full ? "empty full" : "empty" },
    o.icon ? icon(o.icon) : null,
    h("strong", {}, o.title),
    o.hint ? h("span", {}, o.hint) : null,
    o.action ?? null,
  );
}

export function failed(err: ApiError, retry: () => void): HTMLElement {
  const network = err.code === "network";
  return h(
    "div",
    { role: "alert" },
    empty({
      icon: network ? "offline" : "alert",
      title: network ? t("admin.state.network_title") : t("admin.state.error_title"),
      hint: network ? t("admin.state.network_hint") : t("admin.state.error_hint", { code: err.code }),
      action: h("button", { class: "btn", type: "button", onclick: retry }, t("admin.state.retry")),
      full: true,
    }),
  );
}

/** Placeholder rows while a list loads; widths vary so it reads as text, not a grid. */
export function loadingRows(n = 5): HTMLElement {
  const widths = ["45%", "60%", "38%", "52%", "41%"];
  const rows = Array.from({ length: n }, (_, i) => {
    const title = h("div", { class: "skel" });
    title.style.width = widths[i % widths.length] ?? "50%";
    const line = h("div", { class: "skel sm" });
    line.style.width = "72%";
    return h("div", { class: "row", "aria-hidden": "true" }, h("div", { class: "row-main" }, title, line));
  });
  return h(
    "div",
    { class: "rows", "aria-busy": "true" },
    h("span", { class: "sr", role: "status" }, t("admin.state.loading")),
    ...rows,
  );
}

export function offlineBanner(): HTMLElement {
  return h(
    "div",
    { class: "banner", role: "status" },
    icon("offline"),
    h("div", {}, h("b", {}, t("admin.state.offline_title")), " ", t("admin.state.offline_hint")),
  );
}

let pop: { el: HTMLElement; anchor: HTMLElement } | undefined;

export function closePop(): void {
  if (!pop) return;
  const { el, anchor } = pop;
  pop = undefined;
  el.remove();
  anchor.setAttribute("aria-expanded", "false");
  document.removeEventListener("pointerdown", outside, true);
  document.removeEventListener("keydown", onKey, true);
  anchor.focus();
}

function outside(e: Event): void {
  const target = e.target as Node;
  if (pop && !pop.el.contains(target) && !pop.anchor.contains(target)) closePop();
}

function onKey(e: KeyboardEvent): void {
  if (e.key === "Escape") {
    e.preventDefault();
    closePop();
  }
}

/** A popover next to `anchor`: beside it on wide screens, above it on phones. */
export function openPop(anchor: HTMLElement, content: HTMLElement, label: string): void {
  const reopen = pop?.anchor !== anchor;
  closePop();
  if (!reopen) return;
  const el = h("div", { class: "pop", role: "dialog", "aria-label": label }, content);
  document.body.append(el);
  const r = anchor.getBoundingClientRect();
  const p = el.getBoundingClientRect();
  const vw = document.documentElement.clientWidth;
  const vh = window.innerHeight;
  const gap = 8;
  const edge = 16;
  let x: number;
  let y: number;
  if (vw >= 600) {
    x = r.right + gap;
    y = Math.min(r.bottom - p.height, vh - p.height - edge);
  } else {
    x = r.right - p.width;
    y = r.top - p.height - gap;
  }
  el.style.left = `${Math.max(edge, Math.min(x, vw - p.width - edge))}px`;
  el.style.top = `${Math.max(edge, y)}px`;
  anchor.setAttribute("aria-expanded", "true");
  pop = { el, anchor };
  setTimeout(() => document.addEventListener("pointerdown", outside, true));
  document.addEventListener("keydown", onKey, true);
  el.querySelector<HTMLElement>("button, a, input")?.focus();
}
