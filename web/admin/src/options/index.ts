/**
 * Settings > Appearance and Settings > Plugins: forms generated from option declarations
 * (form.ts). Their own lazy chunk, loaded by settings.ts. Nothing here knows a theme or plugin.
 */

import type { Api, ApiError } from "../api/client";
import type { OptionDeclarations, ThemeList } from "../api/model.gen";
import { h } from "../dom";
import { getOnce } from "../early";
import { lang, t } from "../i18n";
import { seg } from "../seg";
import { empty, failed, loadingRows } from "../ui";
import { explain, optionsForm } from "./form";

interface Plugin {
  name: string;
  enabled: boolean;
  disabled_reason: string | null;
  updated_at: number;
}

const enc = encodeURIComponent;

/** Declarations and stored values of one theme or plugin, or the error that stopped either. */
async function loadOptions(api: Api, path: string): Promise<{ ok: true; decls: OptionDeclarations; stored: Record<string, unknown> } | { ok: false; error: ApiError }> {
  const [d, s] = await Promise.all([
    api.call<OptionDeclarations>("GET", `${path}/declarations?lang=${enc(lang())}`),
    api.call<{ options: Record<string, unknown> }>("GET", `${path}/options`),
  ]);
  if (!d.ok) return d;
  if (!s.ok) return s;
  return { ok: true, decls: d.value, stored: s.value.options ?? {} };
}

const header = (title: string, ...lines: string[]): HTMLElement =>
  h("header", { class: "sec" }, h("h2", {}, title), ...lines.map((l) => h("p", { class: "hint" }, l)));

// --- Appearance ---

export function mountAppearance(box: HTMLElement, api: Api, offline: () => boolean): void {
  const unavailable = (name: string): HTMLElement =>
    h("div", { class: "sec", role: "alert" }, h("h2", {}, t("admin.settings.appearance")), h("p", {}, t("admin.options.unavailable", { name })));

  const load = (): void => {
    box.replaceChildren(loadingRows(3));
    void (async () => {
      const list = await getOnce<ThemeList>(api, "/api/v1/themes");
      if (!list.ok) return box.replaceChildren(failed(list.error, load));
      // A bound theme that did not load: the default renders, but its options are not the owner's to edit.
      const bad = list.value.themes.find((x) => !x.available);
      if (bad) return box.replaceChildren(unavailable(bad.name));
      const active = list.value.themes.find((x) => x.active);
      if (!active) return box.replaceChildren(failed({ code: "bad_response", status: 200 }, load));
      const path = `/api/v1/themes/${enc(active.name)}`;
      const r = await loadOptions(api, path);
      if (!r.ok) return box.replaceChildren(r.error.code === "theme.unavailable" ? unavailable(active.name) : failed(r.error, load));
      const line = active.version ? t("admin.options.theme_line", { name: active.name, version: active.version }) : t("admin.options.theme_line_bare", { name: active.name });
      const top = header(t("admin.settings.appearance"), t("admin.options.appearance_hint"), line);
      if (r.decls.options.length === 0) {
        return box.replaceChildren(top, empty({ icon: "settings", title: t("admin.options.no_options"), hint: t("admin.options.no_options_hint") }));
      }
      box.replaceChildren(top, optionsForm({ api, path, decls: r.decls, stored: r.stored, offline, prefix: "app" }));
    })();
  };
  load();
}

// --- Plugins ---

export function mountPlugins(box: HTMLElement, api: Api, offline: () => boolean): void {
  const load = (): void => {
    box.replaceChildren(loadingRows(3));
    void getOnce<{ plugins: Plugin[] }>(api, "/api/v1/plugins").then((r) => {
      if (!r.ok) return box.replaceChildren(failed(r.error, load));
      const top = header(t("admin.settings.plugins"), t("admin.options.plugins_hint"));
      if (r.value.plugins.length === 0) {
        return box.replaceChildren(top, empty({ icon: "settings", title: t("admin.options.plugins_none"), hint: t("admin.options.plugins_none_hint") }));
      }
      box.replaceChildren(top, ...pluginList(api, r.value.plugins, offline));
    });
  };
  load();
}

function pluginList(api: Api, plugins: Plugin[], offline: () => boolean): HTMLElement[] {
  const switches: { sw: HTMLInputElement; busy: boolean }[] = [];
  const rows = plugins.map((p) => {
    const state = h("span", { class: "hint" });
    const err = h("span", { class: "err", role: "alert" });
    err.hidden = true;
    const sw = h("input", { class: "sw", type: "checkbox", role: "switch", checked: p.enabled });
    const me = { sw, busy: false };
    switches.push(me);
    const paint = (): void => {
      state.textContent = p.enabled ? t("admin.options.on") : p.disabled_reason ? t("admin.options.plugin_reason", { reason: p.disabled_reason }) : t("admin.options.off");
    };
    paint();
    sw.addEventListener("change", () => {
      void (async () => {
        const want = sw.checked;
        me.busy = true;
        err.hidden = true;
        follow();
        const r = await api.call<undefined>("PUT", `/api/v1/plugins/${enc(p.name)}`, { enabled: want });
        me.busy = false;
        if (r.ok) {
          p.enabled = want;
          p.disabled_reason = null;
        } else {
          sw.checked = !want;
          err.textContent = `${t("admin.options.plugin_failed", { name: p.name })} ${explain(r.error)}`;
          err.hidden = false;
        }
        paint();
        follow();
      })();
    });
    return h("div", { class: "field" }, h("label", { class: "opt between" }, h("span", { class: "opt-wrap" }, h("span", {}, p.name), state), h("span", { class: "sw-hit" }, sw)), err);
  });
  // A switch cannot be flipped while its write is out or the connection is down.
  let seen = false;
  const follow = (): void => {
    if (list.isConnected) seen = true;
    else if (seen) {
      removeEventListener("online", follow);
      removeEventListener("offline", follow);
      return;
    }
    for (const s of switches) s.sw.disabled = s.busy || offline();
  };
  addEventListener("online", follow);
  addEventListener("offline", follow);

  const list = h("section", { class: "sec", "aria-label": t("admin.settings.plugins") }, ...rows);
  const panel = h("div", { role: "group" });
  const cache = new Map<string, HTMLElement>();
  const show = (name: string): void => {
    panel.setAttribute("aria-label", t("admin.options.plugin_options", { name }));
    const shown = cache.get(name);
    if (shown) return panel.replaceChildren(shown);
    panel.replaceChildren(loadingRows(3));
    void loadOptions(api, `/api/v1/plugins/${enc(name)}`).then((r) => {
      let node: HTMLElement;
      if (!r.ok) node = r.error.code === "store.not_found" ? h("p", { class: "hint" }, t("admin.options.plugin_missing")) : failed(r.error, () => show(name));
      else if (r.decls.options.length === 0) node = h("p", { class: "hint" }, t("admin.options.plugin_no_options"));
      else node = optionsForm({ api, path: `/api/v1/plugins/${enc(name)}`, decls: r.decls, stored: r.stored, offline, prefix: `plg-${name}` });
      // Only a form is kept: a failure is tried again on the next visit.
      if (r.ok) cache.set(name, node);
      if (panel.getAttribute("aria-label") === t("admin.options.plugin_options", { name })) panel.replaceChildren(node);
    });
  };
  const first = plugins[0]?.name ?? "";
  show(first);
  follow();
  const picker = plugins.length > 1 ? seg(t("admin.options.plugin_pick"), plugins.map((p) => p.name), first, (n) => n, show) : null;
  return [list, picker ? h("div", { class: "sec" }, picker) : null, panel].filter((x): x is HTMLElement => x !== null);
}
