/**
 * The writing screen (docs/ARCHITECTURE.md, Admin and editor): title and Markdown body with
 * live styling, Write / Split / Preview, a toolbar that steps back while you type, a
 * selection bubble, the "/" block menu, focus mode, typewriter scrolling, a word count and
 * drafts saved in this browser, then on the server (sync.ts). Loaded as its own chunk, only
 * on the editor route.
 */

import { redo, undo } from "@codemirror/commands";
import { EditorView, type ViewUpdate } from "@codemirror/view";
import { type Api, session } from "../api/client";
import type { Post } from "../api/model.gen";
import { type Child, h } from "../dom";
import { type IconName, icon } from "../icons";
import { num, t } from "../i18n";
import { HREF, editHref, go } from "../router";
import { closePop, openPop, openPopEl, toast } from "../ui";
import { BLOCKS, type Block, blockName, fold, picker } from "./blocks";
import { createEditor, inCode, removeCard, showCard } from "./cm";
import { countWords, heading, link, linePrefix, wrap } from "./commands";
import { mdAlt, mdUrl } from "./md-text";
import { type Draft, type SaveResult, draftKey, dropDraft, followMoved, holdOpen, sweepMoved, loadDraft, loadSides, putDraft, putNow, saveDraft, sideKey, startState } from "./drafts";
import { created, fromPost, label as pubLabel, newPub } from "./pub";
import type { PubHost, PubUi } from "./publish";
import type { Sync } from "./sync";

export interface EditorOptions {
  root: HTMLElement;
  api: Api;
  /** The post being edited; undefined for a new one. */
  post: Post | undefined;
  /** The signed-in user: drafts are kept per user. */
  user: number;
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
    { id: "picture", icon: "image", show: "all" },
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
/** The largest picture taken as is: bigger ones are almost always a mistake (a raw camera
 * file, a scan) and would take minutes on a phone connection. */
const MAX_PICTURE = 20 * 1024 * 1024;
const SHOW_CLASS: Record<Show, string> = { all: "", wide: " wide-only", mid: " wide-only mid", menu: " menu-only" };

const toolName = (id: string): string =>
  // i18n: admin.editor.tool.heading admin.editor.tool.bold admin.editor.tool.italic admin.editor.tool.strike admin.editor.tool.code admin.editor.tool.link admin.editor.tool.quote admin.editor.tool.picture admin.editor.tool.list admin.editor.tool.numbered admin.editor.tool.task admin.editor.tool.table admin.editor.tool.math admin.editor.tool.footnote admin.editor.tool.divider admin.editor.tool.undo admin.editor.tool.redo
  t(`admin.editor.tool.${id}`);

const modeName = (m: Mode): string =>
  // i18n: admin.editor.mode.write admin.editor.mode.split admin.editor.mode.preview
  t(`admin.editor.mode.${m}`);

const statusName = (s: Post["status"]): string =>
  // i18n: admin.status.draft admin.status.scheduled admin.status.published admin.status.trashed
  t(`admin.status.${s}`);

/** The last editor's final local save, flushed when it left the page; the next mount reads after it. */
let flushed: Promise<unknown> = Promise.resolve();

/** A name for this tab's side copy of a draft (drafts.ts sideKey). */
const newTab = (): string => crypto.randomUUID().replaceAll("-", "").slice(0, 16);

export async function mountEditor(o: EditorOptions): Promise<void> {
  const post = o.post;
  // A new post's draft key lives in its URL (?draft=), so a reload finds it again and two new
  // posts, in two tabs, never share one.
  const url = new URL(location.href);
  let newId = url.searchParams.get("draft") ?? "";
  if (post === undefined && !/^[a-z0-9]{12,32}$/.test(newId)) {
    newId = crypto.randomUUID().replaceAll("-", "").slice(0, 16);
    url.searchParams.set("draft", newId);
    history.replaceState(history.state, "", url);
  }
  const key = draftKey(o.user, post?.id, newId);
  await flushed;
  // Redrawn (online/offline) while an earlier mount was creating this post: open the post it
  // became rather than creating a second one (sync.ts `settled`, at most 8 s).
  if (post === undefined) {
    const made = await import("./sync").then(
      (m) => m.settled(key, o.user),
      () => undefined,
    );
    if (made) {
      history.replaceState(history.state, "", editHref(made.id));
      return go(editHref(made.id));
    }
    // Created by a page that is gone (its copy of the last keys may still be under this key):
    // open the post, copies moved with it, rather than create it twice.
    const moved = await followMoved(key);
    if (moved !== undefined) {
      history.replaceState(history.state, "", editHref(moved));
      return go(editHref(moved));
    }
  }
  holdOpen();
  void sweepMoved(o.user);
  const local = await loadDraft(key);
  const { st, fromLocal, serverMoved } = startState(key, local, post);
  /** This page load's side copy (drafts.ts sideKey): the only one it ever writes. */
  const tab = newTab();
  let own: { key: string; rev: number } | undefined;
  // Copies other pages left (a tab paused or closed): one with the text this editor opens with
  // holds nothing new and goes; every other is offered, newest first, one choice after another
  // (none comes back on the next open), never taken in silently. Typing meanwhile goes to this
  // page's own copy, never to theirs.
  const opening: [string, string] = [local?.title ?? post?.title ?? "", local?.body ?? post?.markdown ?? ""];
  const offers: { key: string; rev: number }[] = [];
  for (const c of await loadSides(key)) {
    if (c.draft.title === opening[0] && c.draft.body === opening[1]) await dropDraft(c.key, c.draft.rev);
    else offers.push({ key: c.key, rev: c.draft.rev });
  }
  if (offers.length) st.paused = true;
  let sync: Sync | undefined;
  const lang = post?.lang ?? o.lang;
  const pub = newPub(post);
  const start = fromLocal ? local : undefined;
  let title = start?.title ?? post?.title ?? "";
  const body = start?.body ?? post?.markdown ?? "";

  let mode: Mode = "write";
  let focusMode = false;
  let typewriter = false;
  const wide = () => document.documentElement.clientWidth >= 1200;
  const canSplit = () => document.documentElement.clientWidth >= 900;
  const phone = () => document.documentElement.clientWidth < 600;

  /* ---------- frame ---------- */
  const saved = h("span", { class: "saved", "aria-live": "polite" });
  const saved2 = h("span", {});
  const stLabel = h("span", {}, statusName(pub.status));
  const chip = h("span", { class: `st ${pub.status}` }, statusName(pub.status));
  const gear = h("button", { type: "button", class: "ib", "data-panel-btn": "", "aria-pressed": "false", "aria-label": t("admin.editor.pub.settings"), title: t("admin.editor.pub.settings_tip") }, icon("panel"));
  const pubBtn = h("button", { type: "button", class: "btn primary" }, pubLabel(pub));
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
  const toolsWrap = h("div", { class: "tools-wrap" }, toolbar);
  const choiceSlot = h("div", {});
  const note = h("div", {});
  const write = h(
    "div",
    { class: "pane ed-write" },
    toolsWrap,
    choiceSlot,
    h(
      "div",
      { class: "col" },
      titleIn,
      note,
      cmHost,
      h("div", { class: "phone-status" }, h("span", {}, stLabel, " \u00b7 ", saved2), wc2),
    ),
  );
  const previewPane = h("div", { class: "pane ed-preview" });
  const edBody = h("div", { class: "ed-body" }, write, previewPane);
  const root = h(
    "div",
    { class: "ed", "data-mode": mode },
    h(
      "header",
      { class: "head ed-top" },
      h("a", { class: "ib flush-start", href: HREF.posts, "aria-label": t("admin.editor.back"), title: t("admin.editor.back") }, icon("back")),
      h("div", { class: "crumb" }, h("span", {}, t("admin.nav.posts")), chip, wc),
      h("div", { class: "ed-mid" }, modes),
      h("div", { class: "ed-actions" }, saved, gear, pubBtn),
    ),
    edBody,
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
  // Each preview loads into a fresh frame, swapped in once loaded: a frame's first load
  // replaces its blank page instead of adding to the history, so Back still leaves the editor.
  let frame: HTMLIFrameElement | undefined;
  let frames = 0;
  const field = (name: string) => h("input", { type: "hidden", name });
  const fTitle = field("title");
  const fBody = field("body");
  const fLang = field("lang");
  const form = h("form", { method: "post", class: "sr", "aria-hidden": "true" }, fTitle, fBody, fLang);
  const loadPreview = () => {
    const next = h("iframe", { class: "pv-frame", name: `pb-preview-${++frames}`, title: t("admin.editor.preview_label"), sandbox: "allow-same-origin" });
    next.hidden = true;
    next.addEventListener(
      "load",
      () => {
        // Same origin (no scripts in it), so the reading position carries over.
        const y = frame?.contentWindow?.scrollY ?? 0;
        frame?.remove();
        frame = next;
        next.hidden = false;
        next.contentWindow?.scrollTo(0, y);
      },
      { once: true },
    );
    if (!form.isConnected || (frame && !frame.isConnected)) previewPane.replaceChildren(...(frame ? [frame] : []), form);
    previewPane.append(next);
    form.target = next.name;
    form.submit();
  };
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
      if (!form.isConnected) previewPane.replaceChildren(form);
      form.action = o.api.previewUrl;
      fTitle.value = titleIn.value;
      fBody.value = view.state.doc.toString();
      fLang.value = lang;
      loadPreview();
    }, 350);
  };

  /* ---------- saving ---------- */
  let saveTimer: ReturnType<typeof setTimeout> | undefined;
  let quietTimer: ReturnType<typeof setTimeout> | undefined;
  // A live post's edits stay here until Update: say they are not on the site.
  const here = () => pub.status === "draft" ? t("admin.editor.saved_here") : t("admin.editor.pub.unpublished_changes");
  const setSaved = (text: string, quiet = false) => {
    for (const el of [saved, saved2]) {
      el.textContent = text;
      el.classList.toggle("settled", quiet);
    }
  };
  /** Typed text is not in this device's storage yet: true until the save of the last keys lands. */
  let unsaved = false;
  /** Bumped on every edit: a save landing clears `unsaved` only if nothing was typed since it began. */
  let edits = 0;
  /** The key and rev of this page's next write to its own copy (it follows a moved draft key). */
  const ownNext = (): { key: string; rev: number } => {
    const k = sideKey(st.key, tab);
    return { key: k, rev: own?.key === k ? own.rev + 1 : 1 };
  };
  /** Paused: the text goes to this page's own copy, never over the other tab's draft. */
  const saveSide = async (d: Omit<Draft, "rev">): Promise<SaveResult> => {
    const old = own;
    const to = ownNext();
    const res = await putDraft(to.key, { ...d, rev: to.rev });
    if (res !== "saved") return res;
    own = to;
    // The draft key moved (a new post was created) while paused: the copy follows it.
    if (old && old.key !== to.key) await dropDraft(old.key, old.rev);
    return res;
  };
  /** Once its text is in the draft (or the writer chose the other): this page's copy goes. */
  const dropOwn = () => {
    const s = own;
    own = undefined;
    if (s) st.q = st.q.then(() => dropDraft(s.key, s.rev));
    return st.q;
  };
  const saveNow = async (): Promise<SaveResult> => {
    const mark = edits;
    // `unsaved` stays true while this write waits behind a draft move (st.q): a page hidden
    // meanwhile still puts the keys in its own copy (leaving, putNow).
    const landed = () => {
      if (edits === mark) unsaved = false;
    };
    const d = { title: titleIn.value, body: view.state.doc.toString(), base: st.base, version: st.version, savedAt: Date.now() };
    const paused = st.paused;
    // After any draft move or drop sync.ts has queued, so it reads the current key and rev.
    const job = st.q.then(() => (paused ? saveSide(d) : saveDraft(st.key, d, st.rev)));
    st.q = job;
    const res = await job;
    if (res === "saved") landed();
    if (paused) {
      if (res === "failed") {
        setSaved(t("admin.editor.not_saved"));
        toast(t("admin.editor.not_saved_hint"), 6000);
      } else setSaved(here());
      return res;
    }
    if (res === "conflict") {
      st.paused = true;
      // This text is not in the draft yet: it goes to the side copy now, not at the next key.
      await saveNow();
      // Another page created the post from this new-post key: open it, this copy moved along.
      const moved = st.key.includes(":new-") ? await followMoved(st.key) : undefined;
      if (moved !== undefined) {
        console.warn(`admin editor: ${st.key} became post ${moved} in another page; opening it`);
        history.replaceState(history.state, "", editHref(moved));
        go(editHref(moved));
        return res;
      }
      otherTab();
      return res;
    }
    if (res === "failed") {
      setSaved(t("admin.editor.not_saved"));
      toast(t("admin.editor.not_saved_hint"), 6000);
      return res;
    }
    st.rev++;
    setSaved(here());
    quietTimer = setTimeout(() => setSaved(here(), true), 1800);
    if (mode !== "write" && root.isConnected) preview();
    sync?.changed();
    return res;
  };
  const dirty = () => {
    clearTimeout(saveTimer);
    clearTimeout(quietTimer);
    setSaved(t("admin.editor.saving"));
    unsaved = true;
    edits++;
    saveTimer = setTimeout(saveNow, 700);
  };
  setSaved(fromLocal || (post && pub.status === "draft") ? here() : "", true);

  /* ---------- two texts: the writer chooses, never a silent pick ---------- */
  const setText = (tt: string, bb: string) => {
    titleIn.value = tt;
    title = tt;
    fit();
    view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: bb } });
    count();
  };
  const choice = (head: string, hint: string, keep: [string, () => void], other: [string, () => void]) => {
    const pick = (f: () => void) => () => {
      choiceSlot.replaceChildren();
      f();
      pubState();
    };
    choiceSlot.replaceChildren(
      h(
        "div",
        { class: "banner choice", role: "alert" },
        icon("alert"),
        h("div", { class: "grow" }, h("b", {}, head), " ", hint),
        h("button", { type: "button", class: "btn", onclick: pick(keep[1]) }, keep[0]),
        h("button", { type: "button", class: "btn quiet", onclick: pick(other[1]) }, other[0]),
      ),
    );
    pubState();
  };
  /** A server choice not picked yet: a tab choice shown over it hands back to it. */
  let pendingServer: Post | undefined;
  /** The server copy moved on since this device's draft began. Either pick bases the text on it. */
  const serverChoice = (p: Post) => {
    pendingServer = p;
    // A tab choice is open (paused): it hands back to this one once picked (otherTab settle).
    if (st.paused && choiceSlot.childElementCount > 0) return;
    const adopt = () => {
      pendingServer = undefined;
      Object.assign(st, { hold: false, base: p.updated_at, version: p.version, server: [p.title, p.markdown] });
    };
    choice(
      t("admin.editor.conflict.server_title"),
      t("admin.editor.conflict.server_hint"),
      [t("admin.editor.conflict.keep_mine"), () => (adopt(), dirty())],
      [t("admin.editor.conflict.use_saved"), () => (adopt(), setText(p.title, p.markdown))],
    );
  };
  /**
   * Another tab saved this draft after this tab last read it, or (`other`) another page left a
   * side copy of it. Copies go only once the writer's pick is in the draft: this page's own,
   * and the other one if nothing was written to it since it was offered.
   */
  const otherTab = (other?: { key: string; rev: number }) => {
    const settle = async (dropOther: number | undefined) => {
      if ((await saveNow()) !== "saved") return;
      await dropOwn();
      if (other && dropOther !== undefined) await dropDraft(other.key, dropOther);
      const next = offers.shift();
      if (next) {
        st.paused = true;
        return otherTab(next);
      }
      if (pendingServer && choiceSlot.childElementCount === 0) serverChoice(pendingServer);
    };
    choice(
      t("admin.editor.conflict.tab_title"),
      t("admin.editor.conflict.tab_hint"),
      [
        t("admin.editor.conflict.keep_this"),
        async () => {
          st.rev = (await loadDraft(st.key))?.rev ?? 0;
          st.paused = false;
          clearTimeout(saveTimer);
          await settle(other?.rev);
        },
      ],
      [
        t("admin.editor.conflict.load_other"),
        async () => {
          // Read now: the other page may have written more since the choice was shown.
          const theirs = other && (await loadDraft(other.key));
          const main = await loadDraft(st.key);
          const d = theirs ?? main;
          st.paused = false;
          clearTimeout(saveTimer);
          st.rev = main?.rev ?? 0;
          if (d) {
            st.base = d.base;
            st.version = d.version;
            setText(d.title, d.body);
          }
          await settle(theirs?.rev);
        },
      ],
    );
  };

  const count = () => {
    const words = countWords(`${titleIn.value} ${view.state.doc.toString()}`);
    // A blank page has no reading time: "0 words, 1 min" would be wrong.
    const text = words ? t("admin.editor.words", { count: words, minutes: num(Math.max(1, Math.round(words / 220))) }) : t("admin.editor.words_none");
    wc.textContent = text;
    wc2.textContent = text;
    pubState();
  };
  /* ---------- publishing (publish.ts, loaded on first use) ---------- */
  let working = false;
  /** The server copy was trashed elsewhere: nothing is sent from here again. */
  let trashed = false;
  /** Nothing to publish yet, or a choice between two texts is waiting. */
  const pubState = () => {
    pubBtn.disabled = working || trashed || st.hold || st.paused || (!titleIn.value.trim() && view.state.doc.length === 0);
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
    // On touch screens the system's own menu sits on a selection and the bottom bar already
    // acts on it: a floating bubble would only fight them.
    if (r.empty || !view.hasFocus || matchMedia("(pointer: coarse)").matches) return hideBubble();
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
    const y = a.top - bubble.offsetHeight - 10;
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

  /* ---------- pasting and pictures ---------- */
  /** Asks for a picture's description after it is added; it stays empty if the writer skips. */
  const altPrompt = (url: string) => {
    const src = `![](${url})`;
    if (!view.state.doc.toString().includes(src)) return;
    const input = h("input", { class: "input", id: "pb-alt", autocomplete: "off", "aria-describedby": "pb-alt-hint" });
    const formEl = h(
      "form",
      { class: "altpop" },
      h("label", { class: "label", for: "pb-alt" }, t("admin.editor.upload.alt_label")),
      input,
      h("p", { class: "hint", id: "pb-alt-hint" }, t("admin.editor.upload.alt_hint")),
      h(
        "div",
        { class: "btn-row end" },
        h("button", { type: "button", class: "btn quiet", onclick: () => (closePop(false), view.focus()) }, t("admin.editor.upload.alt_skip")),
        h("button", { type: "submit", class: "btn primary" }, t("admin.editor.upload.alt_save")),
      ),
    );
    formEl.addEventListener("submit", (e) => {
      e.preventDefault();
      closePop(false);
      // Found again by its address: the text may have moved while the writer typed.
      const now = view.state.doc.toString().indexOf(src);
      const alt = mdAlt(input.value);
      if (now >= 0 && alt) view.dispatch({ changes: { from: now + 2, insert: alt } });
      view.focus();
    });
    // Under the picture itself, so the writer sees what they describe.
    const pic = Array.from(view.dom.querySelectorAll(".cm-img img")).find((i) => i.getAttribute("src") === url)?.parentElement;
    openPop(pic ?? cmHost, formEl, t("admin.editor.upload.alt_label"));
  };

  /** Puts `md` on lines of its own at the end of the line holding `at`. */
  const insertOwnLine = (at: number, md: string) => {
    const line = view.state.doc.lineAt(at);
    const before = line.text.trim() ? "\n\n" : "";
    const next = line.number < view.state.doc.lines ? view.state.doc.line(line.number + 1).text.trim() : "";
    const after = next ? "\n\n" : line.number < view.state.doc.lines ? "" : "\n";
    view.dispatch({
      changes: { from: line.to, insert: before + md + after },
      effects: EditorView.scrollIntoView(line.to + before.length, { y: "center" }),
      userEvent: "input.paste",
    });
  };

  /** One picture: a card with progress where it goes, then its Markdown, then its description. */
  const uploadOne = async (file: File, pos: number): Promise<void> => {
    const bar = h("i", {});
    const label = h("span", {}, t("admin.editor.upload.sending", { name: file.name }));
    const card = h("div", { class: "up", role: "status" }, label, h("div", { class: "bar" }, bar));
    bar.style.transform = "scaleX(0)";
    // The outer box holds the card's space above and below (a widget may not have margins).
    const id = showCard(view, pos, h("div", { class: "cm-upload" }, card));
    const ctl = new AbortController();
    const acts = h("div", { class: "btn-row" }, h("button", { type: "button", class: "btn quiet", onclick: () => ctl.abort() }, t("admin.editor.upload.cancel")));
    card.append(acts);
    const r = await o.api.upload(file, (sent) => (bar.style.transform = `scaleX(${sent})`), ctl.signal);
    if (r.ok) {
      const at = removeCard(view, id) ?? view.state.doc.length;
      insertOwnLine(at, `![](${r.value.url})`);
      // After CodeMirror has scrolled to it and drawn the picture.
      const url = r.value.url;
      requestAnimationFrame(() => requestAnimationFrame(() => altPrompt(url)));
      return;
    }
    // Cancelled by the writer: no message, the text is untouched.
    if (r.error.code === "cancelled") {
      removeCard(view, id);
      view.focus();
      return;
    }
    // Failed: the card says so and offers another try; the text is untouched.
    card.className = "up fail";
    card.setAttribute("role", "alert");
    label.textContent = r.error.code === "network" ? t("admin.editor.upload.offline") : t("admin.editor.upload.failed", { name: file.name });
    const retry = h("button", { type: "button", class: "btn quiet" }, t("admin.editor.upload.retry"));
    const remove = h("button", { type: "button", class: "btn quiet" }, t("admin.editor.upload.remove"));
    card.querySelector(".bar")?.remove();
    acts.replaceChildren(retry, remove);
    remove.onclick = () => (removeCard(view, id), view.focus());
    retry.onclick = () => {
      const at = removeCard(view, id);
      void uploadOne(file, at ?? pos);
    };
  };

  /** Pictures from a paste, a drop or the picker, one after another; anything else is refused. */
  const addPictures = async (files: File[], pos: number): Promise<void> => {
    if (o.offline()) {
      toast(t("admin.editor.upload.offline"), 6000);
      return;
    }
    for (const f of files) {
      if (!f.type.startsWith("image/")) toast(t("admin.editor.upload.not_picture", { name: f.name }), 6000);
      else if (f.size > MAX_PICTURE) toast(t("admin.editor.upload.too_big", { name: f.name }), 6000);
      else await uploadOne(f, pos);
    }
  };

  const fileInput = h("input", { type: "file", accept: "image/*", multiple: true, hidden: true, tabindex: "-1" });
  let pickAt = 0;
  fileInput.addEventListener("change", () => {
    const files = Array.from(fileInput.files ?? []);
    fileInput.value = "";
    if (files.length) void addPictures(files, pickAt);
  });
  picker.open = (at) => {
    pickAt = at;
    fileInput.click();
  };

  const onPaste = (e: ClipboardEvent, v: EditorView): boolean => {
    const d = e.clipboardData;
    if (!d) return false;
    const sel = v.state.selection.main;
    const files = Array.from(d.files);
    const html = d.getData("text/html");
    const doc = html ? new DOMParser().parseFromString(html, "text/html") : undefined;
    // Word, Excel and other Office apps put a PNG of the selection next to the HTML: when the HTML
    // has text, that is what was copied, so files count only for pictures copied without text.
    if (files.length && !doc?.body.textContent?.trim()) {
      e.preventDefault();
      void addPictures(files, sel.head);
      return true;
    }
    const text = d.getData("text/plain");
    // An address pasted over selected words links them.
    if (!sel.empty && /^(https?:\/\/|mailto:)\S+$/.test(text.trim())) {
      e.preventDefault();
      link(v, sel.from, sel.to, mdUrl(text.trim()), "");
      return true;
    }
    if (!doc || inCode(v.state, sel.head)) return false;
    // Rich text is converted by a small module loaded on the first such paste.
    e.preventDefault();
    const put = (md: string) => {
      const out = md || text;
      // Blocks (several paragraphs, a table, a list, a heading) get lines of their own: glued
      // to the text around the caret they would stop being blocks. One plain run stays inline.
      const r = v.state.selection.main;
      const block = md !== "" && (md.includes("\n") || /^(#{1,6} |[-+] |\d+\. |> |\||```)/.test(md));
      const before = v.state.doc.lineAt(r.from);
      const after = v.state.doc.lineAt(r.to);
      const lead = block && v.state.sliceDoc(before.from, r.from).trim() ? "\n\n" : "";
      const tail = block && v.state.sliceDoc(r.to, after.to).trim() ? "\n\n" : "";
      v.dispatch(v.state.replaceSelection(lead + out + tail), { userEvent: "input.paste", scrollIntoView: true });
    };
    import("./paste-md").then(
      (m) => {
        let md = "";
        try {
          // The parsed copy is only read, never inserted into the page.
          md = m.isRich(html) ? m.htmlToMarkdown(doc.body) : "";
        } catch (err) {
          console.error("admin editor: converting the pasted HTML failed", err);
        }
        put(md);
      },
      (err: unknown) => {
        // Offline before the module was ever loaded: paste the plain text, never nothing.
        console.error("admin editor: loading the paste converter failed", err);
        put("");
      },
    );
    return true;
  };

  const onDrop = (e: DragEvent, v: EditorView): boolean => {
    const files = Array.from(e.dataTransfer?.files ?? []);
    if (!files.length) return false;
    e.preventDefault();
    const at = v.posAtCoords({ x: e.clientX, y: e.clientY }) ?? v.state.selection.main.head;
    void addPictures(files, at);
    return true;
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
    // The toolbar floats at the top of the column, or at the bottom on phones.
    margins: () => (phone() ? { bottom: toolsWrap.offsetHeight } : { top: toolsWrap.offsetHeight }),
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
      paste: onPaste,
      drop: onDrop,
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
      picture: () => picker.open(view.state.selection.main.head),
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
  // The keys typed within the debounce are saved now when the page may go: the tab hidden or
  // closed, or the session expired (main.ts waits for this before it leaves for sign-in).
  const flush = (): Promise<unknown> => {
    clearTimeout(saveTimer);
    if (unsaved) flushed = saveNow();
    return flushed.then(() => st.q);
  };
  /**
   * The page is hidden or going away: it may not live to finish an IndexedDB request, so the
   * unsaved keys go to this page's own copy at once (putNow), then the usual save runs. Once the
   * text is in the draft the copy goes; otherwise (paused, or storage refused) it stays as the
   * text's only home and is offered when the post is next opened.
   */
  const leaving = () => {
    if (!unsaved) return void flush();
    const d = { title: titleIn.value, body: view.state.doc.toString(), base: st.base, version: st.version, savedAt: Date.now() };
    const to = ownNext();
    if (putNow(to.key, { ...d, rev: to.rev })) own = to;
    const paused = st.paused;
    clearTimeout(saveTimer);
    const job = saveNow();
    flushed = job;
    void job.then((r) => !paused && r === "saved" && dropOwn());
  };
  const onHide = () => {
    if (document.visibilityState === "hidden") leaving();
  };
  const onPageHide = () => leaving();
  session.flush = flush;
  addEventListener("pagehide", onPageHide);
  document.addEventListener("visibilitychange", onHide);
  // Leaving the screen (a route change replaces the root) drops the listeners with it.
  new MutationObserver((_, obs) => {
    if (!root.isConnected) {
      // Redrawn or left within the debounce: the last keys are saved now, under the current key.
      clearTimeout(saveTimer);
      if (unsaved) flushed = saveNow();
      if (session.flush === flush) session.flush = undefined;
      removeEventListener("pagehide", onPageHide);
      document.removeEventListener("visibilitychange", onHide);
      removeEventListener("resize", relayout);
      vv?.removeEventListener("resize", keyboard);
      vv?.removeEventListener("scroll", keyboard);
      hideBubble();
      closeSlash();
      obs.disconnect();
    }
  }).observe(o.root, { childList: true });

  drawModes();
  if (serverMoved && post) pendingServer = post;
  const first = offers.shift();
  if (first) otherTab(first);
  else if (pendingServer) serverChoice(pendingServer);
  /** A sync is started or loading. */
  let syncing = false;
  /**
   * Bumped by every `begin` and `end`: a sync chunk that arrives late starts a sync only if its
   * `begin` is still the current one, so begin, end, begin while loading never runs two.
   */
  let syncGen = 0;
  const end = () => {
    syncing = false;
    syncGen++;
    sync?.stop();
    sync = undefined;
  };
  // Drafts also go to the server (sync.ts); published and scheduled posts stay here until Update.
  const begin = () => {
    if (syncing) return;
    syncing = true;
    const gen = ++syncGen;
    void import("./sync").then(
      (m) => {
        if (gen !== syncGen || !root.isConnected) return;
        sync = m.startSync({
          api: o.api,
          user: o.user,
          lang,
          st,
          id: pub.id,
          draft: pub.status === "draft",
          offline: o.offline,
          alive: () => root.isConnected,
          text: () => [titleIn.value, view.state.doc.toString()],
          say: (s) => setSaved(s),
          choose: serverChoice,
          live,
          made: (p) => {
            created(pub, p);
            ui.draw?.();
          },
          note,
        });
      },
      (e: unknown) => console.error("admin editor: loading the server sync failed; drafts stay on this device", e),
    );
  };
  const ui: PubUi = {};
  /** The status line follows the text: "Saved" only while the server holds exactly what is here. */
  const line = () => {
    const sv = st.server;
    setSaved(sv && sv[0] === titleIn.value && sv[1] === view.state.doc.toString() ? t("admin.editor.saved") : here(), true);
  };
  /** `pub.status` changed: the chip, the button and draft saving follow it. */
  const restatus = () => {
    const s = trashed ? "trashed" : pub.status;
    chip.className = `st ${s}`;
    chip.textContent = statusName(s);
    stLabel.textContent = statusName(s);
    pubBtn.textContent = pubLabel(pub);
    // Only a draft autosaves; a live post's edits reach the server only by Update (publish.ts).
    if (pub.status !== "draft" || trashed) end();
    else begin();
    pubState();
  };
  /** The server copy's status moved on elsewhere (sync.ts learn, or a publish step's stale read). */
  function live(p: Post): void {
    if (p.status === "trashed") trashed = true;
    else fromPost(pub, p);
    restatus();
    line();
    ui.draw?.();
  }
  if (pub.status === "draft") begin();
  const applied = () => {
    restatus();
    line();
  };
  const host: PubHost = {
    api: o.api,
    root,
    body: edBody,
    pub,
    st,
    lang,
    ui,
    text: () => [titleIn.value, view.state.doc.toString()],
    sync: () => sync,
    offline: o.offline,
    wide,
    busy: (on) => {
      working = on;
      pubState();
    },
    applied,
    relabel: () => {
      pubBtn.textContent = pubLabel(pub);
    },
    live,
    choose: serverChoice,
  };
  const loadPublish = (f: (m: typeof import("./publish")) => void) =>
    void import("./publish").then(f, (e: unknown) => {
      console.error("admin editor: loading the publish flow failed", e);
      toast(t("admin.editor.pub.failed"), 6000);
    });
  pubBtn.addEventListener("click", () => loadPublish((m) => m.publishClick(host, pubBtn)));
  gear.addEventListener("click", () => loadPublish((m) => m.togglePanel(host)));
  pubState();
  if (!body && !title) titleIn.focus();
  else if (!phone()) {
    view.focus();
    view.dispatch({ selection: { anchor: 0 } });
  }
}
