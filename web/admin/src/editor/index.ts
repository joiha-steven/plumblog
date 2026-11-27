/**
 * The writing screen (docs/ARCHITECTURE.md, Admin and editor): title and Markdown body with
 * live styling, Write / Split / Preview, a toolbar that steps back while you type, a
 * selection bubble, the "/" block menu, focus mode, typewriter scrolling, a word count and
 * drafts saved in this browser. Loaded as its own chunk, only on the editor route.
 */

import { redo, undo } from "@codemirror/commands";
import type { EditorView, ViewUpdate } from "@codemirror/view";
import type { Api, PostDraft } from "../api/client";
import { type Child, h } from "../dom";
import { type IconName, icon } from "../icons";
import { num, t } from "../i18n";
import { HREF } from "../router";
import { closePop, openPop, openPopEl, toast } from "../ui";
import { BLOCKS, type Block, blockName, fold } from "./blocks";
import { createEditor } from "./cm";
import { countWords, heading, link, linePrefix, wrap } from "./commands";
import { loadDraft, saveDraft } from "./drafts";

export interface EditorOptions {
  root: HTMLElement;
  api: Api;
  /** The post being edited; undefined for a new one. */
  post: PostDraft | undefined;
  lang: string;
  /** The page's CSP nonce (boot data), for CodeMirror's `<style>` element. */
  nonce: string;
  /** Whether the network is gone (or the mock says so). */
  offline: () => boolean;
}

type Mode = "write" | "split" | "preview";

/** Where a tool sits: every width, not on phones, only from 760 px, or only in "More". */
type Show = "all" | "wide" | "mid" | "menu";
interface Tool {
  id: string;
  icon: IconName;
  show: Show;
  key?: string;
}

const MOD = /Mac|iPhone|iPad/.test(navigator.platform) ? "\u2318" : "Ctrl+";
const TOOLS: Tool[][] = [
  [{ id: "heading", icon: "heading", show: "all" }],
  [
    { id: "bold", icon: "bold", show: "all", key: `${MOD}B` },
    { id: "italic", icon: "italic", show: "all", key: `${MOD}I` },
    { id: "strike", icon: "strike", show: "mid" },
    { id: "code", icon: "code", show: "mid" },
  ],
  [
    { id: "link", icon: "link", show: "all", key: `${MOD}K` },
    { id: "quote", icon: "quote", show: "mid" },
  ],
  [
    { id: "list", icon: "ul", show: "wide" },
    { id: "numbered", icon: "ol", show: "mid" },
    { id: "task", icon: "task", show: "mid" },
  ],
  [
    { id: "table", icon: "table", show: "wide" },
    { id: "math", icon: "math", show: "menu" },
    { id: "footnote", icon: "footnote", show: "menu" },
    { id: "divider", icon: "hr", show: "menu" },
  ],
  [
    { id: "undo", icon: "undo", show: "wide", key: `${MOD}Z` },
    { id: "redo", icon: "redo", show: "wide", key: `\u21e7${MOD}Z` },
  ],
];
const SHOW_CLASS: Record<Show, string> = { all: "", wide: " wide-only", mid: " wide-only mid", menu: " menu-only" };

const toolName = (id: string): string =>
  // i18n: admin.editor.tool.heading admin.editor.tool.bold admin.editor.tool.italic admin.editor.tool.strike admin.editor.tool.code admin.editor.tool.link admin.editor.tool.quote admin.editor.tool.list admin.editor.tool.numbered admin.editor.tool.task admin.editor.tool.table admin.editor.tool.math admin.editor.tool.footnote admin.editor.tool.divider admin.editor.tool.undo admin.editor.tool.redo
  t(`admin.editor.tool.${id}`);

const modeName = (m: Mode): string =>
  // i18n: admin.editor.mode.write admin.editor.mode.split admin.editor.mode.preview
  t(`admin.editor.mode.${m}`);

const statusName = (s: PostDraft["status"]): string =>
  // i18n: admin.status.draft admin.status.scheduled admin.status.published
  t(`admin.status.${s}`);

export async function mountEditor(o: EditorOptions): Promise<void> {
  const post = o.post;
  const key = post ? `post-${post.id}` : "new";
  const local = await loadDraft(key);
  // A draft kept here newer than the server's copy wins: it is what the writer last saw.
  const fromLocal = local !== undefined && (post === undefined || local.savedAt > post.updated_at);
  const lang = post?.lang ?? o.lang;
  const status = post?.status ?? "draft";
  let title = fromLocal ? local.title : (post?.title ?? "");
  const body = fromLocal ? local.body : (post?.body ?? "");

  let mode: Mode = "write";
  let focusMode = false;
  let typewriter = false;
  const wide = () => document.documentElement.clientWidth >= 1200;
  const canSplit = () => document.documentElement.clientWidth >= 900;
  const phone = () => document.documentElement.clientWidth < 600;

  /* ---------- frame ---------- */
  const saved = h("span", { class: "saved", "aria-live": "polite" });
  const saved2 = h("span", {});
  const wc = h("span", { class: "wc" });
  const wc2 = h("span", { class: "num" });
  const modes = h("div", { class: "seg", role: "group", "aria-label": t("admin.editor.modes") });
  const titleIn = h("textarea", {
    class: "title-in",
    rows: 1,
    placeholder: t("admin.editor.title_placeholder"),
    "aria-label": t("admin.editor.title_label"),
    lang,
  });
  titleIn.value = title;
  const cmHost = h("div", { lang });
  const toolbar = h("div", { class: "tools", role: "toolbar", "aria-label": t("admin.editor.toolbar") });
  const write = h(
    "div",
    { class: "pane ed-write" },
    h("div", { class: "tools-wrap" }, toolbar),
    h(
      "div",
      { class: "col" },
      titleIn,
      cmHost,
      h("div", { class: "phone-status" }, h("span", {}, statusName(status), " \u00b7 ", saved2), wc2),
    ),
  );
  const previewPane = h("div", { class: "pane ed-preview" });
  const root = h(
    "div",
    { class: "ed", "data-mode": mode },
    h(
      "header",
      { class: "head ed-top" },
      h("a", { class: "ib flush-start", href: HREF.posts, "aria-label": t("admin.editor.back"), title: t("admin.editor.back") }, icon("back")),
      h("div", { class: "crumb" }, h("span", {}, t("admin.nav.posts")), h("span", { class: `st ${status}` }, statusName(status)), wc),
      h("div", { class: "ed-mid" }, modes),
      h("div", { class: "ed-actions" }, saved),
    ),
    h("div", { class: "ed-body" }, write, previewPane),
  );
  o.root.replaceChildren(root);

  /* ---------- modes and preview ---------- */
  const drawModes = () => {
    if (mode === "split" && !canSplit()) mode = "write";
    root.dataset["mode"] = mode;
    root.classList.toggle("wide", wide());
    const opts: [Mode, IconName][] = [["write", "pen"], ...(canSplit() ? ([["split", "split"]] as [Mode, IconName][]) : []), ["preview", "eye"]];
    modes.replaceChildren(
      ...opts.map(([m, ic]) =>
        h(
          "button",
          { type: "button", "aria-pressed": String(m === mode), onclick: () => setMode(m) },
          phone() ? icon(ic) : null,
          h("span", { class: "lbl" }, modeName(m)),
        ),
      ),
    );
  };
  const setMode = (m: Mode) => {
    mode = m;
    drawModes();
    if (m !== "write") preview();
  };

  // The Worker renders the draft with the site's theme into a sandboxed frame: the preview is
  // the published page, and no theme code ever runs in the admin itself. No scripts in the
  // frame: the page is complete as rendered (math, code), so same-origin is safe to grant and
  // lets the theme's fonts load (an opaque origin fails their CORS check).
  const frame = h("iframe", { class: "pv-frame", name: "pb-preview", title: t("admin.editor.preview_label"), sandbox: "allow-same-origin" });
  const field = (name: string) => h("input", { type: "hidden", name });
  const fTitle = field("title");
  const fBody = field("body");
  const fLang = field("lang");
  const form = h("form", { method: "post", target: "pb-preview", class: "sr", "aria-hidden": "true" }, fTitle, fBody, fLang);
  let previewTimer: ReturnType<typeof setTimeout> | undefined;
  const preview = () => {
    clearTimeout(previewTimer);
    previewTimer = setTimeout(() => {
      const note = (k: "offline" | "missing") =>
        h(
          "div",
          { class: "empty full" },
          icon(k === "offline" ? "offline" : "eye"),
          h("strong", {}, k === "offline" ? t("admin.editor.preview_offline_title") : t("admin.editor.preview_missing_title")),
          h("span", {}, k === "offline" ? t("admin.editor.preview_offline_hint") : t("admin.editor.preview_missing_hint")),
        );
      if (o.offline()) return previewPane.replaceChildren(note("offline"));
      if (!o.api.previewUrl) return previewPane.replaceChildren(note("missing"));
      if (!frame.isConnected) previewPane.replaceChildren(frame, form);
      form.action = o.api.previewUrl;
      fTitle.value = titleIn.value;
      fBody.value = view.state.doc.toString();
      fLang.value = lang;
      form.submit();
    }, 350);
  };

  /* ---------- saving ---------- */
  let saveTimer: ReturnType<typeof setTimeout> | undefined;
  let quietTimer: ReturnType<typeof setTimeout> | undefined;
  const setSaved = (text: string, quiet = false) => {
    for (const el of [saved, saved2]) {
      el.textContent = text;
      el.classList.toggle("settled", quiet);
    }
  };
  const dirty = () => {
    clearTimeout(saveTimer);
    clearTimeout(quietTimer);
    setSaved(t("admin.editor.saving"));
    saveTimer = setTimeout(async () => {
      const ok = await saveDraft(key, { title: titleIn.value, body: view.state.doc.toString(), savedAt: Date.now() });
      if (!ok) {
        setSaved(t("admin.editor.not_saved"));
        toast(t("admin.editor.not_saved_hint"), 6000);
        return;
      }
      setSaved(t("admin.editor.saved_here"));
      quietTimer = setTimeout(() => setSaved(t("admin.editor.saved_here"), true), 1800);
      if (mode !== "write") preview();
    }, 700);
  };
  setSaved(fromLocal || post ? t("admin.editor.saved_here") : "", true);

  const count = () => {
    const words = countWords(`${titleIn.value} ${view.state.doc.toString()}`);
    // A blank page has no reading time: "0 words, 1 min" would be wrong.
    const text = words ? t("admin.editor.words", { count: words, minutes: num(Math.max(1, Math.round(words / 220))) }) : t("admin.editor.words_none");
    wc.textContent = text;
    wc2.textContent = text;
  };

  /* ---------- typing: toolbar steps back, typewriter keeps the line mid-screen ---------- */
  let typeTimer: ReturnType<typeof setTimeout> | undefined;
  const typing = () => {
    root.classList.add("typing");
    clearTimeout(typeTimer);
    typeTimer = setTimeout(() => root.classList.remove("typing"), 2200);
  };
  root.addEventListener("pointermove", (e) => {
    if (e.movementX || e.movementY) root.classList.remove("typing");
  });
  const typewriterScroll = (v: EditorView) => {
    const head = v.state.selection.main.head;
    requestAnimationFrame(() => {
      const c = v.coordsAtPos(head);
      if (!c) return;
      const box = write.getBoundingClientRect();
      write.scrollTo({ top: write.scrollTop + (c.top - box.top) - box.height * 0.42, behavior: "smooth" });
    });
  };

  /* ---------- selection bubble ---------- */
  let bubble: HTMLElement | undefined;
  const hideBubble = () => {
    bubble?.remove();
    bubble = undefined;
  };
  const showBubble = () => {
    const r = view.state.selection.main;
    if (r.empty || !view.hasFocus) return hideBubble();
    const a = view.coordsAtPos(r.from);
    const b = view.coordsAtPos(r.to);
    if (!a || !b) return hideBubble();
    if (!bubble) {
      const items: [string, IconName][] = [
        ["bold", "bold"],
        ["italic", "italic"],
        ["strike", "strike"],
        ["code", "code"],
        ["link", "link"],
      ];
      bubble = h(
        "div",
        { class: "bubble", role: "toolbar", "aria-label": t("admin.editor.selection_bar") },
        ...items.map(([id, ic]) => h("button", { class: "ib", type: "button", "aria-label": toolName(id), title: toolName(id), "data-tool": id }, icon(ic))),
      );
      bubble.addEventListener("mousedown", (e) => e.preventDefault());
      bubble.addEventListener("click", (e) => {
        const id = (e.target as Element).closest<HTMLElement>("[data-tool]")?.dataset["tool"];
        if (id) runTool(id, bubble);
      });
      document.body.append(bubble);
    }
    const vw = document.documentElement.clientWidth;
    const w = bubble.offsetWidth;
    const x = Math.max(16, Math.min((a.left + b.right) / 2 - w / 2, vw - w - 16));
    // On touch screens the system's own menu sits above a selection, so the bubble goes below.
    const touch = matchMedia("(pointer: coarse)").matches;
    const y = touch ? b.bottom + 12 : a.top - bubble.offsetHeight - 10;
    bubble.style.left = `${x}px`;
    bubble.style.top = `${Math.max(8, y)}px`;
  };

  /* ---------- link ---------- */
  const linkPrompt = (v: EditorView, anchor?: HTMLElement) => {
    const r = v.state.selection.main;
    const at = v.coordsAtPos(r.from);
    hideBubble();
    const input = h("input", { class: "input", type: "url", id: "pb-link", placeholder: t("admin.editor.link_placeholder"), inputmode: "url", autocomplete: "off" });
    const formEl = h(
      "form",
      { class: "altpop" },
      h("label", { class: "label", for: "pb-link" }, t("admin.editor.link_to")),
      input,
      h(
        "div",
        { class: "btn-row end" },
        h("button", { type: "button", class: "btn quiet", onclick: () => (closePop(false), v.focus()) }, t("admin.common.cancel")),
        h("button", { type: "submit", class: "btn primary" }, t("admin.editor.link_apply")),
      ),
    );
    formEl.addEventListener("submit", (e) => {
      e.preventDefault();
      const url = input.value.trim();
      if (!url) return;
      closePop(false);
      link(v, r.from, r.to, url, t("admin.editor.link_text"));
    });
    const box = at ? new DOMRect(at.left, at.top, 0, at.bottom - at.top) : undefined;
    openPop(anchor ?? cmHost, formEl, t("admin.editor.link_to"), box ? { at: box } : {});
  };

  /* ---------- "/" block menu ---------- */
  let slash: { from: number; sel: number; items: Block[] } | undefined;
  const closeSlash = () => {
    if (!slash) return;
    slash = undefined;
    closePop(false);
  };
  const drawSlash = () => {
    if (!slash) return;
    const q = fold(view.state.sliceDoc(slash.from + 1, view.state.selection.main.head));
    slash.items = BLOCKS.filter((b) => !q || fold(`${blockName(b)} ${b.id}`).includes(q));
    if (!slash.items.length) return closeSlash();
    slash.sel = Math.min(slash.sel, slash.items.length - 1);
    const s = slash;
    const list = h(
      "div",
      { role: "listbox", "aria-label": t("admin.editor.blocks") },
      ...s.items.map((b, i) =>
        h(
          "button",
          { class: "menu-item", type: "button", role: "option", "aria-selected": String(i === s.sel), "data-i": i },
          h("span", { class: "ico" }, icon(b.icon)),
          h("span", {}, blockName(b)),
        ),
      ),
    );
    const open = openPopEl();
    if (open?.classList.contains("slash")) open.replaceChildren(list);
    else {
      const c = view.coordsAtPos(s.from);
      const at = c ? new DOMRect(c.left, c.top, 0, c.bottom - c.top) : undefined;
      const el = openPop(cmHost, list, t("admin.editor.blocks"), { ...(at ? { at } : {}), cls: "slash", keepFocus: true });
      el?.addEventListener("mousedown", (e) => e.preventDefault());
      el?.addEventListener("click", (e) => {
        const i = (e.target as Element).closest<HTMLElement>("[data-i]")?.dataset["i"];
        if (i !== undefined && slash) {
          slash.sel = Number(i);
          runSlash();
        }
      });
    }
    openPopEl()?.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: "nearest" });
  };
  const runSlash = () => {
    if (!slash) return;
    const b = slash.items[slash.sel];
    const from = slash.from;
    const to = view.state.selection.main.head;
    closeSlash();
    b?.run(view, from, to);
  };

  /* ---------- the editor ---------- */
  const view: EditorView = createEditor(cmHost, body, {
    lang,
    nonce: o.nonce,
    label: t("admin.editor.body_label"),
    placeholder: t("admin.editor.body_placeholder"),
    missingAlt: t("admin.editor.picture_no_alt"),
    keys: [
      { key: "ArrowDown", run: () => (slash ? ((slash.sel = (slash.sel + 1) % slash.items.length), drawSlash(), true) : false) },
      { key: "ArrowUp", run: () => (slash ? ((slash.sel = (slash.sel - 1 + slash.items.length) % slash.items.length), drawSlash(), true) : false) },
      { key: "Enter", run: () => (slash ? (runSlash(), true) : false) },
      { key: "Tab", run: () => (slash ? (runSlash(), true) : false) },
      {
        key: "Escape",
        run: () => {
          if (slash) return closeSlash(), true;
          if (focusMode) return toggleFocus(), true;
          return false;
        },
      },
    ],
    link: (v) => linkPrompt(v),
    update(u: ViewUpdate) {
      if (u.docChanged) {
        dirty();
        count();
        const typed = u.transactions.some((tr) => tr.isUserEvent("input"));
        if (typed) typing();
        // "/" typed at the start of a line or after a space opens the block menu.
        const head = u.state.selection.main.head;
        const line = u.state.doc.lineAt(head);
        const m = /(^|\s)\/([^\s/]{0,16})$/.exec(line.text.slice(0, head - line.from));
        if (!slash && m && typed) slash = { from: head - (m[2]?.length ?? 0) - 1, sel: 0, items: [] };
        if (slash) {
          const s = slash;
          if (head <= s.from || u.state.sliceDoc(s.from, s.from + 1) !== "/" || /\s/.test(u.state.sliceDoc(s.from + 1, head))) closeSlash();
          else drawSlash();
        }
      }
      if (u.selectionSet || u.focusChanged) {
        requestAnimationFrame(showBubble);
        if (typewriter && u.selectionSet) typewriterScroll(u.view);
      }
    },
    events: {
      blur: () => {
        setTimeout(() => {
          if (!view.hasFocus) hideBubble();
        }, 120);
        return false;
      },
    },
  });
  count();

  /* ---------- title ---------- */
  const fit = () => {
    titleIn.style.height = "auto";
    titleIn.style.height = `${titleIn.scrollHeight}px`;
  };
  titleIn.addEventListener("input", () => {
    fit();
    title = titleIn.value;
    dirty();
    count();
  });
  // Enter moves into the body: a title is one paragraph.
  titleIn.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      view.focus();
      view.dispatch({ selection: { anchor: 0 } });
    }
  });
  requestAnimationFrame(fit);

  /* ---------- toolbar ---------- */
  const toggleFocus = () => {
    focusMode = !focusMode;
    root.classList.toggle("focus", focusMode);
    if (focusMode) toast(t("admin.editor.focus_on"), 2200);
    view.focus();
  };
  const block = (id: string) => {
    const head = view.state.selection.main.head;
    BLOCKS.find((b) => b.id === id)?.run(view, head, head);
  };
  const headingMenu = (btn: HTMLElement) => {
    const levels: [number, string, string][] = [
      [0, "\u00b6", t("admin.editor.heading.paragraph")],
      [1, "H1", t("admin.editor.heading.h1")],
      [2, "H2", t("admin.editor.heading.h2")],
      [3, "H3", t("admin.editor.heading.h3")],
    ];
    const menu = h(
      "div",
      { role: "menu" },
      ...levels.map(([n, mark, label]) =>
        h(
          "button",
          { class: "menu-item", type: "button", role: "menuitem", onclick: () => (closePop(false), heading(view, n)) },
          h("span", { class: "ico" }, mark),
          label,
        ),
      ),
    );
    menu.addEventListener("mousedown", (e) => e.preventDefault());
    openPop(btn, menu, toolName("heading"), { below: true });
  };
  const moreMenu = (btn: HTMLElement) => {
    const hidden = TOOLS.flat().filter((x) => x.id !== "heading" && !toolbar.querySelector<HTMLElement>(`[data-tool="${x.id}"]`)?.offsetParent);
    const toggle = (id: "focus" | "typewriter", on: boolean, ic: IconName, label: string) =>
      h(
        "button",
        { class: "menu-item", type: "button", role: "menuitemcheckbox", "aria-checked": String(on), "data-tool": id },
        icon(ic),
        h("span", {}, label),
        on ? h("span", { class: "k" }, icon("check")) : null,
      );
    const menu = h(
      "div",
      { role: "menu", "aria-label": t("admin.editor.more_tools") },
      ...hidden.map((x) =>
        h(
          "button",
          { class: "menu-item", type: "button", role: "menuitem", "data-tool": x.id },
          icon(x.icon),
          h("span", {}, toolName(x.id)),
          x.key ? h("span", { class: "k" }, x.key) : null,
        ),
      ),
      hidden.length ? h("div", { class: "menu-sep" }) : null,
      toggle("focus", focusMode, "focus", t("admin.editor.focus")),
      toggle("typewriter", typewriter, "type", t("admin.editor.typewriter")),
    );
    menu.addEventListener("mousedown", (e) => e.preventDefault());
    menu.addEventListener("click", (e) => {
      const id = (e.target as Element).closest<HTMLElement>("[data-tool]")?.dataset["tool"];
      if (!id) return;
      closePop(false);
      runTool(id, btn);
    });
    openPop(btn, menu, t("admin.editor.more_tools"), { below: !phone() });
  };
  const runTool = (id: string, btn?: HTMLElement) => {
    const actions: Record<string, () => void> = {
      heading: () => btn && headingMenu(btn),
      bold: () => wrap(view, "**"),
      italic: () => wrap(view, "*"),
      strike: () => wrap(view, "~~"),
      code: () => wrap(view, "`"),
      link: () => linkPrompt(view, btn),
      quote: () => linePrefix(view, "> "),
      list: () => linePrefix(view, "- "),
      numbered: () => linePrefix(view, "1. "),
      task: () => linePrefix(view, "- [ ] "),
      table: () => block("table"),
      math: () => block("math"),
      footnote: () => block("footnote"),
      divider: () => block("divider"),
      undo: () => (undo(view), view.focus()),
      redo: () => (redo(view), view.focus()),
      more: () => btn && moreMenu(btn),
      focus: toggleFocus,
      typewriter: () => {
        typewriter = !typewriter;
        toast(typewriter ? t("admin.editor.typewriter_on") : t("admin.editor.typewriter_off"), 2000);
        view.focus();
      },
    };
    actions[id]?.();
  };
  const toolButton = (x: Tool): HTMLElement => {
    const name = x.key ? `${toolName(x.id)} (${x.key})` : toolName(x.id);
    if (x.id === "heading") {
      return h(
        "button",
        { class: "ib tx", type: "button", "data-tool": x.id, "aria-haspopup": "menu", "aria-expanded": "false", "aria-label": toolName(x.id), title: name },
        icon("heading"),
        icon("chev"),
      );
    }
    return h(
      "button",
      { class: `ib${SHOW_CLASS[x.show]}`, type: "button", "data-tool": x.id, "aria-label": toolName(x.id), title: name },
      icon(x.icon),
    );
  };
  const groups: Child[] = [];
  TOOLS.forEach((g, i) => {
    if (i) groups.push(h("span", { class: "sep", "aria-hidden": "true" }));
    groups.push(...g.map(toolButton));
  });
  groups.push(
    h("span", { class: "sep", "aria-hidden": "true" }),
    h(
      "button",
      { class: "ib more", type: "button", "data-tool": "more", "aria-haspopup": "menu", "aria-expanded": "false", "aria-label": t("admin.editor.more_tools"), title: t("admin.editor.more_tools") },
      icon("more"),
    ),
  );
  for (const g of groups) if (g instanceof Node) toolbar.append(g);
  // Keep the text selection when a tool is pressed.
  toolbar.addEventListener("mousedown", (e) => {
    if ((e.target as Element).closest(".ib")) e.preventDefault();
  });
  toolbar.addEventListener("click", (e) => {
    const b = (e.target as Element).closest<HTMLElement>("[data-tool]");
    if (b?.dataset["tool"]) runTool(b.dataset["tool"], b);
  });

  // Phones: the toolbar rides just above the on-screen keyboard.
  const vv = window.visualViewport;
  const keyboard = () => {
    if (vv) root.style.setProperty("--kb", `${Math.max(0, innerHeight - vv.height - vv.offsetTop)}px`);
  };
  vv?.addEventListener("resize", keyboard);
  vv?.addEventListener("scroll", keyboard);
  const relayout = () => drawModes();
  addEventListener("resize", relayout);
  // Leaving the screen (a route change replaces the root) drops the listeners with it.
  new MutationObserver((_, obs) => {
    if (!root.isConnected) {
      removeEventListener("resize", relayout);
      vv?.removeEventListener("resize", keyboard);
      vv?.removeEventListener("scroll", keyboard);
      hideBubble();
      closeSlash();
      obs.disconnect();
    }
  }).observe(o.root, { childList: true });

  drawModes();
  if (!body && !title) titleIn.focus();
  else if (!phone()) {
    view.focus();
    view.dispatch({ selection: { anchor: 0 } });
  }
}
