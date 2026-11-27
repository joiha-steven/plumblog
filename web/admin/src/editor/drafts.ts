/**
 * Drafts kept in this browser (IndexedDB), so writing survives a closed tab, a crash or no
 * network (docs/ARCHITECTURE.md, Admin and editor). The server copy comes with the posts API.
 */

export interface Draft {
  title: string;
  body: string;
  /** Unix milliseconds. */
  savedAt: number;
}

const DB = "plumblog-admin";
const STORE = "drafts";

function open(): Promise<IDBDatabase> {
  return new Promise((resolve, reject) => {
    const req = indexedDB.open(DB, 1);
    req.onupgradeneeded = () => req.result.createObjectStore(STORE);
    req.onsuccess = () => resolve(req.result);
    req.onerror = () => reject(req.error ?? new Error("indexedDB.open failed"));
  });
}

function run<T>(mode: IDBTransactionMode, act: (s: IDBObjectStore) => IDBRequest<T>): Promise<T> {
  return open().then(
    (db) =>
      new Promise<T>((resolve, reject) => {
        const tx = db.transaction(STORE, mode);
        const req = act(tx.objectStore(STORE));
        tx.oncomplete = () => {
          db.close();
          resolve(req.result);
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
    const v = await run<unknown>("readonly", (s) => s.get(key));
    return isDraft(v) ? v : undefined;
  } catch (e) {
    console.error(`admin drafts: reading ${key} failed`, e);
    return undefined;
  }
}

/** Saves the draft; false (and a log line) when the browser refuses storage. */
export async function saveDraft(key: string, d: Draft): Promise<boolean> {
  try {
    await run("readwrite", (s) => s.put(d, key));
    return true;
  } catch (e) {
    console.error(`admin drafts: saving ${key} failed`, e);
    return false;
  }
}

function isDraft(v: unknown): v is Draft {
  const d = v as Draft | null;
  return typeof d?.title === "string" && typeof d.body === "string" && typeof d.savedAt === "number";
}
