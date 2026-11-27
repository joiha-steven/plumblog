/**
 * Drafts kept in this browser (IndexedDB), so writing survives a closed tab, a crash or no
 * network (docs/ARCHITECTURE.md, Admin and editor). The server copy comes with the posts API.
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
  /** Raised by every save; a save expecting another value lost a race with another tab. */
  rev: number;
  /** Unix milliseconds, for display only: never compared with a server time. */
  savedAt: number;
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

/** Removes a draft once its text is safe elsewhere (the server, or the writer chose another). */
export async function dropDraft(key: string): Promise<void> {
  try {
    await run<true>("readwrite", (s, done) => {
      s.delete(key).onsuccess = () => done(true);
    });
  } catch (e) {
    console.error(`admin drafts: removing ${key} failed`, e);
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
    typeof d.rev === "number" &&
    typeof d.savedAt === "number"
  );
}
