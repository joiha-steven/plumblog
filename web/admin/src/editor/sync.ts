/**
 * Saving a draft post to the server (docs/ARCHITECTURE.md, Admin and editor). The editor keeps
 * every change on this device first (drafts.ts); this sends the text about 3 s after the last
 * local save, one request at a time, only while online and only for drafts: a published or
 * scheduled post is never sent, so editing one never changes the live page; it stays on this
 * device until the publish step. That holds for a draft published elsewhere while open too: the
 * first server copy seen that is not a draft (a 409's fetch, any answer) stops sending for good.
 *
 * A new post becomes a server post on its first save: its URL and its draft key move to the
 * post's id. Every update is conditional on the version the text is based on; a stale one
 * stops saving and asks the writer which text to keep, never a silent pick, unless the server
 * copy is this editor's text already or exactly an update of its own that got no answer. The
 * local draft is dropped once the server holds exactly the text still in the editor.
 *
 * A mount redrawn (online/offline) while the create is out waits for it up to 8 s and opens
 * the post; past that it shows the local draft, sends nothing and adopts the post when the
 * create settles (`settled`, `adopt`).
 *
 * Every create of one new post carries the same `client_id` (`createId`, kept in localStorage
 * under the draft key until a create answers), so the server returns the post an earlier
 * attempt made instead of making a second one (#86). A create with no answer (a network error,
 * or none within `createMs`; the API client sets creates no bound) is sent again with it, a few
 * times with growing waits, and so is the next one after a reload. A reload while the create
 * is out still asks first (beforeunload). Only without localStorage (the id then lives in this
 * page) can a reload after a lost answer make a second server draft (slug `-2`).
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
  /**
   * The server copy is no longer a draft (published, scheduled or trashed elsewhere): nothing
   * more is sent from this editor; it shows the post's status and keeps the text here.
   */
  live(p: Post): void;
  /** A new post was created from this draft: the editor now knows its id and slug. */
  made(p: Post): void;
  /** Where the renderer's warnings show, under the title. */
  note: HTMLElement;
}

export interface Sync {
  /** Called after every local save. */
  changed(): void;
  /**
   * Pauses autosave and resolves when no request of this sync is out, with the post as the
   * server last answered (a new post is created first, by the same path as an autosave), or
   * undefined when there is none (empty, offline, held for a choice, failed). Waiting text is
   * not sent: the publish update carries it. The publish flow awaits this, sends its one update
   * and then calls `resume`, so no autosave goes out with the same expected version.
   */
  flush(): Promise<Post | undefined>;
  /** Autosave goes on after `flush`. */
  resume(): void;
  /** The post's id once it exists on the server. */
  id(): number | undefined;
  /** Stops saving for good (the post is no longer a draft); a pending save is dropped. */
  stop(): void;
}

const MAX_SLUG = 100;
const SLUG_TRIES = 5;
/** Sends of a create with no answer after the first, waiting `retry`, 2x, 4x... in between. */
const CREATE_RETRIES = 4;

/** The new post's `client_id` by draft key, for this page; localStorage keeps it over a reload. */
const createIds = new Map<string, string>();
const createIdKey = (key: string): string => `plumblog-create:${key}`;

/** The request id every create of the new post under draft key `key` sends (crypto.randomUUID). */
export function createId(key: string): string {
  let id = createIds.get(key);
  if (id) return id;
  try {
    id = localStorage.getItem(createIdKey(key)) ?? undefined;
  } catch (e) {
    console.warn("admin editor: localStorage unreadable; a create's id lasts this page only", e);
  }
  if (!id) {
    id = crypto.randomUUID();
    try {
      localStorage.setItem(createIdKey(key), id);
    } catch (e) {
      console.warn("admin editor: localStorage unwritable; a create's id lasts this page only", e);
    }
  }
  createIds.set(key, id);
  return id;
}

/** A create answered (a post, or a refusal that made none): the next post gets a new id. */
function forgetCreateId(key: string): void {
  createIds.delete(key);
  try {
    localStorage.removeItem(createIdKey(key));
  } catch (e) {
    console.warn("admin editor: localStorage unwritable; a used create id stays", e);
  }
}

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

/** Moves a new post's draft to the created post's key, based on its version. Resolves to its rev. */
const moveTo = (key: string, user: number, p: Post): Promise<number> =>
  moveDraft(key, draftKey(user, p.id, ""), { base: p.updated_at, version: p.version });

/**
 * For a mount of a new post's editor: the post an earlier mount's create made of this draft
 * key, with the draft moved to it, or undefined (no create, it failed, or it is still out after
 * `ms`: a hung request never keeps the writer on the loading frame). The mount then goes on;
 * its sync holds for that create, never starts another, and adopts the post when it settles.
 */
export async function settled(key: string, user: number, ms = 8000): Promise<Post | undefined> {
  const prior = creates.get(key);
  if (!prior) return undefined;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const p = await Promise.race([prior, new Promise<undefined>((ok) => (timer = setTimeout(ok, ms)))]);
  clearTimeout(timer);
  if (p) await moveTo(key, user, p);
  return p;
}

const same = (a: [string, string], b: [string, string] | undefined): boolean => b !== undefined && a[0] === b[0] && a[1] === b[1];

/** Runs `f` after every earlier draft read-modify-write of this editor. */
export const serial = <T>(st: EdState, f: () => Promise<T>): Promise<T> => {
  const p = st.q.then(f);
  st.q = p.catch(() => undefined);
  return p;
};

/**
 * The server answered with `p` for the editor's text `text` (a publish or an unpublish, not an
 * autosave): its version and text become the base, and the local draft goes when it holds
 * nothing the server lacks. The same drop `tidy` does after a save.
 */
export async function accept(st: EdState, p: Post, text: () => [string, string]): Promise<void> {
  Object.assign(st, { base: p.updated_at, version: p.version, server: [p.title, p.markdown] });
  const rev = st.rev;
  await serial(st, async () => {
    if (rev && st.rev === rev && same(text(), st.server) && (await dropDraft(st.key, rev))) st.rev = 0;
  });
}

/**
 * `slow`: a create still out after this long (its retries included) says so on the status line.
 * `createMs`: a create with no answer by then is sent again (the first stays out; same id).
 * `retry`: the first wait before a create with no answer is sent again.
 */
export function startSync(o: SyncHost, delay = 3000, slow = 20_000, createMs = 20_000, retry = 1000): Sync {
  if (!o.draft) return { changed: () => undefined, flush: async () => undefined, resume: () => undefined, id: () => o.id, stop: () => undefined };
  const st = o.st;
  let id = o.id;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let busy = false;
  let again = false;
  /** A publish step is out (`flush` to `resume`): nothing is sent from here meanwhile. */
  let held = false;
  /** The request in flight (`send` with its follow-ups), for `flush`. */
  let running: Promise<void> | undefined;
  /** The newest post the server answered with. */
  let last: Post | undefined;
  /** A create has been out longer than `slow`: the status line says so until it settles. */
  let lagging = false;
  /** The last attempt failed: the next failure in a row adds no toast. */
  let failing = false;
  /** The server copy turned out not to be a draft: this editor never sends again. */
  let stopped = false;
  /** The text of an update that got no answer: it may have landed, so a 409 may be our own. */
  let lost: [string, string] | undefined;

  /**
   * Every server copy this editor learns of passes here. One that is not a draft stops the
   * sync for good: a PATCH without `status` keeps the live status, so sending would change the
   * live page without the writer's Update. True while it is still a draft.
   */
  const learn = (p: Post): boolean => {
    if (p.status === "draft") return true;
    if (!stopped) {
      stopped = true;
      clearTimeout(timer);
      console.warn(`admin editor: post ${p.id} is ${p.status} on the server; edits stay on this device`);
      if (o.alive()) o.live(p);
    }
    return false;
  };

  /** Drops the local draft when the server holds the editor's exact text and no save came since `rev`. */
  const tidy = (rev: number) =>
    serial(st, async () => {
      if (rev && st.rev === rev && same(o.text(), st.server) && (await dropDraft(st.key, rev))) st.rev = 0;
    });

  const changed = () => {
    clearTimeout(timer);
    // A local save just set the status line; a create still out keeps saying so.
    if (lagging) o.say(t("admin.editor.sync.slow"));
    if (st.hold || stopped || held) return;
    if (same(o.text(), st.server)) {
      o.say(t("admin.editor.saved"));
      void tidy(st.rev);
    } else timer = setTimeout(() => void send(), delay);
  };

  const hold = (e: BeforeUnloadEvent) => e.preventDefault();
  /** One send of a create; no answer within `createMs` reads as a network failure. */
  const sendCreate = async (input: PostCreateInput): Promise<Result<Post>> => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const none = new Promise<Result<Post>>((ok) => {
      timer = setTimeout(() => {
        console.warn(`admin editor: the create of ${input.slug} has no answer within ${createMs} ms`);
        ok({ ok: false, error: { code: "network", status: 0 } });
      }, createMs);
    });
    try {
      return await Promise.race([o.api.createPost(input), none]);
    } finally {
      clearTimeout(timer);
    }
  };
  /** A create, sent again with the same body and id while it gets no answer (bounded). */
  const createOnce = async (input: PostCreateInput): Promise<Result<Post>> => {
    let r = await sendCreate(input);
    for (let i = 0; i < CREATE_RETRIES && !r.ok && r.error.code === "network"; i++) {
      if (stopped || !o.alive() || o.offline()) break;
      const wait = retry * 2 ** i;
      console.warn(`admin editor: sending the create of ${input.slug} again in ${wait} ms`);
      await new Promise((ok) => setTimeout(ok, wait));
      if (stopped || !o.alive() || o.offline()) break;
      r = await sendCreate(input);
    }
    return r;
  };
  const create = async (title: string, markdown: string): Promise<Result<Post>> => {
    addEventListener("beforeunload", hold);
    try {
      const client_id = createId(st.key);
      for (let n = 1; n <= SLUG_TRIES; n++) {
        const input: PostCreateInput = { lang: o.lang, slug: slugify(title, n), title, markdown, client_id };
        const r = await createOnce(input);
        if (r.ok || r.error.field !== "slug") return r;
      }
      console.error(`admin editor: slug ${slugify(title)} and ${SLUG_TRIES - 1} variants are taken`);
      return { ok: false, error: { code: "store.conflict", status: 409, field: "slug_all" } };
    } finally {
      removeEventListener("beforeunload", hold);
    }
  };

  /**
   * The post `p` was made of this draft (by this mount, or by an earlier one this mount waited
   * for): the draft, then the URL, now name it. An editor no longer on the page leaves the move
   * to the next mount (`settled` or its own `adopt`), so two never move one draft.
   */
  const adopt = (p: Post) =>
    serial(st, async () => {
      if (!o.alive() || st.key.includes(":post-")) return;
      id = p.id;
      last = p;
      o.made(p);
      const rev = await moveTo(st.key, o.user, p);
      st.key = draftKey(o.user, p.id, "");
      // Moved already (nothing here): read it. Another tab's rev is kept apart: the next save here then asks.
      if (!rev) st.rev = (await loadDraft(st.key))?.rev ?? 0;
      else if (rev !== st.rev) console.warn(`admin editor: draft moved at rev ${rev}, this tab had ${st.rev}`);
      Object.assign(st, { base: p.updated_at, version: p.version, server: [p.title, p.markdown] });
      history.replaceState(history.state, "", editHref(p.id));
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
    // Stopped meanwhile (trashed, or the publish flow took over): its own path decides.
    if (stopped) {
      st.hold = false;
      return;
    }
    if (!r.ok) {
      st.hold = false;
      return failed(r.error);
    }
    const p = r.value;
    const draft = learn(p);
    const ours = lost;
    lost = undefined;
    if (same(o.text(), [p.title, p.markdown])) {
      Object.assign(st, { hold: false, base: p.updated_at, version: p.version, server: [p.title, p.markdown] });
      o.say(draft ? t("admin.editor.saved") : t("admin.editor.saved_here"));
      return tidy(st.rev);
    }
    // The server copy is exactly an update of ours whose answer was lost, typed on since: no
    // one else's text to keep, so the newer text goes out on that version.
    if (draft && same([p.title, p.markdown], ours)) {
      console.warn(`admin editor: post ${p.id} version ${p.version} is this editor's unanswered save; sending the newer text`);
      Object.assign(st, { hold: false, base: p.updated_at, version: p.version, server: [p.title, p.markdown] });
      again = true;
      return;
    }
    // Not a draft: either pick only keeps the text on this device (learn stopped the sync).
    o.say(draft ? t("admin.editor.not_saved") : t("admin.editor.saved_here"));
    o.choose(p);
  };

  /** `force`: the create `flush` itself needs, while autosave is held. */
  const send = async (force = false): Promise<void> => {
    if (stopped || (held && !force) || st.hold || st.paused || o.offline() || !o.alive()) return;
    if (busy) {
      again = true;
      return;
    }
    // Busy until the answer is handled, so a change meanwhile never starts a second create.
    busy = true;
    running = (async () => {
      try {
        await attempt();
      } finally {
        busy = false;
      }
      if (again && o.alive() && !stopped && !held) {
        again = false;
        await send();
      }
    })();
    await running;
  };

  const flush = async (): Promise<Post | undefined> => {
    held = true;
    clearTimeout(timer);
    while (busy) await running;
    // Only a post that does not exist yet needs a send: the publish update carries the text.
    if (id === undefined) await send(true);
    while (busy) await running;
    return last;
  };

  const attempt = async (): Promise<void> => {
    const prior = id === undefined ? creates.get(st.key) : undefined;
    if (prior) {
      const p = await prior;
      if (p) await adopt(p);
      if (!o.alive() || stopped) return;
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
      const late = setTimeout(() => {
        lagging = true;
        console.warn(`admin editor: the create of ${key} has been out for ${slow} ms`);
        if (o.alive()) o.say(t("admin.editor.sync.slow"));
      }, slow);
      try {
        r = await create(title, markdown);
      } finally {
        clearTimeout(late);
        lagging = false;
      }
      // Only a post or a refusal (4xx) is sure: after no answer or a server error the post may
      // exist, so the id stays for the next try (here or after a reload).
      if (r.ok || (r.error.status >= 400 && r.error.status < 500)) forgetCreateId(key);
      if (r.ok) {
        id = r.value.id;
        await adopt(r.value);
        // An earlier attempt's post (same id) with its older text: this text goes as an update.
        if (!same(text, [r.value.title, r.value.markdown])) again = true;
      } else creates.delete(key);
      done(r.ok ? r.value : undefined);
    } else {
      r = await o.api.updatePost(id, { expected_version: st.version ?? 0, title, markdown });
      if (!r.ok && r.error.code === "network") lost = [title, markdown];
      else if (r.ok) lost = undefined;
    }
    // Stopped while the request was out (trashed, or not a draft any more): its answer changes nothing here.
    if (stopped) {
      console.warn(`admin editor: a save answered after this sync stopped (${r.ok ? `version ${r.value.version}` : r.error.code})`);
      return;
    }
    if (r.ok) {
      const p = r.value;
      // What the server holds is its answer, not what was sent: a normalised text is not "confirmed".
      Object.assign(st, { base: p.updated_at, version: p.version, server: [p.title, p.markdown] });
      last = p;
      failing = false;
      if (!learn(p)) return;
      if (o.alive()) {
        // Typed while the request was out: that text is not on the server yet.
        o.say(same(o.text(), st.server) ? t("admin.editor.saved") : t("admin.editor.saved_here"));
        // i18n: admin.editor.warning.markdown.math_invalid
        const notes = p.warnings.map((w: RenderWarning) => t(`admin.editor.warning.${w}`));
        o.note.replaceChildren(...(notes.length ? [h("p", { class: "hint", role: "status" }, notes.join(" "))] : []));
        await tidy(rev);
      }
    } else if (r.error.field === "expected_version" && id !== undefined) await stale(id);
    else failed(r.error);
  };

  // Mounted while an earlier mount's create is out: adopt it as soon as it settles.
  const prior = id === undefined ? creates.get(st.key) : undefined;
  if (prior) void prior.then((p) => p && adopt(p).then(changed));
  changed();
  return {
    changed,
    flush,
    resume: () => {
      held = false;
      again = false;
      changed();
    },
    id: () => id,
    stop: () => {
      stopped = true;
      clearTimeout(timer);
    },
  };
}
