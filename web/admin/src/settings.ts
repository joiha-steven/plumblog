/**
 * The Settings screen: a list of sections on the left (a strip of tabs on phones) and the
 * current section on the right. Its own lazy chunk, loaded by screens.ts on first use.
 * Adding a section: one line in `ENTRIES` (and a renderer in `SECTIONS` when this file draws it).
 */

import type { Api, ApiError } from "./api/client";
import { PERMISSION_ROLES, type Permission, type Role } from "./api/model.gen";
import { h } from "./dom";
import { langs, t } from "./i18n";
import { HREF } from "./router";
import { failed, loadingRows } from "./ui";

interface Entry {
  key: string;
  href: string;
  label: () => string;
  permission: Permission;
}

/** The section list. Entries drawn by other screens (they only link there) belong here too. */
const ENTRIES: readonly Entry[] = [
  { key: "general", href: HREF.settings, label: () => t("admin.settings.general"), permission: "settings.manage" },
  { key: "users", href: "/admin/settings/users", label: () => t("admin.settings.users"), permission: "user.manage" },
  { key: "log", href: HREF.syslog, label: () => t("admin.syslog.title"), permission: "syslog.view" },
];

export interface SettingsOptions {
  role: Role;
  /** The signed-in user, marked "you" in People. */
  userId: number;
  /** Read at each use: a page that goes offline keeps its values but cannot save. */
  offline: () => boolean;
  /** The section id from the route; General when absent. */
  section?: string | undefined;
}

/** The section list for `role`, `active` marked current. Exported so a section drawn by another screen shows the same list. */
export function settingsNav(role: Role, active: string): HTMLElement {
  return h(
    "nav",
    { class: "subnav", "aria-label": t("admin.settings.sections") },
    ...ENTRIES.filter((e) => PERMISSION_ROLES[e.permission].includes(role)).map((e) =>
      h("a", { href: e.href, "aria-current": e.key === active ? "page" : null }, h("span", {}, e.label())),
    ),
  );
}

export function mountSettings(box: HTMLElement, api: Api, o: SettingsOptions): void {
  const active = o.section ?? "general";
  const nav = settingsNav(o.role, active);
  const content = h("div", { class: "settings-main" });
  box.replaceChildren(h("div", { class: "settings" }, nav, content));
  if (active === "general") general(content, api, o.offline);
  else if (active === "users") usersSection(content, api, o);
}

/** People is its own chunk: the General form never carries it. */
function usersSection(content: HTMLElement, api: Api, o: SettingsOptions): void {
  const load = (): void => {
    content.replaceChildren(loadingRows(3));
    import("./users").then(
      (m) => m.mountUsers(content, api, { me: o.userId, offline: o.offline }),
      (e: unknown) => {
        // A chunk that failed to load (offline, new deploy): say so instead of a blank screen.
        console.error("admin: people failed to load", e);
        content.replaceChildren(failed({ code: "network", status: 0 }, load));
      },
    );
  };
  load();
}

// --- General ---

const KEYS = ["site_title", "site_description", "site_footer", "site_lang", "site_per_page"] as const;
type Key = (typeof KEYS)[number];
type Values = Record<Key, string>;

/** What the Worker reads when `site_lang` is absent. */
const DEFAULT_LANG = "en";
const MAX_PER_PAGE = 50;

/** The form's text for each key, from the stored JSON values; absent keys read as empty. */
function toText(stored: Record<string, unknown>): Values {
  const str = (k: string): string => (typeof stored[k] === "string" ? (stored[k] as string) : "");
  const per = stored["site_per_page"];
  return {
    site_title: str("site_title"),
    site_description: str("site_description"),
    site_footer: str("site_footer"),
    site_lang: str("site_lang") || DEFAULT_LANG,
    site_per_page: typeof per === "number" || typeof per === "string" ? String(per) : "",
  };
}

const same = (a: string, b: string): boolean => a.trim() === b.trim();

/** An empty number is valid (the default applies); anything else must be a whole 1 to 50. */
const perPageOk = (v: string): boolean => {
  const s = v.trim();
  if (s === "") return true;
  const n = Number(s);
  return /^\d+$/.test(s) && n >= 1 && n <= MAX_PER_PAGE;
};

function general(content: HTMLElement, api: Api, offline: () => boolean): void {
  const load = (): void => {
    content.replaceChildren(loadingRows(3));
    void api.call<{ settings: Record<string, unknown> }>("GET", "/api/v1/settings").then((r) => {
      if (!r.ok) {
        content.replaceChildren(failed(r.error, load));
        return;
      }
      content.replaceChildren(generalForm(api, toText(r.value.settings ?? {}), offline));
    });
  };
  load();
}

/** The one plain message for a failed write. */
function explain(e: ApiError): string {
  switch (e.code) {
    case "network":
      return t("admin.settings.err_network");
    case "auth.forbidden":
      return t("admin.settings.err_forbidden");
    case "input.invalid":
      return t("admin.settings.err_invalid");
    default:
      return t("admin.settings.err_other", { code: e.code });
  }
}

function generalForm(api: Api, initial: Values, offline: () => boolean): HTMLElement {
  const base: Values = { ...initial };
  const cur: Values = { ...initial };
  let saving = false;

  const fields = {} as Record<Key, { input: HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement; err: HTMLElement; hintId: string }>;

  const field = (key: Key, label: string, hint: string, control: HTMLInputElement | HTMLTextAreaElement | HTMLSelectElement): HTMLElement => {
    const id = `set-${key}`;
    const err = h("span", { class: "err", id: `${id}-err` });
    err.hidden = true;
    control.id = id;
    control.name = key;
    control.setAttribute("aria-describedby", `${id}-hint`);
    fields[key] = { input: control, err, hintId: `${id}-hint` };
    control.addEventListener(control.tagName === "SELECT" ? "change" : "input", () => {
      cur[key] = control.value;
      if (!saving) idle();
      setError(key, undefined);
      refresh();
    });
    return h("div", { class: "field" }, h("label", { for: id }, label), control, h("span", { class: "hint", id: `${id}-hint` }, hint), err);
  };

  const setError = (key: Key, msg: string | undefined): void => {
    const f = fields[key];
    f.err.hidden = msg === undefined;
    f.err.textContent = msg ?? "";
    if (msg === undefined) {
      f.input.removeAttribute("aria-invalid");
      f.input.setAttribute("aria-describedby", f.hintId);
    } else {
      f.input.setAttribute("aria-invalid", "true");
      f.input.setAttribute("aria-describedby", `${f.hintId} ${f.err.id}`);
    }
  };

  const text = (key: Key, placeholder: string): HTMLInputElement =>
    h("input", { class: "input", type: "text", value: initial[key], placeholder, autocomplete: "off", maxlength: 200 });
  const area = (key: Key, placeholder: string): HTMLTextAreaElement => {
    const el = h("textarea", { class: "textarea", rows: 3, placeholder, maxlength: 500 });
    el.value = initial[key];
    return el;
  };

  // A language stored but not among the admin's own is still offered, so it is never silently changed.
  const codes = [...new Set([...langs(), initial.site_lang])];
  const select = h(
    "select",
    { class: "select" },
    ...codes.map((c) => h("option", { value: c, selected: c === initial.site_lang }, langName(c))),
  );
  const per = h("input", { class: "input", type: "number", inputmode: "numeric", min: 1, max: MAX_PER_PAGE, step: 1, value: initial.site_per_page, placeholder: "10" });

  const status = h("span", { class: "hint", role: "status" });
  const idle = (): void => {
    status.className = "hint";
    status.textContent = offline() ? t("admin.settings.offline") : "";
  };
  const save = h("button", { class: "btn primary", type: "submit" }, t("admin.settings.save"));
  const dirty = (): Key[] => KEYS.filter((k) => !same(cur[k], base[k]));
  const refresh = (): void => {
    save.disabled = saving || offline() || dirty().length === 0;
  };

  const form = h(
    "form",
    { class: "sec", novalidate: true, "aria-labelledby": "set-general" },
    h("h2", { id: "set-general" }, t("admin.settings.general")),
    field("site_title", t("admin.settings.title"), t("admin.settings.title_hint"), text("site_title", t("admin.settings.title_default"))),
    field("site_description", t("admin.settings.description"), t("admin.settings.description_hint"), area("site_description", "")),
    field("site_footer", t("admin.settings.footer"), t("admin.settings.footer_hint"), area("site_footer", "")),
    field("site_lang", t("admin.settings.lang"), t("admin.settings.lang_hint"), select),
    field("site_per_page", t("admin.settings.per_page"), t("admin.settings.per_page_hint", { max: MAX_PER_PAGE }), per),
    h("div", { class: "btn-row" }, save, status),
  );
  idle();

  form.addEventListener("submit", (e) => {
    e.preventDefault();
    if (saving || offline()) return;
    void submit();
  });

  const submit = async (): Promise<void> => {
    const todo = dirty();
    if (todo.length === 0) return;
    if (!perPageOk(cur.site_per_page)) {
      setError("site_per_page", t("admin.settings.err_per_page", { max: MAX_PER_PAGE }));
      fields.site_per_page.input.focus();
      return;
    }
    saving = true;
    status.className = "hint";
    status.textContent = t("admin.settings.saving");
    refresh();
    let firstBad: Key | undefined;
    for (const key of todo) {
      const path = `/api/v1/settings/${key}`;
      const v = cur[key].trim();
      const r =
        key !== "site_lang" && v === ""
          ? await api.call<undefined>("DELETE", path)
          : await api.call<undefined>("PUT", path, { value: key === "site_per_page" ? Number(v) : key === "site_lang" ? cur[key] : v });
      if (r.ok) {
        base[key] = cur[key];
      } else {
        setError(key, explain(r.error));
        firstBad ??= key;
      }
    }
    saving = false;
    if (firstBad) {
      status.className = "err";
      status.textContent = t("admin.settings.not_saved");
      fields[firstBad].input.focus();
    } else {
      status.textContent = t("admin.settings.saved");
    }
    refresh();
  };

  // Unsaved edits ask before the page is closed (as the editor does); a screen left by a link just drops them.
  const hold = (e: Event): void => {
    if (!form.isConnected) removeEventListener("beforeunload", hold);
    else if (dirty().length > 0) e.preventDefault();
  };
  addEventListener("beforeunload", hold);

  // The connection can drop or return while the form is open: the button and the message follow it.
  const link = (): void => {
    if (!form.isConnected) {
      removeEventListener("online", link);
      removeEventListener("offline", link);
      return;
    }
    if (!saving) idle();
    refresh();
  };
  addEventListener("online", link);
  addEventListener("offline", link);

  refresh();
  return form;
}

function langName(code: string): string {
  // i18n: admin.lang.en admin.lang.vi
  return (langs() as readonly string[]).includes(code) ? t(`admin.lang.${code}`) : code;
}
