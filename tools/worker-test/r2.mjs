// Local R2 and Cache API state of a test Worker (miniflare's SQLite files), for assertions
// in r2pages.test.mjs and r2cron.test.mjs.
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";

import { cacheFile, r2File } from "./harness.mjs";

export function withDb(file, fn) {
  const db = new DatabaseSync(file);
  try {
    db.exec("PRAGMA busy_timeout = 5000");
    return fn(db);
  } finally {
    db.close();
  }
}
export const r2 = (w, fn, fallback) => {
  const f = r2File(w.state);
  return f === null ? fallback : withDb(f, fn);
};
export const pageKeys = (w) => r2(w, (db) => db.prepare("SELECT key FROM _mf_objects WHERE key LIKE 'pages/%' ORDER BY key").all().map((r) => r.key), []);
export const r2Row = (w, key) => r2(w, (db) => db.prepare("SELECT * FROM _mf_objects WHERE key = ?").get(key), undefined);
/** The object's bytes, from the bucket's blob directory (the one dir next to miniflare's own). */
export function r2Body(w, row) {
  const root = join(w.state, "v3", "r2");
  const [bucket, ...more] = readdirSync(root).filter((d) => !d.startsWith("miniflare-"));
  assert.equal(more.length, 0, `one bucket dir under ${root}`);
  return readFileSync(join(root, bucket, "blobs", row.blob_id), "utf8");
}
export const renameR2 = (w, from, to) => r2(w, (db) => db.prepare("UPDATE _mf_objects SET key = ? WHERE key = ?").run(to, from));

/** Runs `fn(db)` on this Worker's Cache API database. */
export function withCache(w, fn) {
  const f = cacheFile(w.state);
  assert.ok(f, `no Cache API database under ${w.state}`);
  return withDb(f, fn);
}
/** Empties the Cache API, so the next request misses it as a data centre that never saw the page. */
export const emptyCache = (w) => withCache(w, (db) => db.exec("DELETE FROM _mf_entries"));
export const cacheKeys = (w) => withCache(w, (db) => db.prepare("SELECT key FROM _mf_entries").all().map((r) => r.key));

export async function versionId(w) {
  const res = await fetch(`${w.url}/healthz`);
  await res.text();
  const id = res.headers.get("x-plumblog-version");
  assert.match(id ?? "", /^[0-9a-f-]{8,}$/i);
  return id;
}
/** Empty objects (no blob) uploaded at `uploaded` ms, as R2 would list them. */
export const seed = (w, keys, uploaded = Date.now()) =>
  r2(w, (db) => {
    const st = db.prepare(
      "INSERT INTO _mf_objects (key, blob_id, version, size, etag, uploaded, checksums, http_metadata, custom_metadata) VALUES (?, NULL, ?, 0, ?, ?, '{}', '{}', '{}')",
    );
    db.exec("BEGIN");
    for (const k of [keys].flat()) st.run(k, `seed-${k}`, `etag-${k}`, uploaded);
    db.exec("COMMIT");
  });
export const contentVersion = (w) => Number(w.query("SELECT content_version FROM site WHERE id = 1")[0].content_version);
