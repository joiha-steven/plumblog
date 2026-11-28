/**
 * The media library screen (a lazy chunk, like the editor and the account page): a grid of
 * the pictures, filters, "Load more", a details card with the description and delete, and a
 * full-screen viewer. Data: api.media*; the pure parts are in logic.ts.
 */

import type { Api, ApiError, Me, MediaItem } from "../api/client";
import { h } from "../dom";
import { icon } from "../icons";
import { lang, t } from "../i18n";
import { editHref } from "../router";
import { closePop, empty, failed, openPop, toast } from "../ui";
import { FILTERS, type Filter, canDelete, canUpload, dimensions, filterOf, formatBytes, kindOf, mergePage, nameOf, queryFor } from "./logic";
import { guardFileDrops, mountUpload } from "./upload";

const when = (ms: number): string => new Intl.DateTimeFormat(lang(), { dateStyle: "medium", timeStyle: "short" }).format(ms);
const metaOf = (m: MediaItem): string => `${formatBytes(m.bytes, lang())} \u00b7 ${dimensions(m)}`;

/** The no-description marker: a lit lamp and the words, so it never rests on colour alone. */
const lamp = (cls: string): HTMLElement => h("span", { class: cls }, h("span", { class: "lamp" }), t("admin.media.no_alt"));

const filterName = (f: Filter): string =>
  f === "all" ? t("admin.media.all") : f === "missing_alt" ? t("admin.media.no_alt") : t("admin.media.unused");

/** On phones and tablets the details replace the grid, so the grid's scroll place is kept by hand. */
const narrow = (): boolean => matchMedia("(max-width: 1099px)").matches;

/** Placeholder tiles while the first page loads. */
function loadingTiles(): HTMLElement {
  const tiles = Array.from({ length: 8 }, () => {
    return h("div", { class: "tile-w", "aria-hidden": "true" }, h("div", { class: "tile-ph" }), h("div", { class: "skel tile-line" }));
  });
  return h("div", { class: "grid", "aria-busy": "true" }, h("span", { class: "sr", role: "status" }, t("admin.state.loading")), ...tiles);
}

export function mountMedia(box: HTMLElement, me: Me, api: Api): void {
  let filter = filterOf(location.search);
  let items: MediaItem[] = [];
  let cursor: string | null = null;
  /** Bumped by every fresh load, so a late answer for an old filter is dropped. */
  let gen = 0;
  let selected: MediaItem | undefined;
  /** The main column's scroll before a phone opened the details, which replace the grid there. */
  let scrolled = 0;
  /** The "drop pictures here" line, for those who may upload. */
  let hint: HTMLElement | undefined;
  /** The first page failed and shows Try again; an upload must not replace that. */
  let loadFailed = false;
  /** Pictures uploaded while the first page was loading (or after it failed); merged on top when a page lands. */
  let pending: MediaItem[] = [];

  const tiles = new Map<number, HTMLElement>();
  const body = h("div", { class: "media-body" });
  const tail = h("div", { class: "more" });
  const inspector = h("aside", { class: "inspector", "aria-label": t("admin.nav.media"), hidden: true });
  const main = h("div", { class: "media-main" });
  const layout = h("div", { class: "media-layout" }, main, inspector);
  const scroller = (): HTMLElement | null => box.closest<HTMLElement>(".main");

  // ---- filters --------------------------------------------------------------------------
  const buttons = new Map<Filter, HTMLButtonElement>();
  const pick = (f: Filter): void => {
    const url = new URL(location.href);
    if (f === "all") url.searchParams.delete("show");
    else url.searchParams.set("show", f);
    const next = url.pathname + url.search;
    // A new history entry, so Back returns to the filter before (the router redraws on popstate).
    if (next !== location.pathname + location.search) history.pushState(null, "", next);
    filter = f;
    for (const [k, b] of buttons) b.setAttribute("aria-pressed", String(k === f));
    deselect(false);
    void load(false);
  };
  for (const f of FILTERS) buttons.set(f, h("button", { type: "button", "aria-pressed": String(f === filter), onclick: () => pick(f) }, filterName(f)));
  const filters = h("div", { class: "filters" }, h("div", { class: "seg", role: "group", "aria-label": t("admin.media.filter") }, ...buttons.values()));
  const bar = h("div", { class: "media-bar" }, h("div", { class: "media-bar-f" }, filters));
  main.append(bar, body, tail);
  box.append(layout);

  // ---- upload (only for those the API lets add pictures) ---------------------------------
  guardFileDrops(box, main);
  if (canUpload(me)) {
    const up = mountUpload({ api, box, main, added: addNew, existing: showExisting, show: (m) => select(m) });
    bar.append(up.button);
    bar.after(up.list);
    hint = up.hint;
  }

  // ---- grid -----------------------------------------------------------------------------
  function tile(m: MediaItem): HTMLElement {
    const name = nameOf(m.src);
    const open = h(
      "button",
      { type: "button", class: "tile", "aria-pressed": String(selected?.id === m.id), onclick: () => select(m) },
      h(
        "span",
        { class: "tile-img" },
        h("img", { src: m.src, width: m.width, height: m.height, loading: "lazy", alt: "" }),
        m.alt === "" ? lamp("badge-na") : null,
      ),
      h("span", { class: "tile-txt" }, h("span", { class: "fname" }, name), h("span", { class: "fmeta" }, metaOf(m))),
    );
    const peek = h(
      "button",
      { type: "button", class: "ib glass peek", "aria-label": t("admin.media.view", { name }), onclick: () => viewer(m, peek) },
      icon("eye"),
    );
    return h("div", { class: "tile-w" }, open, peek);
  }

  /** Draws what `items` and `cursor` hold; `err` is a failed "Load more". */
  function show(err?: ApiError, firstNew?: number): void {
    if (!items.length) {
      tiles.clear();
      tail.replaceChildren();
      body.replaceChildren(emptyState());
      return;
    }
    let grid = body.querySelector<HTMLElement>(".grid:not([aria-busy])");
    if (!grid) {
      grid = h("div", { class: "grid" });
      body.replaceChildren(grid);
      tiles.clear();
    }
    for (const m of items) {
      if (tiles.has(m.id)) continue;
      const el = tile(m);
      tiles.set(m.id, el);
      grid.append(el);
    }
    const more = h("button", { class: "btn", type: "button", onclick: () => void load(true) }, t("admin.list.more"));
    tail.replaceChildren(...(err ? [failed(err, () => void load(true))] : cursor !== null ? [more] : []));
    if (firstNew !== undefined && firstNew < items.length) tiles.get(items[firstNew]?.id ?? -1)?.querySelector<HTMLElement>(".tile")?.focus();
    else if (err) tail.querySelector("button")?.focus();
  }

  /** A picture just uploaded goes to the top of the grid (every filter takes it: it is unused and has no description). */
  function addNew(m: MediaItem): void {
    if (items.some((x) => x.id === m.id) || pending.some((x) => x.id === m.id)) return;
    // The page in flight may not know it, and a failed one shows Try again, which stays: keep it for the next page.
    if (loadFailed || body.querySelector(".grid[aria-busy]")) return void pending.push(m);
    items.unshift(m);
    const grid = body.querySelector<HTMLElement>(".grid:not([aria-busy])");
    if (!grid) return show();
    const el = tile(m);
    tiles.set(m.id, el);
    grid.prepend(el);
  }

  /** The caller already had this picture: open it, instead of a second tile. On a phone the details replace the grid, so while other files are still sending they keep it. */
  function showExisting(m: MediaItem, busy: boolean): void {
    tiles.get(m.id)?.scrollIntoView({ block: "nearest" });
    if (!busy || !narrow()) select(m);
  }

  function emptyState(): HTMLElement {
    if (filter === "missing_alt") return empty({ icon: "media", title: t("admin.media.empty_alt") });
    if (filter === "unused") return empty({ icon: "media", title: t("admin.media.empty_unused") });
    return empty({ icon: "media", title: t("admin.media.empty"), hint: t("admin.media.empty_hint") });
  }

  async function load(append: boolean): Promise<void> {
    const mine = ++gen;
    if (append) {
      const more = tail.querySelector("button");
      if (more) {
        more.disabled = true;
        more.setAttribute("aria-busy", "true");
        more.textContent = t("admin.list.loading_more");
      }
    } else {
      items = [];
      cursor = null;
      loadFailed = false;
      tail.replaceChildren();
      body.replaceChildren(loadingTiles());
    }
    const r = await api.media(queryFor(filter, cursor));
    if (mine !== gen) return;
    if (!r.ok) {
      if (r.error.code === "auth.unauthenticated") return;
      console.error(`admin media: loading the library failed: ${r.error.code} (${r.error.status})`);
      if (!items.length) {
        loadFailed = true;
        body.replaceChildren(failed(r.error, () => void load(false)));
      }
      else show(r.error);
      return;
    }
    const before = items.length;
    items = mergePage(items, r.value);
    if (!append && pending.length) {
      const known = new Set(items.map((x) => x.id));
      items = [...pending.filter((x) => !known.has(x.id)).reverse(), ...items];
      pending = [];
    }
    cursor = r.value.next_cursor;
    show(undefined, append ? before : undefined);
    // Added once the first page is drawn, below it, so it never moves anything on screen.
    if (hint && !hint.isConnected) tail.after(hint);
  }

  // ---- details --------------------------------------------------------------------------
  function select(m: MediaItem): void {
    const sc = narrow() ? scroller() : null;
    if (!selected && sc) scrolled = sc.scrollTop;
    selected = m;
    for (const [id, el] of tiles) el.querySelector(".tile")?.setAttribute("aria-pressed", String(id === m.id));
    layout.classList.add("has-sel");
    inspector.hidden = false;
    inspector.replaceChildren(card(m));
    inspector.querySelector<HTMLElement>(".insp-title b")?.focus({ preventScroll: true });
    if (sc) sc.scrollTop = 0;
  }

  /** Closes the details; `refocus` returns focus to the picture's tile. */
  function deselect(refocus: boolean): void {
    const was = selected;
    if (!was) return;
    closePop(false);
    selected = undefined;
    layout.classList.remove("has-sel");
    inspector.hidden = true;
    inspector.replaceChildren();
    for (const el of tiles.values()) el.querySelector(".tile")?.setAttribute("aria-pressed", "false");
    const sc = narrow() ? scroller() : null;
    if (sc) sc.scrollTop = scrolled;
    if (refocus) tiles.get(was.id)?.querySelector<HTMLElement>(".tile")?.focus();
  }

  box.addEventListener("keydown", (e) => {
    // Unsaved words in the description are not thrown away by Esc.
    const typing = e.target instanceof HTMLTextAreaElement && e.target.value !== selected?.alt;
    if (e.key === "Escape" && selected && !e.defaultPrevented && !typing) {
      e.preventDefault();
      deselect(true);
    }
  });

  function card(m: MediaItem): HTMLElement {
    const name = nameOf(m.src);
    const close = h("button", { type: "button", class: "ib", "aria-label": t("admin.media.close"), title: t("admin.media.close"), onclick: () => deselect(true) }, icon("close"));
    const stage = h(
      "button",
      { type: "button", class: "stage", "aria-label": t("admin.media.view", { name }), onclick: () => viewer(m, stage) },
      h("img", { src: m.src, width: m.width, height: m.height, alt: "" }),
      h("span", { class: "stage-ico ib glass" }, icon("eye")),
    );
    const usage = h("dd", {}, t("admin.state.loading"));
    const facts = h(
      "dl",
      { class: "kv" },
      h("dt", {}, t("admin.media.size")),
      h("dd", {}, formatBytes(m.bytes, lang())),
      h("dt", {}, t("admin.media.type")),
      h("dd", {}, kindOf(m.mime)),
      h("dt", {}, t("admin.media.dimensions")),
      h("dd", {}, dimensions(m)),
      h("dt", {}, t("admin.media.uploaded")),
      h("dd", {}, when(m.created_at)),
      h("dt", {}, t("admin.media.used_in")),
      usage,
    );
    void loadUsage(m, usage);
    return h(
      "div",
      { class: "insp-card" },
      h("div", { class: "insp-head" }, h("div", { class: "insp-title" }, h("b", { tabindex: -1 }, name), h("span", {}, metaOf(m))), close),
      stage,
      altForm(m),
      facts,
      deleteBlock(m),
    );
  }

  /** Where the picture is used comes with the single item, not the list. */
  async function loadUsage(m: MediaItem, into: HTMLElement): Promise<void> {
    const r = await api.mediaItem(m.id);
    if (selected?.id !== m.id) return;
    if (!r.ok) {
      console.error(`admin media: loading usage of ${m.id} failed: ${r.error.code} (${r.error.status})`);
      const retry = h("button", { type: "button", class: "linkbtn", onclick: () => ((into.textContent = t("admin.state.loading")), void loadUsage(m, into)) }, t("admin.state.retry"));
      into.replaceChildren(t("admin.media.err_usage"), " ", retry);
      return;
    }
    const posts = r.value.usage ?? [];
    into.replaceChildren(posts.length ? h("span", { class: "chips" }, ...postLinks(posts)) : t("admin.media.unused_note"));
  }

  const postLinks = (ids: readonly number[]): HTMLElement[] =>
    ids.map((id) => h("a", { class: "chip", href: editHref(id) }, h("span", {}, t("admin.media.post_n", { id }))));

  function altForm(m: MediaItem): HTMLElement {
    const id = `alt-${m.id}`;
    const input = h("textarea", { class: "textarea", id, rows: 3, "aria-describedby": `${id}-hint` });
    input.value = m.alt;
    const msg = h("p", { class: "err", role: "alert", hidden: true });
    const save = h("button", { type: "button", class: "btn", disabled: true }, t("admin.media.save"));
    let saving = false;
    input.addEventListener("input", () => (save.disabled = saving || input.value === m.alt));
    /** Saves; if the text changed while a save was out, the latest text goes next. */
    const run = async (): Promise<void> => {
      saving = true;
      save.disabled = true;
      save.setAttribute("aria-busy", "true");
      msg.hidden = true;
      for (;;) {
        const r = await api.setMediaAlt(m.id, input.value);
        if (!r.ok) {
          if (r.error.code === "auth.unauthenticated") break;
          console.error(`admin media: saving the description of ${m.id} failed: ${r.error.code} (${r.error.status})`);
          msg.textContent = r.error.code === "network" ? t("admin.state.network_title") : t("admin.media.err_save");
          msg.hidden = false;
          break;
        }
        m.alt = r.value.alt;
        const i = items.findIndex((x) => x.id === m.id);
        if (i >= 0) items[i] = m;
        const old = tiles.get(m.id);
        if (old) {
          const fresh = tile(m);
          old.replaceWith(fresh);
          tiles.set(m.id, fresh);
        }
        if (input.value === m.alt) {
          toast(t("admin.media.saved"));
          break;
        }
      }
      saving = false;
      save.removeAttribute("aria-busy");
      save.disabled = input.value === m.alt;
    };
    save.onclick = () => {
      if (!saving && !save.disabled) void run();
    };
    return h(
      "div",
      { class: "field" },
      h("label", { for: id }, t("admin.media.alt")),
      input,
      h("span", { class: "hint", id: `${id}-hint` }, t("admin.media.alt_hint")),
      msg,
      h("div", { class: "btn-row" }, save),
    );
  }

  function deleteBlock(m: MediaItem): HTMLElement {
    if (!canDelete(m, me)) {
      return h(
        "div",
        { class: "field" },
        h("button", { type: "button", class: "btn danger", disabled: true, "aria-describedby": `del-${m.id}` }, t("admin.media.delete")),
        h("span", { class: "hint", id: `del-${m.id}` }, t("admin.media.err_forbidden")),
      );
    }
    const note = h("div", { role: "alert", class: "del-note", hidden: true });
    const del = h("button", { type: "button", class: "btn danger", "aria-haspopup": "dialog", "aria-expanded": "false", onclick: () => ask() }, t("admin.media.delete"));
    const ask = (): void => {
      const cancel = h("button", { type: "button", class: "btn", onclick: () => closePop() }, t("admin.media.cancel"));
      const yes = h("button", { type: "button", class: "btn danger", onclick: () => ((yes.disabled = true), closePop(false), void remove()) }, t("admin.media.delete_yes"));
      openPop(del, h("div", { class: "confirm" }, h("h3", {}, t("admin.media.delete_q")), h("p", { class: "hint" }, t("admin.media.delete_hint")), h("div", { class: "btn-row end" }, cancel, yes)), t("admin.media.delete_q"));
    };
    const remove = async (): Promise<void> => {
      // The button is about to go dead and the popover is closed: keep focus in the card (Esc still works).
      if (selected?.id === m.id) inspector.querySelector<HTMLElement>(".insp-title b")?.focus({ preventScroll: true });
      del.disabled = true;
      del.setAttribute("aria-busy", "true");
      note.hidden = true;
      const r = await api.deleteMedia(m.id);
      del.disabled = false;
      del.removeAttribute("aria-busy");
      if (r.ok || r.error.code === "store.not_found") return gone(m);
      if (r.error.code === "auth.unauthenticated") return;
      // A refusal (not yours, in use) is an answer, which the client already logged; the rest is a fault.
      if (r.error.code !== "auth.forbidden" && r.error.code !== "store.conflict") console.error(`admin media: deleting ${m.id} failed: ${r.error.code} (${r.error.status})`);
      const used = r.error.code === "store.conflict" && r.error.posts?.length ? r.error.posts : [];
      const text =
        r.error.code === "auth.forbidden" ? t("admin.media.err_forbidden") : used.length ? t("admin.media.err_used") : r.error.code === "network" ? t("admin.state.network_title") : t("admin.media.err_delete");
      // The card may have been closed (or another opened) while the request was out: then a toast says it.
      if (selected?.id !== m.id || !note.isConnected) {
        toast([text, ...used.map((id) => t("admin.media.post_n", { id }))].join(" "), 8000);
        return;
      }
      del.focus();
      note.replaceChildren(h("p", { class: "err" }, text), ...(used.length ? [h("div", { class: "chips" }, ...postLinks(used))] : []));
      note.hidden = false;
      note.scrollIntoView({ block: "nearest" });
    };
    return h("div", { class: "field" }, h("div", { class: "btn-row" }, del), note);
  }

  /** A deleted item leaves the grid; focus goes to its neighbour, or to the filters when none is left. */
  function gone(m: MediaItem): void {
    const i = items.findIndex((x) => x.id === m.id);
    if (i < 0) return;
    items.splice(i, 1);
    tiles.get(m.id)?.remove();
    tiles.delete(m.id);
    // Only this item's own card closes; another picture's open card stays.
    const open = selected?.id === m.id;
    if (open) deselect(false);
    toast(t("admin.media.deleted"));
    if (!items.length && cursor === null) return show();
    if (!open) return;
    const next = items[Math.min(i, items.length - 1)];
    // The last item of a page that has more: "Load more" is still there to ask for them.
    if (next) tiles.get(next.id)?.querySelector<HTMLElement>(".tile")?.focus();
    else tail.querySelector("button")?.focus();
  }

  // ---- viewer ---------------------------------------------------------------------------
  /** The full-screen dark viewer; Esc closes it, the arrows step through the loaded pictures. */
  function viewer(start: MediaItem, from: HTMLElement): void {
    let at = Math.max(0, items.findIndex((x) => x.id === start.id));
    const cur = (): MediaItem => items[at] ?? start;
    const name = h("b");
    const meta = h("span");
    const fig = h("figure", { class: "lb-fig" });
    const prev = h("button", { type: "button", class: "ib glass lb-prev", "aria-label": t("admin.media.prev"), onclick: () => step(-1) }, icon("back"));
    const next = h("button", { type: "button", class: "ib glass lb-next", "aria-label": t("admin.media.next"), onclick: () => step(1) }, icon("back"));
    const close = h("button", { type: "button", class: "ib glass", "aria-label": t("admin.media.close"), onclick: () => end() }, icon("close"));
    const el = h(
      "div",
      { class: "lb", role: "dialog", "aria-modal": "true", "aria-label": nameOf(start.src) },
      h("div", { class: "lb-top" }, h("div", { class: "lb-name" }, name, meta), close),
      fig,
      h("div", { class: "lb-nav" }, prev, next),
    );
    const draw = (): void => {
      const m = cur();
      name.textContent = nameOf(m.src);
      meta.textContent = metaOf(m);
      fig.replaceChildren(h("img", { src: m.src, width: m.width, height: m.height, alt: m.alt }), m.alt === "" ? lamp("lb-na") : h("figcaption", {}, m.alt));
      prev.disabled = at === 0;
      next.disabled = at === items.length - 1;
    };
    const step = (d: number): void => {
      const to = at + d;
      if (to < 0 || to >= items.length) return;
      at = to;
      draw();
    };
    // The screen going away (another page, a redraw) takes the viewer with it.
    const watch = new MutationObserver(() => box.isConnected || end(false));
    watch.observe(document.body, { childList: true, subtree: true });
    const end = (refocus = true): void => {
      watch.disconnect();
      el.remove();
      if (!refocus) return;
      // Back to what opened it; if that is gone, the picture on screen.
      const back = tiles.get(cur().id)?.querySelector<HTMLElement>(".peek");
      (from.isConnected ? from : back?.isConnected ? back : null)?.focus();
    };
    el.addEventListener("keydown", (e) => {
      if (e.key === "Escape") {
        e.preventDefault();
        e.stopPropagation();
        end();
      } else if (e.key === "ArrowLeft") step(-1);
      else if (e.key === "ArrowRight") step(1);
      else if (e.key === "Tab") {
        // Focus stays inside the dialog.
        const keys = [...el.querySelectorAll<HTMLButtonElement>("button:not(:disabled)")];
        const first = keys[0];
        const last = keys[keys.length - 1];
        if (e.shiftKey && document.activeElement === first) (e.preventDefault(), last?.focus());
        else if (!e.shiftKey && document.activeElement === last) (e.preventDefault(), first?.focus());
      }
    });
    draw();
    document.body.append(el);
    close.focus();
  }

  void load(false);
}
