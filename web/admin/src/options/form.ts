/**
 * The option form engine: one control per declared field type, shared by Appearance (a theme's
 * options) and the plugin tabs. The form is drawn from `option_declarations`; nothing here knows
 * a theme or plugin by name. Values go out typed as the server checks them (docs/ARCHITECTURE.md,
 * "Option declarations"): numbers as numbers, toggles as booleans, colors as `{light, dark}`.
 */

import type { Api, ApiError, Json, Result } from "../api/client";
import type { OptionDeclaration, OptionDeclarations } from "../api/model.gen";
import { h } from "../dom";
import { t } from "../i18n";
import { hasIcon, icon } from "../icons";
import { seg } from "../seg";

type Val = unknown;

interface Control {
  node: HTMLElement;
  /** The focusable elements that carry aria-invalid and aria-describedby. */
  targets: HTMLElement[];
  /** The control is a group of inputs, so the label names the group instead of one input. */
  group: boolean;
  get(): Val;
  set(v: Val): void;
}

export interface FormOptions {
  api: Api;
  /** `/api/v1/themes/<name>` or `/api/v1/plugins/<name>`; options live under `/options`. */
  path: string;
  decls: OptionDeclarations;
  /** Stored values; a key that is absent shows its default. */
  stored: Record<string, unknown>;
  /** Read at each use: a page that goes offline keeps its values but cannot save. */
  offline: () => boolean;
  /** Makes element ids unique when several forms (plugin tabs) share a page. */
  prefix: string;
}

const same = (a: Val, b: Val): boolean => JSON.stringify(a) === JSON.stringify(b);
const HEX = /^#(?:[0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})$/i;
const MODES = ["light", "dark"] as const;
/** At most this many choices draw as a segmented control; more draw as a menu. */
const SEG_MAX = 3;

/** The declared default, or a neutral value for a type whose declaration left it out. */
function defaultOf(d: OptionDeclaration): Val {
  if (d.default !== null && d.default !== undefined) return normal(d, d.default);
  switch (d.type) {
    case "number":
      return d.min ?? 0;
    case "toggle":
      return false;
    case "choice":
      return d.choices[0]?.value ?? "";
    case "color":
      return { light: "#000000", dark: "#ffffff" };
    case "list":
      return [];
    default:
      return "";
  }
}

/** Colors are compared as `{light, dark}` whatever key order the server stored them in. */
function normal(d: OptionDeclaration, v: Val): Val {
  if (d.type !== "color" || typeof v !== "object" || v === null) return v;
  const o = v as Record<string, unknown>;
  return { light: o["light"], dark: o["dark"] };
}

function defaultText(d: OptionDeclaration, v: Val): string {
  switch (d.type) {
    case "toggle":
      return v === true ? t("admin.options.on") : t("admin.options.off");
    case "choice":
      return d.choices.find((c) => c.value === v)?.label ?? String(v);
    case "color": {
      const c = v as { light: string; dark: string };
      return `${c.light} / ${c.dark}`;
    }
    case "list":
      return Array.isArray(v) && v.length > 0 ? v.join(", ") : t("admin.options.none");
    case "image":
      return v === "" || v === null ? t("admin.options.none") : String(v);
    default:
      return v === "" || v === null ? t("admin.options.empty_text") : String(v);
  }
}

// --- one control per field type ---

type Make = (d: OptionDeclaration, id: string, def: Val, changed: () => void) => Control;

const text: Make = (d, id, def, changed) => {
  const el = h("input", { class: "input", type: "text", id, autocomplete: "off", maxlength: d.max_len, placeholder: def === "" ? null : String(def) });
  el.addEventListener("input", changed);
  // A stray space at either end is never meant.
  return { node: el, targets: [el], group: false, get: () => el.value.trim(), set: (v) => void (el.value = String(v ?? "")) };
};

const area: Make = (d, id, def, changed) => {
  const el = h("textarea", { class: "textarea", id, rows: 3, maxlength: d.max_len, placeholder: def === "" ? null : String(def) });
  el.addEventListener("input", changed);
  return { node: el, targets: [el], group: false, get: () => el.value, set: (v) => void (el.value = String(v ?? "")) };
};

const num: Make = (d, id, def, changed) => {
  const el = h("input", { class: "input", type: "number", id, inputmode: "decimal", min: d.min, max: d.max, step: d.step ?? "any", placeholder: String(def) });
  el.addEventListener("input", changed);
  return {
    node: el,
    targets: [el],
    group: false,
    get: () => (el.value.trim() === "" ? null : Number(el.value)),
    set: (v) => void (el.value = v === null || v === undefined ? "" : String(v)),
  };
};

/** Media ids are typed in: the admin has no reusable picture picker yet (the editor's is bound to the post). */
const image: Make = (_d, id, _def, changed) => {
  const el = h("input", { class: "input", type: "number", id, inputmode: "numeric", min: 1, step: 1, placeholder: t("admin.options.none") });
  el.addEventListener("input", changed);
  return {
    node: el,
    targets: [el],
    group: false,
    get: () => (el.value.trim() === "" ? "" : Number(el.value)),
    set: (v) => void (el.value = v === "" || v === null || v === undefined ? "" : String(v)),
  };
};

const choice: Make = (d, id, def, changed) => {
  const values = d.choices.map((c) => c.value);
  const label = (v: string): string => d.choices.find((c) => c.value === v)?.label ?? v;
  if (values.length <= SEG_MAX) {
    let cur = String(def);
    const node = seg(d.label, values, cur, label, (v) => {
      cur = v;
      changed();
    });
    node.id = id;
    const keys = [...node.querySelectorAll("button")];
    return {
      node,
      targets: [node],
      group: true,
      get: () => cur,
      set: (v) => {
        cur = String(v);
        keys.forEach((b, i) => b.setAttribute("aria-pressed", String(values[i] === cur)));
      },
    };
  }
  const el = h("select", { class: "select", id }, ...d.choices.map((c) => h("option", { value: c.value }, c.label)));
  el.addEventListener("change", changed);
  return { node: el, targets: [el], group: false, get: () => el.value, set: (v) => void (el.value = String(v)) };
};

/** `#abc` and `#abcd` widen to `#aabbcc` for the native picker, which takes only six digits. */
function forPicker(hex: string): string | undefined {
  if (/^#[0-9a-f]{6}$/i.test(hex)) return hex.toLowerCase();
  const short = /^#([0-9a-f])([0-9a-f])([0-9a-f])[0-9a-f]?$/i.exec(hex);
  return short ? `#${short[1]}${short[1]}${short[2]}${short[2]}${short[3]}${short[3]}`.toLowerCase() : undefined;
}

const color: Make = (d, id, def, changed) => {
  const hexes: HTMLInputElement[] = [];
  const pickers: HTMLInputElement[] = [];
  const fields = MODES.map((mode) => {
    const name = mode === "light" ? t("admin.options.mode_light") : t("admin.options.mode_dark");
    const hex = h("input", { class: "input mono", type: "text", id: `${id}-${mode}`, autocomplete: "off", spellcheck: "false", maxlength: 9, placeholder: (def as Record<string, string>)[mode] ?? null });
    const pick = h("input", { class: "input swatch", type: "color", "aria-label": t("admin.options.color_pick", { mode: name }) });
    pick.addEventListener("input", () => {
      hex.value = pick.value;
      changed();
    });
    hex.addEventListener("input", () => {
      const p = forPicker(hex.value.trim());
      if (p) pick.value = p;
      changed();
    });
    hexes.push(hex);
    pickers.push(pick);
    return h("div", { class: "field" }, h("label", { for: hex.id }, name), h("div", { class: "list-row" }, pick, hex));
  });
  const node = h("div", { class: "two", id, role: "group" }, ...fields);
  return {
    node,
    targets: [node],
    group: true,
    get: () => ({ light: hexes[0]?.value.trim() ?? "", dark: hexes[1]?.value.trim() ?? "" }),
    set: (v) => {
      const o = (v ?? {}) as Record<string, string>;
      MODES.forEach((m, i) => {
        const val = o[m] ?? "";
        const hex = hexes[i];
        const pick = pickers[i];
        if (hex) hex.value = val;
        const p = forPicker(val);
        if (pick && p) pick.value = p;
      });
    },
  };
};

const list: Make = (d, id, _def, changed) => {
  const rows = h("div", { class: "list-f" });
  const add = h("button", { class: "btn", type: "button" }, icon("plus"), t("admin.options.list_add"));
  const inputs = (): HTMLInputElement[] => [...rows.querySelectorAll("input")];
  const sync = (): void => {
    inputs().forEach((el, i) => {
      el.setAttribute("aria-label", t("admin.options.list_item", { n: i + 1 }));
      el.parentElement?.querySelector("button")?.setAttribute("aria-label", t("admin.options.list_remove", { n: i + 1 }));
    });
    add.disabled = d.max_items !== null && inputs().length >= d.max_items;
  };
  const row = (value: string): HTMLInputElement => {
    const el = h("input", { class: "input", type: "text", autocomplete: "off", maxlength: d.max_len });
    el.value = value;
    el.addEventListener("input", changed);
    const del = h("button", { class: "ib", type: "button" }, icon("close"));
    del.addEventListener("click", () => {
      const at = inputs().indexOf(el);
      el.parentElement?.remove();
      sync();
      (inputs()[at] ?? inputs()[at - 1] ?? add).focus();
      changed();
    });
    rows.append(h("div", { class: "list-row" }, el, del));
    sync();
    return el;
  };
  add.addEventListener("click", () => {
    row("").focus();
    changed();
  });
  const node = h("div", { class: "list-f", id, role: "group" }, rows, h("div", { class: "btn-row" }, add));
  return {
    node,
    targets: [node],
    group: true,
    // A blank line is not an item.
    get: () => inputs().map((el) => el.value.trim()).filter((v) => v !== ""),
    set: (v) => {
      rows.replaceChildren();
      for (const s of Array.isArray(v) ? v : []) row(String(s));
      sync();
    },
  };
};

const MAKE: Record<string, Make> = { short_text: text, long_text: area, number: num, choice, color, image, list };

/** A toggle is its own label: the row is the switch's tap area. */
function toggle(d: OptionDeclaration, id: string, changed: () => void): Control & { row: HTMLElement } {
  const sw = h("input", { class: "sw", type: "checkbox", id, role: "switch" });
  sw.addEventListener("change", changed);
  const row = h("label", { class: "opt between", for: id }, h("span", {}, d.label), h("span", { class: "sw-hit" }, sw));
  return { node: sw, row, targets: [sw], group: false, get: () => sw.checked, set: (v) => void (sw.checked = v === true) };
}

// --- client checks, mirroring the server's so a bad value never leaves the page ---

function problem(d: OptionDeclaration, v: Val): string | undefined {
  if (d.type === "number") {
    if (typeof v !== "number" || Number.isNaN(v)) return t("admin.options.err_number");
    const lo = d.min !== null && v < d.min;
    const hi = d.max !== null && v > d.max;
    if (lo || hi) {
      if (d.min !== null && d.max !== null) return t("admin.options.err_range", { min: d.min, max: d.max });
      return d.min !== null ? t("admin.options.err_min", { min: d.min }) : t("admin.options.err_max", { max: d.max ?? 0 });
    }
    if (d.step !== null && d.step > 0) {
      const steps = (v - (d.min ?? 0)) / d.step;
      if (Math.abs(steps - Math.round(steps)) > 1e-9) return t("admin.options.err_step", { step: d.step, from: d.min ?? 0 });
    }
  } else if (d.type === "color") {
    const c = v as Record<string, string>;
    if (!MODES.every((m) => HEX.test(c[m] ?? ""))) return t("admin.options.err_color");
  } else if (d.type === "image") {
    if (v !== "" && !(typeof v === "number" && Number.isInteger(v) && v > 0)) return t("admin.options.err_image");
  }
  return undefined;
}

/** The one plain message for a failed write. */
export function explain(e: ApiError): string {
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

// --- the form ---

interface Entry {
  d: OptionDeclaration;
  ctl: Control;
  err: HTMLElement;
  hintId: string;
  reset: HTMLButtonElement;
  def: Val;
}

export function optionsForm(o: FormOptions): HTMLFormElement {
  const base: Record<string, Val> = {};
  const entries: Entry[] = [];
  let saving = false;

  const build = (d: OptionDeclaration): { entry: Entry; node: HTMLElement } => {
    const id = `${o.prefix}-${d.key}`;
    const def = defaultOf(d);
    const hintId = `${id}-hint`;
    const err = h("span", { class: "err", id: `${id}-err` });
    err.hidden = true;
    const entry = { d, err, hintId, def } as Entry;
    const changed = (): void => {
      setError(entry, undefined);
      if (!saving) idle();
      refresh();
    };
    const ctl: Control & { row?: HTMLElement } = d.type === "toggle" ? toggle(d, id, changed) : (MAKE[d.type] as Make)(d, id, def, changed);
    entry.ctl = ctl;
    ctl.set(o.stored[d.key] !== undefined ? normal(d, o.stored[d.key]) : def);
    base[d.key] = ctl.get();
    const desc = [d.help ? hintId : "", d.type === "list" && d.max_items !== null ? `${id}-max` : "", `${id}-def`].filter(Boolean).join(" ");
    for (const el of ctl.targets) el.setAttribute("aria-describedby", desc);
    if (ctl.group) ctl.node.setAttribute("aria-labelledby", `${id}-label`);

    const reset = h("button", { class: "linkbtn", type: "button", "aria-label": t("admin.options.reset_field", { name: d.label }) }, t("admin.options.reset"));
    reset.addEventListener("click", () => void resetOne(entry));
    entry.reset = reset;

    const label = d.type === "toggle" ? (ctl as { row?: HTMLElement }).row : ctl.group ? h("span", { class: "label", id: `${id}-label` }, d.label) : h("label", { for: id }, d.label);
    const note = [
      d.help ? h("span", { class: "hint", id: hintId }, d.help) : null,
      d.type === "list" && d.max_items !== null ? h("span", { class: "hint", id: `${id}-max` }, t("admin.options.list_max", { max: d.max_items })) : null,
      d.type === "image" ? h("span", { class: "hint" }, t("admin.options.image_hint")) : null,
    ];
    const defHint = h("span", { class: "hint", id: `${id}-def` }, t("admin.options.default_label", { value: defaultText(d, def) }));
    const node = h("div", { class: "field" }, label, d.type === "toggle" ? null : ctl.node, ...note, err, h("div", { class: "btn-row" }, defHint, reset));
    return { entry, node };
  };

  const setError = (e: Entry, msg: string | undefined): void => {
    e.err.hidden = msg === undefined;
    e.err.textContent = msg ?? "";
    const ids = e.ctl.targets[0]?.getAttribute("aria-describedby")?.split(" ").filter((x) => x !== e.err.id) ?? [];
    for (const el of e.ctl.targets) {
      if (msg === undefined) el.removeAttribute("aria-invalid");
      else el.setAttribute("aria-invalid", "true");
      el.setAttribute("aria-describedby", (msg === undefined ? ids : [...ids, e.err.id]).join(" "));
    }
  };

  // Groups in declared order; options keep the order the API gives; one without a group comes first.
  const sections: HTMLElement[] = [];
  const built = o.decls.options.map(build);
  entries.push(...built.map((b) => b.entry));
  const place = (key: string | null, heading: HTMLElement | null): void => {
    const mine = built.filter((b) => (b.entry.d.group ?? null) === key);
    if (mine.length === 0) return;
    sections.push(h("section", { class: "sec", "aria-labelledby": heading?.id ?? null }, heading, ...mine.map((b) => b.node)));
  };
  place(null, null);
  const known = new Set(o.decls.groups.map((g) => g.key));
  for (const g of o.decls.groups) {
    const heading = h("h3", { class: "grp", id: `${o.prefix}-g-${g.key}` }, g.icon && hasIcon(g.icon) ? icon(g.icon) : null, g.label);
    place(g.key, heading);
  }
  // An option naming a group the theme did not declare still shows.
  for (const key of new Set(entries.map((e) => e.d.group).filter((k): k is string => k !== null && !known.has(k)))) place(key, null);

  const status = h("span", { class: "hint", role: "status" });
  const save = h("button", { class: "btn primary", type: "submit" }, t("admin.settings.save"));
  const resetAll = h("button", { class: "btn", type: "button" }, t("admin.options.reset_all"));
  const sure = h("button", { class: "btn danger", type: "button" }, t("admin.options.reset_all_yes"));
  const keep = h("button", { class: "btn", type: "button" }, t("admin.common.cancel"));
  const ask = h("div", { class: "btn-row", role: "group", "aria-label": t("admin.options.reset_all") }, h("span", { class: "hint" }, t("admin.options.reset_all_q")), sure, keep);
  ask.hidden = true;
  const actions = h("div", { class: "btn-row" }, save, resetAll, status);

  const dirty = (): Entry[] => entries.filter((e) => !same(e.ctl.get(), base[e.d.key]));
  const idle = (): void => {
    status.className = "hint";
    status.textContent = o.offline() ? t("admin.settings.offline") : "";
  };
  const refresh = (): void => {
    const off = saving || o.offline();
    save.disabled = off || dirty().length === 0;
    resetAll.disabled = off;
    sure.disabled = off;
    for (const e of entries) e.reset.disabled = off || (same(e.ctl.get(), e.def) && same(base[e.d.key], e.def));
  };

  const put = (e: Entry, v: Val): Promise<Result<undefined>> => o.api.call<undefined>("PUT", `${o.path}/options/${e.d.key}`, { value: v as Json });

  /** Per-field reset: the declared default is written like any other value (no per-key DELETE). */
  const resetOne = async (e: Entry): Promise<void> => {
    if (saving || o.offline()) return;
    saving = true;
    refresh();
    e.ctl.set(e.def);
    setError(e, undefined);
    const r = await put(e, e.def);
    saving = false;
    if (r.ok) base[e.d.key] = e.ctl.get();
    else {
      setError(e, explain(r.error));
      status.className = "err";
      status.textContent = t("admin.settings.not_saved");
    }
    refresh();
  };

  const submit = async (): Promise<void> => {
    const todo = dirty();
    if (todo.length === 0) return;
    let firstBad: Entry | undefined;
    for (const e of todo) {
      const msg = problem(e.d, e.ctl.get());
      if (msg !== undefined) {
        setError(e, msg);
        firstBad ??= e;
      }
    }
    if (firstBad) {
      focus(firstBad);
      return;
    }
    saving = true;
    status.className = "hint";
    status.textContent = t("admin.settings.saving");
    refresh();
    for (const e of todo) {
      const v = e.ctl.get();
      const r = await put(e, v);
      if (r.ok) base[e.d.key] = v;
      else {
        setError(e, explain(r.error));
        firstBad ??= e;
      }
    }
    saving = false;
    if (firstBad) {
      status.className = "err";
      status.textContent = t("admin.settings.not_saved");
      focus(firstBad);
    } else {
      status.textContent = t("admin.settings.saved");
    }
    refresh();
  };

  const focus = (e: Entry): void => (e.ctl.group ? e.ctl.node.querySelector<HTMLElement>("input, button, select") : e.ctl.targets[0])?.focus();

  const form = h("form", { class: "opts", novalidate: true }, ...sections, h("div", { class: "sec" }, actions, ask));
  form.addEventListener("submit", (ev) => {
    ev.preventDefault();
    if (!saving && !o.offline()) void submit();
  });

  resetAll.addEventListener("click", () => {
    actions.hidden = true;
    ask.hidden = false;
    keep.focus();
  });
  const closeAsk = (): void => {
    ask.hidden = true;
    actions.hidden = false;
    resetAll.focus();
  };
  keep.addEventListener("click", closeAsk);
  sure.addEventListener("click", () => {
    if (saving || o.offline()) return;
    void (async () => {
      saving = true;
      status.className = "hint";
      status.textContent = t("admin.options.resetting");
      refresh();
      const r = await o.api.call<undefined>("DELETE", `${o.path}/options`);
      saving = false;
      if (r.ok) {
        for (const e of entries) {
          e.ctl.set(e.def);
          base[e.d.key] = e.ctl.get();
          setError(e, undefined);
        }
        status.textContent = t("admin.options.reset_done");
      } else {
        status.className = "err";
        status.textContent = explain(r.error);
      }
      closeAsk();
      refresh();
    })();
  });

  // Unsaved edits ask before the page is closed (as General does); a screen left by a link drops them.
  const hold = (ev: Event): void => {
    if (!form.isConnected) removeEventListener("beforeunload", hold);
    else if (dirty().length > 0) ev.preventDefault();
  };
  addEventListener("beforeunload", hold);

  // The connection can drop or return while the form is open: the buttons and the message follow it.
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

  idle();
  refresh();
  return form;
}
