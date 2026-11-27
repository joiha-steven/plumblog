/**
 * Saving a draft post to the server (docs/ARCHITECTURE.md, Admin and editor). The editor keeps
 * every change on this device first (drafts.ts); this sends the text about 3 s after the last
 * local save, one request at a time, only while online and only for drafts: a published or
 * scheduled post is never sent, so editing one never changes the live page; it stays on this
 * device until the publish step.
 *
 * A new post becomes a server post on its first save: its URL and its draft key move to the
 * post's id. Every update is conditional on the version the text is based on; a stale one
 * stops saving and asks the writer which text to keep, never a silent pick. The local draft
 * is dropped once the server holds exactly the text still in the editor.
 *
 * A mount redrawn (online/offline) while the create is out waits for it (`creates`) and opens
 * the post. A reload while the create request is out: the browser asks first (beforeunload). If the
 * page goes anyway and the create had reached the server, the reloaded editor still holds
 * the local draft under its new-post key and creates a second server draft (slug `-2`):
 * the text is never lost, and the extra draft shows in the posts list to delete.
 */

import type { Api, ApiError, Result } from "../api/client";
import type { Post, PostCreateInput, RenderWarning } from "../api/model.gen";
import { h } from "../dom";
import { t } from "../i18n";
import { editHref } from "../router";
import { toast } from "../ui";
import { draftKey, dropDraft, type EdState, loadDraft, moveDraft } from "./drafts";

export interface SyncHost {
  api: Api;
  user: number;
  lang: string;
  st: EdState;
  /** The post's id; undefined until a new post is created. */
  id: number | undefined;
  /** Whether the post is a draft (or new): nothing else is ever sent from here. */
  draft: boolean;
  offline(): boolean;
  /** False once the editor left the page. */
  alive(): boolean;
  text(): [string, string];
  /** Sets the status line ("Saved", "Not saved"). */
  say(text: string): void;
  /** Shows the server copy next to this text and lets the writer pick (index.ts serverChoice). */
  choose(p: Post): void;
  /** Where the renderer's warnings show, under the title. */
  note: HTMLElement;
}

export interface Sync {
  /** Called after every local save. */
  changed(): void;
}

const MAX_SLUG = 100;
const SLUG_TRIES = 5;

/** A URL slug from a title: diacritics folded, `a-z 0-9` and inner hyphens, at most 100. */
export const slugify = (title: string, n = 1): string => {
  const tail = n > 1 ? `-${n}` : "";
  const s = title
    .normalize("NFD")
    .replace(/\p{M}/gu, "")
    .replace(/[\u0111\u0110]/g, "d")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+/, "")
    .slice(0, MAX_SLUG - tail.length)
    .replace(/-+$/, "");
  return (s || "untitled") + tail;
};

/**
 * Creates by new-post draft key, shared by every mount of the editor in this page (online and
 * offline redraw it): resolves to the post once its draft has moved, or undefined if the create
 * failed. A later mount for the same key waits for it instead of creating a second post.
 */
const creates = new Map<string, Promise<Post | undefined>>();
export const settled = (key: string): Promise<Post | undefined> => creates.get(key) ?? Promise.resolve(undefined);

const same = (a: [string, string], b: [string, string] | undefined): boolean => b !== undefined && a[0] === b[0] && a[1] === b[1];

/** Runs `f` after every earlier draft read-modify-write of this editor. */
export const serial = <T>(st: EdState, f: () => Promise<T>): Promise<T> => {
  const p = st.q.then(f);
  st.q = p.catch(() => undefined);
  return p;
};

export function startSync(o: SyncHost, delay = 3000): Sync {
  if (!o.draft) return { changed: () => undefined };
  const st = o.st;
  let id = o.id;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let busy = false;
  let again = false;
  /** The last attempt failed: the next failure in a row adds no toast. */
  let failing = false;

  /** Drops the local draft when the server holds the editor's exact text and no save came since `rev`. */
  const tidy = (rev: number) =>
    serial(st, async () => {
      if (rev && st.rev === rev && same(o.text(), st.server) && (await dropDraft(st.key, rev))) st.rev = 0;
    });

  const changed = () => {
    clearTimeout(timer);
    if (st.hold) return;
    if (same(o.text(), st.server)) {
      o.say(t("admin.editor.saved"));
      void tidy(st.rev);
    } else timer = setTimeout(() => void send(), delay);
  };

  const hold = (e: BeforeUnloadEvent) => e.preventDefault();
  const create = async (title: string, markdown: string): Promise<Result<Post>> => {
    addEventListener("beforeunload", hold);
    try {
      for (let n = 1; n <= SLUG_TRIES; n++) {
        const input: PostCreateInput = { lang: o.lang, slug: slugify(title, n), title, markdown };
        const r = await o.api.createPost(input);
        if (r.ok || r.error.field !== "slug") return r;
      }
      console.error(`admin editor: slug ${slugify(title)} and ${SLUG_TRIES - 1} variants are taken`);
      return { ok: false, error: { code: "store.conflict", status: 409, field: "slug_all" } };
    } finally {
      removeEventListener("beforeunload", hold);
    }
  };

  /** A new post was created: its draft, then its URL, now name the post. */
  const created = (p: Post) =>
    serial(st, async () => {
      const newId = st.key.slice(st.key.indexOf(":new-") + 5);
      const to = draftKey(o.user, p.id, "");
      const rev = await moveDraft(st.key, to, { base: p.updated_at, version: p.version });
      st.key = to;
      // A draft changed by another tab keeps its own rev: the next save here then asks.
      if (rev !== st.rev) console.warn(`admin editor: draft moved at rev ${rev}, this tab had ${st.rev}`);
      // A mount redrawn meanwhile waits on `creates` and opens the post itself.
      if (o.alive() || new URLSearchParams(location.search).get("draft") === newId) history.replaceState(history.state, "", editHref(p.id));
    });

  /** Another mount created this post: continue as it, with the draft now under its key. */
  const join = (p: Post) =>
    serial(st, async () => {
      id = p.id;
      st.key = draftKey(o.user, p.id, "");
      st.rev = (await loadDraft(st.key))?.rev ?? 0;
      Object.assign(st, { base: p.updated_at, version: p.version, server: [p.title, p.markdown] });
      if (o.alive()) history.replaceState(history.state, "", editHref(p.id));
    });

  const failed = (e: ApiError) => {
    o.say(t("admin.editor.not_saved"));
    if (e.code === "auth.unauthenticated") return; // the client sends the writer to sign in
    if (!failing) toast(e.field === "slug_all" ? t("admin.editor.sync.slug_taken") : t("admin.editor.sync.failed"), 6000);
    failing = true;
  };

  /** A stale version: the server copy moved on. Same text needs no choice; else the writer picks. */
  const stale = async (pid: number) => {
    st.hold = true;
    const r = await o.api.post(pid);
    if (!r.ok) {
      st.hold = false;
      return failed(r.error);
    }
    const p = r.value;
    if (same(o.text(), [p.title, p.markdown])) {
      Object.assign(st, { hold: false, base: p.updated_at, version: p.version, server: [p.title, p.markdown] });
      o.say(t("admin.editor.saved"));
      return tidy(st.rev);
    }
    o.say(t("admin.editor.not_saved"));
    o.choose(p);
  };

  const send = async (): Promise<void> => {
    if (st.hold || st.paused || o.offline() || !o.alive()) return;
    if (busy) {
      again = true;
      return;
    }
    // Busy until the answer is handled, so a change meanwhile never starts a second create.
    busy = true;
    try {
      await attempt();
    } finally {
      busy = false;
    }
    if (again && o.alive()) {
      again = false;
      await send();
    }
  };

  const attempt = async (): Promise<void> => {
    const prior = id === undefined ? creates.get(st.key) : undefined;
    if (prior) {
      const p = await prior;
      if (p) await join(p);
    }
    const text = o.text();
    if (same(text, st.server)) return tidy(st.rev);
    // An empty new post stays on this device.
    if (id === undefined && !text[0].trim() && !text[1].trim()) return;
    const rev = st.rev;
    const [title, markdown] = text;
    let r: Result<Post>;
    if (id === undefined) {
      const key = st.key;
      let done: (p: Post | undefined) => void = () => undefined;
      creates.set(key, new Promise((ok) => (done = ok)));
      r = await create(title, markdown);
      if (r.ok) {
        id = r.value.id;
        await created(r.value);
      } else creates.delete(key);
      done(r.ok ? r.value : undefined);
    } else r = await o.api.updatePost(id, { expected_version: st.version ?? 0, title, markdown });
    if (r.ok) {
      const p = r.value;
      // What the server holds is its answer, not what was sent: a normalised text is not "confirmed".
      Object.assign(st, { base: p.updated_at, version: p.version, server: [p.title, p.markdown] });
      failing = false;
      if (o.alive()) {
        o.say(t("admin.editor.saved"));
        // i18n: admin.editor.warning.markdown.math_invalid
        const notes = p.warnings.map((w: RenderWarning) => t(`admin.editor.warning.${w}`));
        o.note.replaceChildren(...(notes.length ? [h("p", { class: "hint", role: "status" }, notes.join(" "))] : []));
        await tidy(rev);
      }
    } else if (r.error.field === "expected_version" && id !== undefined) await stale(id);
    else failed(r.error);
  };

  changed();
  return { changed };
}
