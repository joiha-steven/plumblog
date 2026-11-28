/**
 * The System log screen (its own chunk, loaded by load.ts): search and filters kept in the URL,
 * entries newest first under day headings, a detail that opens in place, "Load more".
 * Data: api.logList; the API's rules are in docs/ARCHITECTURE.md, System log.
 */

import type { Api, ApiError } from "../api/client";
import { LOG_KINDS, type LogEntry, type Role } from "../api/model.gen";
import { type Child, h } from "../dom";
import { lang, t } from "../i18n";
import { seg } from "../seg";
import { settingsNav } from "../settings";
import { empty, failed, loadingRows, toast } from "../ui";
import { errorMessage, kindLabel } from "./labels";
import { levelMark } from "./mark";
import { type Filters, KIND_GROUPS, LEVELS, type LevelFilter, NO_FILTERS, Q_MAX, RANGES, type Range, filtersOf, isFiltered, queryOf, searchOf } from "./query";

export interface Mount {
  /** The header slot the search field goes in (rightmost control). */
  find: HTMLElement;
  /** The `.settings-box` under the header: the section list and the log go inside it. */
  body: HTMLElement;
  api: Api;
  role: Role;
}

/** Typing waits this long before it asks again. */
const DEBOUNCE_MS = 300;
const REDACTED = "{redacted}";

const cap = (s: string): string => s.charAt(0).toLocaleUpperCase(lang()) + s.slice(1);
const time = (ms: number): string => new Intl.DateTimeFormat(lang(), { hour: "2-digit", minute: "2-digit" }).format(ms);
const full = (ms: number, utc: boolean): string =>
  new Intl.DateTimeFormat(lang(), { dateStyle: "medium", timeStyle: "medium", ...(utc ? { timeZone: "UTC" } : {}) }).format(ms);

/** One local calendar day, as a key. */
export const dayKey = (ms: number): string => {
  const d = new Date(ms);
  return `${d.getFullYear()}-${d.getMonth()}-${d.getDate()}`;
};

/** "Today, Saturday 28 November 2026"; older days are the date alone. */
export function dayTitle(ms: number, now = Date.now()): string {
  const date = new Intl.DateTimeFormat(lang(), { weekday: "long", day: "numeric", month: "long", year: "numeric" }).format(ms);
  const start = (x: number): number => new Date(new Date(x).setHours(0, 0, 0, 0)).getTime();
  const back = Math.round((start(now) - start(ms)) / 86_400_000);
  if (back === 0 || back === 1) return t("admin.syslog.day_rel", { rel: cap(new Intl.RelativeTimeFormat(lang(), { numeric: "auto" }).format(-back, "day")), date });
  return cap(date);
}

const who = (e: LogEntry): string => e.actor_name ?? (e.actor_id !== null ? t("admin.syslog.who_deleted") : t("admin.syslog.who_system"));

const levelFilterName = (l: LevelFilter): string =>
  // i18n: admin.syslog.level_all admin.syslog.level_warning admin.syslog.level_error
  t(`admin.syslog.level_${l}`);

const rangeName = (r: Range): string =>
  // i18n: admin.syslog.range_24h admin.syslog.range_7d admin.syslog.range_30d admin.syslog.range_all
  t(`admin.syslog.range_${r}`);

const groupName = (id: (typeof KIND_GROUPS)[number]["id"]): string =>
  // i18n: admin.syslog.group_accounts admin.syslog.group_content admin.syslog.group_settings admin.syslog.group_system admin.syslog.group_webhooks admin.syslog.group_errors
  t(`admin.syslog.group_${id}`);

async function copy(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
    toast(t("admin.syslog.copied"));
  } catch (e) {
    console.error("admin: copying from the system log failed", e);
    toast(t("admin.syslog.copy_failed"), 5000);
  }
}

const mono = (text: string): HTMLElement => h("span", { class: "log-mono" }, text);

/** The opened entry: every fact the row leaves out, and the two copy buttons. */
function details(e: LogEntry): HTMLElement {
  const line = (label: Child, ...value: Child[]): Child[] => [h("dt", {}, label), h("dd", {}, ...value)];
  const rows: Child[] = [
    line(t("admin.syslog.d_when"), full(e.at, false)),
    line(t("admin.syslog.d_utc"), full(e.at, true)),
    line(t("admin.syslog.d_level"), levelMark(e.level)),
    line(t("admin.syslog.d_kind"), kindLabel(e.kind), " ", mono(e.kind)),
    line(t("admin.syslog.d_who"), who(e)),
    e.error_code === null ? null : line(t("admin.syslog.d_error"), errorMessage(e.error_code), " ", mono(e.error_code)),
    line(
      t("admin.syslog.d_request"),
      h(
        "span",
        { class: "log-copy" },
        mono(e.request_id),
        h("button", { class: "btn quiet", type: "button", onclick: () => void copy(e.request_id) }, t("admin.syslog.copy")),
      ),
    ),
  ];
  // Keys are shown as stored (mono); the marker for a removed value is shown as words, muted.
  const fields = e.fields.map((f) => line(mono(f.key), f.value === REDACTED ? h("span", { class: "log-redacted" }, t("admin.syslog.redacted")) : f.value));
  return h(
    "div",
    { class: "log-inner" },
    h("dl", { class: "kv log-kv" }, rows),
    e.fields.length ? [h("h3", { class: "cap" }, t("admin.syslog.d_fields")), h("dl", { class: "kv log-kv" }, fields)] : null,
    h("p", { class: "log-hint" }, t("admin.syslog.request_hint")),
    h(
      "div",
      { class: "btn-row" },
      h("button", { class: "btn", type: "button", onclick: () => void copy(JSON.stringify({ time_utc: new Date(e.at).toISOString(), ...e })) }, t("admin.syslog.copy_ai")),
    ),
  );
}

/** One entry: a button that opens its detail in place (aria-expanded, aria-controls). */
function item(e: LogEntry): HTMLElement {
  const id = `log-d-${e.id}`;
  const region = h("div", { class: "log-detail", id, role: "region", "aria-label": t("admin.syslog.detail_label"), hidden: true });
  let built = false;
  const button = h(
    "button",
    {
      class: "log-btn",
      type: "button",
      "aria-expanded": "false",
      "aria-controls": id,
      onclick: () => {
        const open = button.getAttribute("aria-expanded") !== "true";
        if (open && !built) {
          built = true;
          region.append(details(e));
        }
        button.setAttribute("aria-expanded", String(open));
        region.hidden = !open;
        li.classList.toggle("open", open);
      },
    },
    h("span", { class: "log-level" }, levelMark(e.level)),
    h(
      "span",
      { class: "log-main" },
      h("span", { class: "log-kind" }, kindLabel(e.kind)),
      h("span", { class: "log-subject" }, e.subject),
      e.error_code === null ? null : h("span", { class: "log-err" }, errorMessage(e.error_code)),
    ),
    h("span", { class: "log-meta" }, h("time", { class: "log-time", datetime: new Date(e.at).toISOString() }, time(e.at)), h("span", { class: "log-who" }, who(e))),
  );
  const li = h("li", { class: "log-item" }, button, region);
  return li;
}

/**
 * The entries for one filter set, with its own loading, empty and error states. A new filter
 * is a new list, so a late answer for the old one lands in a box nobody sees.
 */
function entryList(api: Api, f: Filters, clear: () => void): HTMLElement {
  const box = h("div");
  const days = h("div", { class: "log-days" });
  const tail = h("div", { class: "more" });
  const more = h("button", { class: "btn", type: "button", onclick: () => void load(true) }, t("admin.list.more"));
  // One clock for every page of this list, so "Last 24 hours" does not drift between loads.
  const now = Date.now();
  let cursor: string | null = null;
  let shown = 0;
  let day = "";
  let rows = h("ul", { class: "log-rows" });

  /** Adds entries under their day headings; the first new row's button, for focus. */
  const add = (entries: LogEntry[]): HTMLElement | undefined => {
    const added: HTMLElement[] = [];
    for (const e of entries) {
      const key = dayKey(e.at);
      if (key !== day) {
        day = key;
        rows = h("ul", { class: "log-rows" });
        days.append(h("section", { class: "log-day" }, h("h2", { class: "cap log-day-h" }, dayTitle(e.at)), rows));
      }
      const li = item(e);
      rows.append(li);
      added.push(li);
    }
    shown += entries.length;
    return added[0]?.querySelector("button") ?? undefined;
  };

  const fail = (err: ApiError, append: boolean): void => {
    if (!append) {
      box.replaceChildren(failed(err, () => void load(false)));
      return;
    }
    tail.replaceChildren(failed(err, () => void load(true)));
    tail.querySelector("button")?.focus();
  };

  const load = async (append: boolean): Promise<void> => {
    if (append) {
      more.disabled = true;
      more.setAttribute("aria-busy", "true");
      more.textContent = t("admin.list.loading_more");
      tail.replaceChildren(more);
    } else {
      shown = 0;
      day = "";
      days.replaceChildren();
      box.replaceChildren(loadingRows());
    }
    const r = await api.logList(queryOf(f, now, append && cursor !== null ? cursor : undefined));
    if (!r.ok) return fail(r.error, append);
    cursor = r.value.next_cursor;
    if (shown === 0 && r.value.entries.length === 0) {
      box.replaceChildren(
        isFiltered(f)
          ? empty({
              icon: "note",
              title: t("admin.syslog.none_title"),
              hint: t("admin.syslog.none_hint"),
              action: h("button", { class: "btn", type: "button", onclick: clear }, t("admin.syslog.clear")),
            })
          : empty({ icon: "note", title: t("admin.syslog.empty_title"), hint: t("admin.syslog.empty_hint") }),
      );
      return;
    }
    if (!box.contains(days)) box.replaceChildren(days, tail);
    const first = add(r.value.entries);
    more.disabled = false;
    more.removeAttribute("aria-busy");
    more.textContent = t("admin.list.more");
    tail.replaceChildren(...(cursor !== null ? [more] : []));
    if (append) first?.focus();
  };

  void load(false);
  return box;
}

export function mountSyslog({ find, body, api, role }: Mount): void {
  let filters = filtersOf(location.search);
  const input = h("input", {
    class: "input",
    type: "search",
    "aria-label": t("admin.syslog.search"),
    placeholder: t("admin.syslog.search"),
    maxlength: String(Q_MAX),
    autocomplete: "off",
    enterkeyhint: "search",
    value: filters.q,
  });
  find.replaceChildren(input);

  let timer: ReturnType<typeof setTimeout> | undefined;
  /** A filter change: cancels a pending search and takes the text typed so far into the same history entry. */
  const change = (patch: Partial<Filters>): void => {
    clearTimeout(timer);
    commit({ ...filters, ...patch, q: input.value.trim().slice(0, Q_MAX) }, "push");
  };
  const level = seg(t("admin.syslog.filter_level"), LEVELS, filters.level, levelFilterName, (l) => change({ level: l }));
  const kind = h(
    "select",
    { class: "select auto", "aria-label": t("admin.syslog.filter_kind") },
    h("option", { value: "all" }, t("admin.syslog.kind_all")),
    KIND_GROUPS.map((g) => h("optgroup", { label: groupName(g.id) }, g.kinds.map((k) => h("option", { value: k }, kindLabel(k))))),
  );
  const range = h(
    "select",
    { class: "select auto", "aria-label": t("admin.syslog.filter_range") },
    RANGES.map((r) => h("option", { value: r }, rangeName(r))),
  );
  const results = h("div");
  const clear = (): void => {
    commit({ ...NO_FILTERS }, "push");
    // The button that was pressed is gone with the no-results state.
    input.focus();
  };

  /** Makes the controls show `filters`. */
  const sync = (): void => {
    level.querySelectorAll("button").forEach((b, i) => b.setAttribute("aria-pressed", String(LEVELS[i] === filters.level)));
    kind.value = filters.kind;
    range.value = filters.range;
    if (input.value.trim() !== filters.q) input.value = filters.q;
  };
  const commit = (next: Filters, how: "push" | "replace"): void => {
    if (!input.isConnected) return; // the screen is gone (a timer fired late)
    const changed = searchOf(next) !== searchOf(filters);
    filters = next;
    const url = location.pathname + searchOf(next);
    if (url !== location.pathname + location.search) {
      // A new history entry, so Back returns to the filters before; the router reads the same
      // address on popstate and draws the screen from it.
      if (how === "push") history.pushState(null, "", url);
      else history.replaceState(null, "", url);
    }
    if (changed) {
      sync();
      results.replaceChildren(entryList(api, filters, clear));
    }
  };

  kind.addEventListener("change", () => change({ kind: LOG_KINDS.find((k) => k === kind.value) ?? "all" }));
  range.addEventListener("change", () => change({ range: RANGES.find((r) => r === range.value) ?? "all" }));
  const search = (how: "push" | "replace"): void => {
    clearTimeout(timer);
    commit({ ...filters, q: input.value.trim().slice(0, Q_MAX) }, how);
  };
  input.addEventListener("input", () => {
    clearTimeout(timer);
    timer = setTimeout(() => search("replace"), DEBOUNCE_MS);
  });
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      search("push");
    }
  });

  sync();
  body.replaceChildren(h("div", { class: "settings" }, settingsNav(role, "log"), h("div", { class: "settings-main" }, h("div", { class: "filters inset" }, level, kind, range), results)));
  results.replaceChildren(entryList(api, filters, clear));
}
