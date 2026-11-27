/**
 * Drafts kept in this browser (IndexedDB), so writing survives a closed tab, a crash or no
 * network (docs/ARCHITECTURE.md, Admin and editor). A draft is dropped once the server holds
 * its exact text (editor/sync.ts).
 *
 * Nothing is ever picked silently: a draft remembers the server version it started from
 * (`base`), and every save is conditional on the draft not having changed under this tab
 * (`rev`), checked and written in one transaction.
 */

export interface Draft {
  title: string;
  body: string;
  /** The server copy's `updated_at` the draft started from; `null` for a post never saved. */
  base: number | null;
  /** The server `version` the draft started from; absent when unknown (older drafts, new posts). */
  version?: number | undefined;
  /** Raised by every save; a save expecting another value lost a race with another tab. */
  rev: number;
  /** Unix milliseconds, for display only: never compared with a server time. */
  savedAt: number;
}

import type { Post } from "../api/model.gen";

/** The editor's saving state, shared with index.ts. */
export interface EdState {
  /** The local draft's key; moves from `<user>:new-<id>` to `<user>:post-<id>` on create. */
  key: string;
  /** The server `updated_at` and `version` the editor's text is based on. */
  base: number | null;
  version: number | undefined;
  /** The stored draft's `rev` this tab last read or wrote. */
  rev: number;
  /** Another tab's newer draft is unresolved. */
  paused: boolean;
  /** The server copy moved on and the writer has not chosen yet: nothing is sent. */
  hold: boolean;
  /** The text the server holds (title, body); undefined for a post never saved. */
  server: [string, string] | undefined;
  /** Every draft read-modify-write runs through this chain, so none interleave. */
  q: Promise<unknown>;
}

/**
 * The editor's starting state from the stored draft and the server copy. The draft is the
 * writer's latest text, unless the server copy moved on since the version the draft started
 * from (its `updated_at` for drafts that do not know the version): then both exist, nothing is
 * sent (`hold`) and the writer chooses (never a silent pick).
 */
export function startState(key: string, local: Draft | undefined, post: Post | undefined): { st: EdState; fromLocal: boolean; serverMoved: boolean } {
  const same = local !== undefined && post !== undefined && local.title === post.title && local.body === post.markdown;
  const fromLocal = local !== undefined && !same;
  const serverMoved =
    fromLocal && post !== undefined && (local.version === undefined ? local.base !== post.updated_at : local.version !== post.version);
  const st: EdState = {
    key,
    base: fromLocal ? local.base : (post?.updated_at ?? null),
    // Until the writer picks, the draft keeps the version it started from, so a reload still
    // sees the server copy as moved and asks again (only the pick, index.ts adopt, changes it).
    version: serverMoved ? local?.version : post?.version,
    rev: local?.rev ?? 0,
    paused: false,
    hold: serverMoved,
    server: post && [post.title, post.markdown],
    q: Promise.resolve(),
  };
  return { st, fromLocal, serverMoved };
}

export type SaveResult = "saved" | "conflict" | "failed";

const DB = "plumblog-admin";
// Version 2: drafts are keyed per user and carry `base` and `rev`; version 1 drafts (one
// shared "new" key, no base) cannot be told apart safely, so the old store is dropped.
const VERSION = 2;
const STORE = "drafts";

/** The key of a post's draft for a user: `<user>:post-<id>`, or `<user>:new-<id>` for a post
 * not saved yet (each new post has its own, kept in its URL, so two never share one). */
export const draftKey = (user: number, post: number | undefined, newId: string): string =>
  post === undefined ? `${user}:new-${newId}` : `${user}:post-${post}`;

function open(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open(DB, VERSION);
    req.onupgradeneeded = () => {
      const db = req.result;
      if (db.objectStoreNames.contains(STORE)) db.deleteObjectStore(STORE);
      db.createObjectStore(STORE);
    };
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error ?? new Error("indexedDB.open failed"));
  });
}

function run<T>(mode: IDBTransactionMode, act: (s: IDBObjectStore, done: (v: T) => void) => void): Promise<T> {
  return open().then(
    (db) =>
      new Promise<T>((resolve, reject) => {
        const tx = db.transaction(STORE, mode);
        let out: T | undefined;
        let set = false;
        act(tx.objectStore(STORE), (v) => {
          out = v;
          set = true;
        });
        tx.oncomplete = () => {
          db.close();
          if (set) resolve(out as T);
          else reject(new Error("indexedDB transaction ended without a result"));
        };
        tx.onerror = tx.onabort = () => {
          db.close();
          reject(tx.error ?? new Error("indexedDB transaction failed"));
        };
      }),
  );
}

/** The draft saved under `key`, or undefined. A storage failure is logged and reads as none. */
export async function loadDraft(key: string): Promise<Draft | undefined> {
  try {
    return await run<Draft | undefined>("readonly", (s, done) => {
      const req = s.get(key);
      req.onsuccess = () => done(isDraft(req.result) ? req.result : undefined);
    });
  } catch (e) {
    console.error(`admin drafts: reading ${key} failed`, e);
    return undefined;
  }
}

/**
 * Saves `d` with `rev` one past `expectRev`, only if the stored draft is still at `expectRev`
 * (0: none stored). "conflict" when another tab saved in between; "failed" (and a log line)
 * when the browser refuses storage.
 */
export async function saveDraft(key: string, d: Omit<Draft, "rev">, expectRev: number): Promise<SaveResult> {
  try {
    return await run<SaveResult>("readwrite", (s, done) => {
      const req = s.get(key);
      req.onsuccess = () => {
        const cur: unknown = req.result;
        const rev = isDraft(cur) ? cur.rev : 0;
        if (rev !== expectRev) return done("conflict");
        s.put({ ...d, rev: expectRev + 1 }, key).onsuccess = () => done("saved");
      };
    });
  } catch (e) {
    console.error(`admin drafts: saving ${key} failed`, e);
    return "failed";
  }
}

/**
 * Removes a draft once its text is safe on the server, only if it is still at `expectRev`
 * (checked and deleted in one transaction, so a save made meanwhile is kept). True if removed.
 */
export async function dropDraft(key: string, expectRev: number): Promise<boolean> {
  try {
    return await run<boolean>("readwrite", (s, done) => {
      const req = s.get(key);
      req.onsuccess = () => {
        const cur: unknown = req.result;
        if (!isDraft(cur) || cur.rev !== expectRev) return done(false);
        s.delete(key).onsuccess = () => done(true);
      };
    });
  } catch (e) {
    console.error(`admin drafts: removing ${key} failed`, e);
    return false;
  }
}

/**
 * Moves the draft at `from` to `to` with `patch` applied, in one transaction, so a new post's
 * draft becomes the saved post's without a moment where both or neither exist. Resolves to
 * the moved draft's `rev` (0: nothing stored at `from`, or a storage failure, logged).
 */
export async function moveDraft(from: string, to: string, patch: Partial<Draft>): Promise<number> {
  try {
    return await run<number>("readwrite", (s, done) => {
      const req = s.get(from);
      req.onsuccess = () => {
        const cur: unknown = req.result;
        if (!isDraft(cur)) return done(0);
        s.put({ ...cur, ...patch }, to);
        s.delete(from).onsuccess = () => done(cur.rev);
      };
    });
  } catch (e) {
    console.error(`admin drafts: moving ${from} to ${to} failed`, e);
    return 0;
  }
}

/**
 * How many drafts `user` keeps here, so sign-out can warn before `clearDrafts` deletes them.
 * A storage failure is logged and counts as none (sign-out's clear would fail the same way).
 */
export async function countDrafts(user: number): Promise<number> {
  try {
    return await run<number>("readonly", (s, done) => {
      const req = s.count(IDBKeyRange.bound(`${user}:`, `${user}:\uffff`));
      req.onsuccess = () => done(req.result);
    });
  } catch (e) {
    console.error(`admin drafts: counting user ${user} failed`, e);
    return 0;
  }
}

/** A kept draft as the posts list shows it. */
export interface DraftEntry {
  key: string;
  title: string;
  savedAt: number;
  /** The post it edits, or the id of a post never saved (`newId`, in the editor's `?draft=`). */
  post?: number;
  newId?: string;
}

/**
 * The entry for one stored draft of `user`, or undefined for a key that is not theirs or not
 * ours, a stored value that is not a draft, or a draft with nothing written in it.
 */
export function toEntry(user: number, key: string, v: unknown): DraftEntry | undefined {
  const m = /^(\d+):(?:post-(\d{1,15})|new-([a-z0-9]{12,32}))$/.exec(key);
  if (!m || Number(m[1]) !== user || !isDraft(v) || (v.title.trim() === "" && v.body.trim() === "")) return undefined;
  const entry: DraftEntry = { key, title: v.title, savedAt: v.savedAt };
  if (m[2] !== undefined) entry.post = Number(m[2]);
  else if (m[3] !== undefined) entry.newId = m[3];
  return entry;
}

/** What `user` has written here and not saved to the server, newest first. Read-only; a
 * storage failure is logged and reads as none. */
export async function listDrafts(user: number): Promise<DraftEntry[]> {
  try {
    const all = await run<DraftEntry[]>("readonly", (s, done) => {
      const found: DraftEntry[] = [];
      const req = s.openCursor(IDBKeyRange.bound(`${user}:`, `${user}:\uffff`));
      req.onsuccess = () => {
        const c = req.result;
        if (!c) return done(found);
        const e = toEntry(user, String(c.key), c.value);
        if (e) found.push(e);
        c.continue();
      };
    });
    return all.sort((a, b) => b.savedAt - a.savedAt);
  } catch (e) {
    console.error(`admin drafts: listing user ${user} failed`, e);
    return [];
  }
}

/** Removes every draft of `user`. Sign-out (part D) calls it, so a shared browser keeps none. */
export async function clearDrafts(user: number): Promise<void> {
  try {
    await run<true>("readwrite", (s, done) => {
      const req = s.delete(IDBKeyRange.bound(`${user}:`, `${user}:\uffff`));
      req.onsuccess = () => done(true);
    });
  } catch (e) {
    console.error(`admin drafts: clearing user ${user} failed`, e);
  }
}

function isDraft(v: unknown): v is Draft {
  const d = v as Draft | null;
  return (
    typeof d?.title === "string" &&
    typeof d.body === "string" &&
    (d.base === null || typeof d.base === "number") &&
    (d.version === undefined || typeof d.version === "number") &&
    typeof d.rev === "number" &&
    typeof d.savedAt === "number"
  );
}
