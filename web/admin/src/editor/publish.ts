/**
 * Publishing from the editor (docs/ARCHITECTURE.md, Admin and editor): the confirm popover,
 * the post settings panel (link, when, unpublish, trash) and the one update they send. Its own
 * lazy chunk, loaded on the first click of Publish or of the settings button.
 *
 * Publishing is ONE update with the editor's current text, the status, the time (for later)
 * and the slug (only if it changed), conditional on the version sync.ts holds. A new post is
 * created first by sync's own path (`flush`), never by a second create here. A stale version
 * goes to the same choice banner sync uses; nothing is picked silently.
 */

import type { Api, ApiError } from "../api/client";
import type { Post, PostUpdateInput } from "../api/model.gen";
import { h } from "../dom";
import { icon } from "../icons";
import { lang, num, t } from "../i18n";
import { HREF, go } from "../router";
import { closePop, openPop, toast } from "../ui";
import { countWords } from "./commands";
import type { EdState } from "./drafts";
import { type Pub, label } from "./pub";
import { type Sync, accept, slugify } from "./sync";

/** What the panel keeps between redraws. */
export interface PubUi {
  panel?: HTMLElement | undefined;
  scrim?: HTMLElement | undefined;
  /** Redraws the panel body, if it is open. */
  draw?: (() => void) | undefined;
  /** A problem the panel shows (a taken slug). */
  slugError?: string | undefined;
}

export interface PubHost {
  api: Api;
  /** The `.ed` element; the panel opens by a class on it. */
  root: HTMLElement;
  /** The `.ed-body` element the panel lives in. */
  body: HTMLElement;
  pub: Pub;
  st: EdState;
  lang: string;
  ui: PubUi;
  text(): [string, string];
  sync(): Sync | undefined;
  offline(): boolean;
  wide(): boolean;
  /** A publish step is out: the button is disabled. */
  busy(on: boolean): void;
  /** `pub` changed: redraw the chip, the button and start or stop draft saving. */
  applied(): void;
  /** The server copy moved on: the choice banner (index.ts serverChoice). */
  choose(p: Post): void;
}

const BASE = /-\d+$/;

/** The slug the post will have: what the writer typed, else the title's while it is still a draft named after it. */
export function slugPlan(pub: Pub, title: string, savedTitle: string | undefined): { slug: string; send: boolean } {
  const typed = pub.typed?.trim() ? slugify(pub.typed) : undefined;
  if (typed !== undefined) return { slug: typed, send: typed !== pub.slug };
  const auto = pub.status === "draft" && (pub.slug === "" || pub.slug.replace(BASE, "") === slugify(savedTitle ?? title));
  if (!auto) return { slug: pub.slug, send: false };
  const want = slugify(title);
  // No slug yet: the create names it from the title.
  const same = pub.slug === "" || pub.slug === want || (pub.slug !== "" && pub.slug.replace(BASE, "") === want);
  return same ? { slug: pub.slug || want, send: false } : { slug: want, send: true };
}

/** A `datetime-local` value (local time) for a time in ms. */
export const localValue = (ms: number): string => new Date(ms - new Date(ms).getTimezoneOffset() * 60000).toISOString().slice(0, 16);
const atMs = (v: string): number => (v ? new Date(v).getTime() : Number.NaN);
const when = (ms: number): string => new Intl.DateTimeFormat(lang(), { dateStyle: "medium", timeStyle: "short" }).format(ms);

/** Why the chosen time cannot be sent, or undefined. */
export const timeError = (p: Pub): string | undefined =>
  !p.later ? undefined : !p.at || Number.isNaN(atMs(p.at)) ? t("admin.editor.pub.time_missing") : atMs(p.at) <= Date.now() ? t("admin.editor.pub.time_past") : undefined;

const oops = (e: ApiError, what: string): void => {
  // The client sends the writer to sign in; nothing else to say.
  if (e.code === "auth.unauthenticated") return;
  console.error(`admin editor: ${what} failed: ${e.code} (${e.status})${e.field ? ` ${e.field}` : ""}`);
  toast(t("admin.editor.pub.failed"), 6000);
};

const sameText = (a: [string, string], p: Post): boolean => a[0] === p.title && a[1] === p.markdown;

/** The server copy moved on. Same text: take its version and go on (true); else the writer picks. */
async function stale(host: PubHost, id: number): Promise<boolean> {
  const { st } = host;
  st.hold = true;
  const r = await host.api.post(id);
  if (!r.ok) {
    st.hold = false;
    oops(r.error, "reading the post after a stale version");
    return false;
  }
  const p = r.value;
  if (sameText(host.text(), p)) {
    Object.assign(st, { hold: false, base: p.updated_at, version: p.version, server: [p.title, p.markdown] });
    return true;
  }
  host.choose(p);
  return false;
}

/** Sends one update built by `build`, after sync has sent what waits. The saved post, or undefined (said). */
async function commit(host: PubHost, build: () => PostUpdateInput): Promise<Post | undefined> {
  const p = await send(host, build);
  // Nothing went through: the autosave that `flush` held back starts again.
  if (!p) host.sync()?.changed();
  return p;
}

async function send(host: PubHost, build: () => PostUpdateInput): Promise<Post | undefined> {
  const { pub, st } = host;
  host.busy(true);
  try {
    for (let n = 0; n < 2; n++) {
      const sent = await host.sync()?.flush();
      if (sent) Object.assign(pub, { id: sent.id, slug: sent.slug });
      if (st.hold) return undefined; // a choice is pending
      if (pub.id === undefined) {
        toast(t("admin.editor.pub.failed"), 6000);
        return undefined;
      }
      const r = await host.api.updatePost(pub.id, build());
      if (r.ok) {
        await accept(st, r.value, host.text);
        return r.value;
      }
      if (r.error.field === "expected_version") {
        if (await stale(host, pub.id)) continue;
        return undefined;
      }
      if (r.error.field === "slug") {
        host.ui.slugError = t("admin.editor.pub.slug_conflict");
        toast(host.ui.slugError, 6000);
        host.ui.draw?.(); // already open: show the message
        openPanel(host);
        return undefined;
      }
      oops(r.error, "the update");
      return undefined;
    }
    return undefined;
  } finally {
    host.busy(false);
  }
}

/** The server's answer is now what the editor shows. */
function adopt(host: PubHost, p: Post): void {
  if (p.status === "trashed") return;
  Object.assign(host.pub, { id: p.id, status: p.status, publishedAt: p.published_at, slug: p.slug, typed: undefined, later: p.status === "scheduled", at: "" });
  host.ui.slugError = undefined;
  host.applied();
  host.ui.draw?.();
}

const row = (k: string, v: string): Node[] => [h("dt", {}, k), h("dd", {}, v)];
const cancelBtn = () => h("button", { class: "btn quiet", type: "button", onclick: () => closePop() }, t("admin.common.cancel"));

/** The Publish button was pressed. */
export function publishClick(host: PubHost, anchor: HTMLElement): void {
  const { pub, st } = host;
  const button = label(pub);
  if (host.offline()) {
    openPop(
      anchor,
      h("div", { class: "pubpop" }, h("h3", {}, t("admin.editor.pub.offline_title")), h("span", { class: "hint" }, t("admin.editor.pub.offline_hint", { button }))),
      t("admin.editor.pub.offline_title"),
      { below: true },
    );
    return;
  }
  const bad = timeError(pub);
  if (bad) {
    toast(bad, 6000);
    openPanel(host, true);
    return;
  }
  const [title, markdown] = host.text();
  const plan = slugPlan(pub, title, st.server?.[0]);
  const words = countWords(`${title} ${markdown}`);
  const question = pub.later ? t("admin.editor.pub.q_schedule") : pub.status === "published" ? t("admin.editor.pub.q_update") : t("admin.editor.pub.q_publish");
  const yes = h(
    "button",
    {
      class: "btn primary",
      type: "button",
      onclick: () => {
        closePop();
        void publish(host);
      },
    },
    pub.later || pub.status === "published" ? button : t("admin.editor.pub.publish_now"),
  );
  openPop(
    anchor,
    h(
      "div",
      { class: "pubpop" },
      h("h3", {}, question),
      h(
        "dl",
        { class: "kv" },
        row(t("admin.editor.pub.address"), `/${host.lang}/${plan.slug}`),
        row(t("admin.editor.pub.when"), pub.later ? when(atMs(pub.at)) : t("admin.editor.pub.now")),
        row(t("admin.editor.pub.length"), t("admin.editor.words", { count: words, minutes: num(Math.max(1, Math.round(words / 220))) })),
      ),
      h("div", { class: "btn-row end" }, cancelBtn(), yes),
    ),
    question,
    { below: true },
  );
}

async function publish(host: PubHost): Promise<void> {
  const { pub, st } = host;
  // The time may have passed while the popover was open.
  const bad = timeError(pub);
  if (bad) {
    toast(bad, 6000);
    return;
  }
  const later = pub.later;
  const wasScheduled = pub.status === "scheduled";
  const p = await commit(host, () => {
    const [title, markdown] = host.text();
    const plan = slugPlan(pub, title, st.server?.[0]);
    const input: PostUpdateInput = { expected_version: st.version ?? 0, title, markdown, status: later ? "scheduled" : "published" };
    // Now: the server keeps a live post's time; a scheduled one would keep its future time, so say now.
    if (later) input.published_at = atMs(pub.at);
    else if (wasScheduled) input.published_at = Date.now();
    if (plan.send) input.slug = plan.slug;
    return input;
  });
  if (!p) return;
  adopt(host, p);
  if (p.status === "scheduled") toast(t("admin.editor.pub.scheduled_toast", { when: when(p.published_at ?? 0) }), 5000);
  else toast(t("admin.editor.pub.published_toast"), 6000, { text: t("admin.editor.pub.view_post"), href: `/${p.lang}/${p.slug}` });
}

async function unpublish(host: PubHost): Promise<void> {
  const { st } = host;
  const p = await commit(host, () => ({ expected_version: st.version ?? 0, status: "draft", published_at: null }));
  if (!p) return;
  adopt(host, p);
  toast(t("admin.editor.pub.unpublished"), 5000);
}

async function trash(host: PubHost): Promise<void> {
  const { pub } = host;
  if (pub.id === undefined) return;
  host.busy(true);
  const r = await host.api.call("POST", `/api/v1/posts/${pub.id}/trash`);
  host.busy(false);
  if (!r.ok) return oops(r.error, "moving to the trash");
  // Nothing may save this post again; the local draft stays for a restore.
  host.sync()?.stop();
  go(HREF.posts);
  toast(t("admin.editor.pub.trashed"), 5000);
}

const ask = (anchor: HTMLElement, q: string, hint: string, text: string, then: () => void): void => {
  const yes = h("button", { class: "btn danger", type: "button", onclick: () => (closePop(), then()) }, text);
  openPop(anchor, h("div", { class: "confirm" }, h("h3", {}, q), h("p", { class: "hint" }, hint), h("div", { class: "btn-row end" }, cancelBtn(), yes)), q);
};

/* ---------- the settings panel ---------- */

export function closePanel(host: PubHost): void {
  const { ui } = host;
  host.root.classList.remove("panel-open");
  host.root.querySelector("[data-panel-btn]")?.setAttribute("aria-pressed", "false");
  ui.scrim?.remove();
  ui.scrim = undefined;
  ui.draw = undefined;
  ui.panel?.remove();
  ui.panel = undefined;
}

/** Opens the panel (it stays open if it is); `time` puts the cursor in the time field. */
export function openPanel(host: PubHost, time = false): void {
  const { pub, ui } = host;
  if (!ui.panel) {
    ui.panel = h("aside", { class: "panel", "aria-label": t("admin.editor.pub.settings") });
    host.body.append(ui.panel);
    host.root.classList.add("panel-open");
    host.root.querySelector("[data-panel-btn]")?.setAttribute("aria-pressed", "true");
    if (!host.wide()) {
      ui.scrim = h("div", { class: "scrim", onclick: () => closePanel(host) });
      host.root.append(ui.scrim);
    }
    ui.panel.addEventListener("keydown", (e) => {
      if (e.key !== "Escape") return;
      e.stopPropagation();
      closePanel(host);
      host.root.querySelector<HTMLElement>("[data-panel-btn]")?.focus();
    });
    ui.draw = () => draw(host);
    draw(host);
    ui.panel.querySelector<HTMLElement>("button, input")?.focus();
  }
  if (time) host.ui.panel?.querySelector<HTMLElement>(pub.later ? "[data-at]" : "[data-slug]")?.focus();
  else if (ui.slugError) ui.panel.querySelector<HTMLElement>("[data-slug]")?.focus();
}

export function togglePanel(host: PubHost): void {
  if (host.ui.panel) {
    closePanel(host);
    host.root.querySelector<HTMLElement>("[data-panel-btn]")?.focus();
  } else openPanel(host);
}

function draw(host: PubHost): void {
  const { pub, ui, st } = host;
  const panel = ui.panel;
  if (!panel) return;
  // A scheduled post shows its time; "later" with no time yet starts a day from now.
  if (!pub.at && pub.status === "scheduled" && pub.publishedAt) pub.at = localValue(pub.publishedAt);
  const plan = slugPlan(pub, host.text()[0], st.server?.[0]);
  const slug = h("input", {
    class: "input",
    id: "pub-slug",
    "data-slug": "",
    value: pub.typed ?? plan.slug,
    spellcheck: "false",
    autocapitalize: "none",
    autocomplete: "off",
    "aria-describedby": "pub-slug-hint",
    oninput: () => {
      pub.typed = slug.value;
      ui.slugError = undefined;
      err.textContent = "";
    },
    onchange: () => {
      // Normalised once the writer leaves the field; emptied, it goes back to following the title.
      pub.typed = slug.value.trim() ? slugify(slug.value) : undefined;
      slug.value = pub.typed ?? slugPlan(pub, host.text()[0], st.server?.[0]).slug;
    },
  });
  const prefix = `/${host.lang}/`;
  const err = h("p", { class: "err", role: "alert" }, ui.slugError ?? "");
  const at = h("input", {
    class: "input",
    type: "datetime-local",
    "data-at": "",
    value: pub.at,
    min: localValue(Date.now()),
    "aria-label": t("admin.editor.pub.when_at"),
    oninput: () => {
      pub.at = at.value;
      timeNote.textContent = "";
    },
  });
  at.hidden = !pub.later;
  const timeNote = h("p", { class: "err", role: "alert" });
  const pick = (later: boolean) => () => {
    pub.later = later;
    if (later && !pub.at) {
      pub.at = localValue(Date.now() + 86_400_000);
      at.value = pub.at;
    }
    at.hidden = !later;
    host.applied();
  };
  const button = (later: boolean) => label({ ...pub, later });
  const radio = (later: boolean, text: string) =>
    h("label", { class: "opt" }, h("input", { type: "radio", class: "rd", name: "pub-when", checked: pub.later === later, onchange: pick(later) }), text);
  const live = pub.status !== "draft";
  const danger = (key: "unpublish" | "trash", btn: string, q: string, hint: string, text: string, f: (h: PubHost) => Promise<void>) => {
    const b = h("button", { class: "btn quiet danger", type: "button", "data-act": key, onclick: () => ask(b, q, hint, text, () => void f(host)) }, btn);
    return h("div", {}, b);
  };
  panel.replaceChildren(
    h("div", { class: "panel-head" }, h("h2", {}, t("admin.editor.pub.settings")), h("button", { class: "ib", type: "button", "aria-label": t("admin.editor.pub.close"), onclick: () => togglePanel(host) }, icon("close"))),
    h(
      "div",
      { class: "panel-body" },
      h(
        "div",
        { class: "psec" },
        h("div", { class: "prow" }, h("label", { class: "k", for: "pub-slug" }, t("admin.editor.pub.slug")), h("div", { class: "prefix" }, h("span", {}, prefix), slug)),
        err,
        h("span", { class: "hint", id: "pub-slug-hint" }, pub.status === "published" ? t("admin.editor.pub.slug_hint_pub") : t("admin.editor.pub.slug_hint")),
      ),
      h(
        "div",
        { class: "psec" },
        h("div", { class: "cap" }, t("admin.editor.pub.publishing")),
        radio(false, t("admin.editor.pub.when_now", { button: button(false) })),
        radio(true, t("admin.editor.pub.when_later")),
        at,
        timeNote,
      ),
      pub.id === undefined
        ? null
        : h(
            "div",
            { class: "psec" },
            live ? danger("unpublish", t("admin.editor.pub.unpublish"), t("admin.editor.pub.unpublish_q"), t("admin.editor.pub.unpublish_hint"), t("admin.editor.pub.unpublish_go"), unpublish) : null,
            danger("trash", t("admin.editor.pub.trash"), t("admin.editor.pub.trash_q"), t("admin.editor.pub.trash_hint"), t("admin.editor.pub.trash"), trash),
          ),
    ),
  );
}
