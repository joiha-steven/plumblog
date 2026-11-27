/**
 * The posts and pages lists: status filter kept in the URL, paging with "Load more", and the
 * strip of drafts that exist only in this browser. Screens: screens.ts; data: api.posts.
 */

import type { Api, ApiError, Result } from "./api/client";
import type { PostKind, PostListQuery, PostPage, PostStatus, PostSummary } from "./api/model.gen";
import { h } from "./dom";
import { type DraftEntry, listDrafts } from "./editor/drafts";
import { lang, t } from "./i18n";
import { HREF, editHref } from "./router";
import { empty, failed, loadingRows } from "./ui";

export const FILTERS = ["all", "draft", "scheduled", "published", "trashed"] as const;
export type Filter = (typeof FILTERS)[number];

/** Rows asked per request, and the rows a load aims to add. */
const PAGE = 20;
/** "All" cannot ask the API to leave trash out, so a page can come back short: a load keeps
 * asking for more, at most this many extra times, before it hands the rest to "Load more". */
const MAX_EXTRA = 5;
/** Drafts kept on this device that the posts list shows. */
const STRIP = 3;

/** The first request a list makes for a filter, before any "Load more". */
const firstQuery = (kind: PostKind, filter: Filter): PostListQuery => ({ kind, limit: PAGE, ...(filter === "all" ? {} : { status: filter }) });

/** A first page asked for at boot, used once by the list that asks the same. */
let early: { key: string; page: Promise<Result<PostPage>> } | undefined;

/**
 * Starts the list's first request at boot, beside the signed-in check, instead of after it:
 * one round trip less before the rows show (the excerpts are the largest paint on phones).
 */
export function prefetch(api: Api, kind: PostKind, search: string): void {
  const q = firstQuery(kind, kind === "post" ? filterOf(search) : "all");
  early = { key: JSON.stringify(q), page: api.posts(q) };
}

/** The prefetched page when `q` is the request it made; each prefetch serves once. */
function ask(api: Api, q: PostListQuery): Promise<Result<PostPage>> {
  const hit = early?.key === JSON.stringify(q) ? early : undefined;
  early = undefined;
  return hit ? hit.page : api.posts(q);
}

/** The filter in `?status=`; anything else is "all". */
export function filterOf(search: string): Filter {
  const s = new URLSearchParams(search).get("status");
  return FILTERS.find((f) => f === s) ?? "all";
}

const statusName = (s: PostStatus): string =>
  // i18n: admin.status.draft admin.status.scheduled admin.status.published admin.status.trashed
  t(`admin.status.${s}`);

const lower = (s: PostStatus): string =>
  // i18n: admin.list.lc.draft admin.list.lc.scheduled admin.list.lc.published admin.list.lc.trashed
  t(`admin.list.lc.${s}`);

const date = (ms: number, time = false): string =>
  new Intl.DateTimeFormat(lang(), time ? { dateStyle: "medium", timeStyle: "short" } : { dateStyle: "medium" }).format(ms);

/** "5 minutes ago" in the page language; a month or more ago reads as the date. */
export function ago(ms: number, now = Date.now()): string {
  const secs = Math.round((ms - now) / 1000);
  const abs = Math.abs(secs);
  const f = new Intl.RelativeTimeFormat(lang(), { numeric: "auto" });
  if (abs < 60) return f.format(0, "second");
  if (abs < 3600) return f.format(Math.round(secs / 60), "minute");
  if (abs < 86400) return f.format(Math.round(secs / 3600), "hour");
  if (abs < 30 * 86400) return f.format(Math.round(secs / 86400), "day");
  return date(ms);
}

/** The date column: a draft shows when it was last edited, the rest the date that matters. */
function whenOf(p: PostSummary): string {
  switch (p.status) {
    case "draft":
      return t("admin.list.edited", { when: ago(p.updated_at) });
    case "scheduled":
      return date(p.published_at ?? p.updated_at, true);
    case "published":
      return date(p.published_at ?? p.updated_at);
    case "trashed":
      return date(p.updated_at);
  }
}

/** Title / status / date / language, aligned so the eye can run down each column. */
function row(p: PostSummary): HTMLElement {
  return h(
    "a",
    { class: "row", href: editHref(p.id) },
    h("div", { class: "row-main" }, h("span", { class: "row-title", lang: p.lang }, p.title || t("admin.posts.untitled")), p.excerpt ? h("span", { class: "row-ex", lang: p.lang }, p.excerpt) : null),
    h("div", { class: "row-meta" }, h("span", {}, h("span", { class: `st ${p.status}` }, statusName(p.status))), h("span", { class: "row-date" }, whenOf(p))),
    h("div", { class: "row-lang" }, h("span", { class: "lang" }, p.lang)),
  );
}

const colhead = (): HTMLElement =>
  h(
    "div",
    { class: "colhead cap", "aria-hidden": "true" },
    h("span", {}, t("admin.list.col_title")),
    h("span", {}, t("admin.list.col_status")),
    h("span", {}, t("admin.list.col_date")),
    h("span", {}, t("admin.list.col_lang")),
  );

/**
 * One list of one kind and filter. It owns its loading, empty and error states; a new filter
 * is a new `pager`, so a late answer for the old one lands in a box nobody sees.
 */
function pager(api: Api, kind: PostKind, filter: Filter, emptyState: () => HTMLElement): HTMLElement {
  const box = h("div");
  const rowsEl = h("div", { class: "rows" });
  const tail = h("div", { class: "more" });
  const head = colhead();
  const more = h("button", { class: "btn", type: "button", onclick: () => void load(true) }, t("admin.list.more"));
  const rows: PostSummary[] = [];
  let cursor: string | null = null;

  /** `first`: the index of the first new row, which takes focus after "Load more". */
  const show = (err: ApiError | undefined, first?: number): void => {
    if (!rows.length) {
      box.replaceChildren(err ? failed(err, () => void load(false)) : emptyState());
      return;
    }
    if (!box.contains(rowsEl)) box.replaceChildren(head, rowsEl, tail);
    rowsEl.append(...rows.slice(rowsEl.childElementCount).map(row));
    more.disabled = false;
    more.removeAttribute("aria-busy");
    more.textContent = t("admin.list.more");
    tail.replaceChildren(...(err ? [failed(err, () => void load(true))] : cursor !== null ? [more] : []));
    if (first !== undefined && first < rows.length) (rowsEl.children[first] as HTMLElement).focus();
    else if (err) tail.querySelector("button")?.focus();
  };

  const load = async (append: boolean): Promise<void> => {
    if (append) {
      more.disabled = true;
      more.setAttribute("aria-busy", "true");
      more.textContent = t("admin.list.loading_more");
      tail.replaceChildren(more);
    } else box.replaceChildren(loadingRows());
    const before = rows.length;
    let err: ApiError | undefined;
    let added = 0;
    for (let asked = 0; ; asked++) {
      const q = { ...firstQuery(kind, filter), ...(cursor === null ? {} : { cursor }) };
      const r = await ask(api, q);
      if (!r.ok) {
        err = r.error;
        break;
      }
      const keep = filter === "all" ? r.value.posts.filter((p) => p.status !== "trashed") : r.value.posts;
      rows.push(...keep);
      added += keep.length;
      cursor = r.value.next_cursor;
      if (cursor === null || added >= PAGE || asked >= MAX_EXTRA) break;
    }
    show(err, append ? before : undefined);
  };

  void load(false);
  return box;
}

/** Where a kept draft opens: its post, or the editor with the draft id that finds it again. */
const draftHref = (d: DraftEntry): string => (d.post !== undefined ? editHref(d.post) : `${HREF.new}?draft=${d.newId ?? ""}`);

/** Drafts kept only in this browser, above the list; nothing is drawn when there are none. */
function deviceStrip(user: number, source: (user: number) => Promise<DraftEntry[]>): HTMLElement {
  const box = h("section", { class: "device" });
  void source(user).then(
    (all) => {
      const kept = all.slice(0, STRIP);
      if (!kept.length) return;
      const items = kept.map((d) =>
        h(
          "a",
          { class: "row", href: draftHref(d) },
          h("div", { class: "row-main" }, h("span", { class: "row-title" }, d.title || t("admin.posts.untitled")), h("span", { class: "row-ex" }, t("admin.list.device_kept", { when: ago(d.savedAt) }))),
        ),
      );
      box.append(h("h2", { class: "cap dev-label" }, t("admin.list.device_title")), h("div", { class: "rows" }, ...items));
    },
    (e: unknown) => console.error("admin: reading drafts kept on this device failed", e),
  );
  return box;
}

export interface PostsOptions {
  user: number;
  /** The "New post" button for the empty state. */
  newPost: () => HTMLElement;
  /** Where kept drafts come from; tests pass their own. */
  drafts?: (user: number) => Promise<DraftEntry[]>;
}

/** Posts, with the status filter and the strip of drafts kept on this device. */
export function postsList(api: Api, o: PostsOptions): HTMLElement {
  const root = h("div");
  const strip = deviceStrip(o.user, o.drafts ?? listDrafts);
  const body = h("div");
  const buttons = new Map<Filter, HTMLButtonElement>();
  const names: Record<Filter, string> = {
    all: t("admin.list.all"),
    draft: statusName("draft"),
    scheduled: statusName("scheduled"),
    published: statusName("published"),
    trashed: statusName("trashed"),
  };
  const emptyFor = (f: Filter): (() => HTMLElement) => () =>
    f === "all"
      ? empty({ icon: "posts", title: t("admin.posts.empty"), hint: t("admin.posts.empty_hint"), action: o.newPost() })
      : f === "trashed"
        ? empty({ icon: "posts", title: t("admin.list.empty_status", { status: t("admin.list.lc.trashed") }) })
        : empty({
            icon: "posts",
            title: t("admin.list.empty_status", { status: lower(f) }),
            hint: f === "scheduled" ? t("admin.list.empty_scheduled_hint") : t("admin.list.empty_hint"),
            action: o.newPost(),
          });
  const apply = (f: Filter): void => {
    for (const [k, b] of buttons) b.setAttribute("aria-pressed", String(k === f));
    strip.hidden = f !== "all";
    body.replaceChildren(pager(api, "post", f, emptyFor(f)));
  };
  const pick = (f: Filter): void => {
    const url = new URL(location.href);
    if (f === "all") url.searchParams.delete("status");
    else url.searchParams.set("status", f);
    const next = url.pathname + url.search;
    // A new history entry, so Back returns to the filter before; the router reads the same
    // address again on popstate and draws the list from it.
    if (next !== location.pathname + location.search) history.pushState(null, "", next);
    apply(f);
  };
  for (const f of FILTERS) buttons.set(f, h("button", { type: "button", "aria-pressed": "false", onclick: () => pick(f) }, names[f]));
  const filters = h("div", { class: "filters" }, h("div", { class: "seg", role: "group", "aria-label": t("admin.list.filter") }, ...buttons.values()));
  root.append(strip, filters, body);
  apply(filterOf(location.search));
  return root;
}

/** Pages: the same list, no filter, no "New page" until the editor can make one. */
export function pagesList(api: Api): HTMLElement {
  return pager(api, "page", "all", () => empty({ icon: "pages", title: t("admin.pages.empty"), hint: t("admin.pages.empty_hint") }));
}
