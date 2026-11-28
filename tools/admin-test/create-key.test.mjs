// The create id kept in localStorage (`plumblog-create:<draft key>`, editor/sync.ts) goes with
// the draft it belongs to: when a new draft is dropped and at sign-out, but never on a storage error.
import assert from "node:assert/strict";
import { test } from "node:test";
import { Window } from "happy-dom";

const win = new Window({ url: "http://localhost/admin/" });
Object.defineProperty(globalThis, "localStorage", { value: win.localStorage, configurable: true, writable: true });

const { clearDrafts, createIdKey, dropDraft, loadDraft } = await import(new URL("../../web/admin/src/editor/drafts.ts", import.meta.url).href);

// A Map behind the slice of IndexedDB drafts.ts uses; `fail` makes the next transaction error.
const rows = new Map();
const fail = { on: false };
globalThis.IDBKeyRange = { bound: (lo, hi) => ({ lo, hi }) };
{
  const request = (fn) => {
    const r = {};
    queueMicrotask(() => {
      r.result = fn();
      r.onsuccess?.();
    });
    return r;
  };
  const store = {
    get: (k) => request(() => rows.get(k)),
    delete: (k) =>
      request(() => {
        if (typeof k === "string") rows.delete(k);
        else for (const key of [...rows.keys()]) if (key >= k.lo && key <= k.hi) rows.delete(key);
      }),
  };
  const db = {
    objectStoreNames: { contains: () => true },
    close() {},
    transaction() {
      const tx = { objectStore: () => store };
      const bad = fail.on;
      setTimeout(() => (bad ? tx.onerror?.() : tx.oncomplete?.()), 0);
      return tx;
    },
  };
  globalThis.indexedDB = { open: () => request(() => db) };
}

const draft = (rev = 1) => ({ title: "t", body: "b", base: null, rev, savedAt: 1 });
const quiet = async (f) => {
  const e = console.error;
  console.error = () => {};
  try {
    return await f();
  } finally {
    console.error = e;
  }
};
const setup = () => {
  rows.clear();
  localStorage.clear();
  fail.on = false;
  for (const k of ["1:new-aaaaaaaaaaaa", "1:new-bbbbbbbbbbbb", "2:new-cccccccccccc"]) {
    rows.set(k, draft());
    localStorage.setItem(createIdKey(k), "id-of-" + k);
  }
  localStorage.setItem("unrelated", "x");
};
const keys = () => Object.keys(Object.fromEntries([...Array(localStorage.length).keys()].map((i) => [localStorage.key(i), 1]))).sort();

test("dropping a new draft forgets its create id and no other", async () => {
  setup();
  assert.equal(await dropDraft("1:new-aaaaaaaaaaaa", 1), true);
  assert.deepEqual(keys(), ["plumblog-create:1:new-bbbbbbbbbbbb", "plumblog-create:2:new-cccccccccccc", "unrelated"]);
});

test("dropping a side copy or a saved post's draft keeps every create id", async () => {
  setup();
  for (const k of ["1:new-aaaaaaaaaaaa~tab1", "1:post-5"]) {
    rows.set(k, draft());
    localStorage.setItem(createIdKey(k), "id-of-" + k);
  }
  assert.equal(await dropDraft("1:new-aaaaaaaaaaaa~tab1", 1), true);
  assert.equal(await dropDraft("1:post-5", 1), true);
  assert.ok(keys().includes("plumblog-create:1:new-aaaaaaaaaaaa"), "the new draft's id stays with a side copy gone");
  assert.ok(keys().includes("plumblog-create:1:new-aaaaaaaaaaaa~tab1") && keys().includes("plumblog-create:1:post-5"), "only new- keys are forgotten");
});

test("a drop that removed nothing (changed revision) keeps the id", async () => {
  setup();
  assert.equal(await dropDraft("1:new-aaaaaaaaaaaa", 9), false);
  assert.equal(keys().length, 4);
});

test("a failed delete keeps the id", async () => {
  setup();
  fail.on = true;
  assert.equal(await quiet(() => dropDraft("1:new-aaaaaaaaaaaa", 1)), false);
  assert.equal(keys().length, 4);
});

test("a read error never forgets an id", async () => {
  setup();
  fail.on = true;
  assert.equal(await quiet(() => loadDraft("1:new-aaaaaaaaaaaa")), undefined);
  assert.equal(keys().length, 4);
});

test("sign-out forgets all of this user's ids and no one else's", async () => {
  setup();
  await clearDrafts(1);
  assert.deepEqual(keys(), ["plumblog-create:2:new-cccccccccccc", "unrelated"]);
});

test("a failed sign-out clear keeps the ids", async () => {
  setup();
  fail.on = true;
  await quiet(() => clearDrafts(1));
  assert.equal(keys().length, 4);
});

test("an unwritable localStorage is logged, not thrown", async () => {
  setup();
  const real = globalThis.localStorage;
  const denied = () => {
    throw new Error("denied");
  };
  globalThis.localStorage = { removeItem: denied, key: denied, get length() { return denied(); } };
  const logged = [];
  const e = console.error;
  console.error = (...a) => logged.push(a.join(" "));
  try {
    assert.equal(await dropDraft("1:new-aaaaaaaaaaaa", 1), true);
    await clearDrafts(1);
  } finally {
    console.error = e;
    globalThis.localStorage = real;
  }
  assert.equal(logged.filter((l) => l.includes("create id")).length, 2);
});
