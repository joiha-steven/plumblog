// The media screen's pure parts (web/admin/src/media/logic.ts): which filter the address
// names, the request for a page, merging pages, who is offered "Delete", and how sizes read.
import assert from "node:assert/strict";
import { test } from "node:test";
import { build } from "esbuild";

const root = new URL("../../", import.meta.url);
const out = await build({ entryPoints: [new URL("web/admin/src/media/logic.ts", root).pathname], bundle: true, write: false, format: "esm", logLevel: "silent" });
const m = await import(`data:text/javascript;base64,${Buffer.from(out.outputFiles[0].text).toString("base64")}`);

const item = (id, uploaded_by = 1) => ({ id, src: `/media/${id}/p.png`, mime: "image/png", bytes: 1, width: 1, height: 1, alt: "", uploaded_by, created_at: id });

test("the filter comes from ?show=, anything else is All", () => {
  assert.equal(m.filterOf(""), "all");
  assert.equal(m.filterOf("?show=missing_alt"), "missing_alt");
  assert.equal(m.filterOf("?show=unused&x=1"), "unused");
  assert.equal(m.filterOf("?show=trashed"), "all");
});

test("a page request carries the filter flag, the limit and the cursor only when there is one", () => {
  assert.deepEqual(m.queryFor("all", null), { limit: m.PAGE });
  assert.deepEqual(m.queryFor("missing_alt", null), { limit: m.PAGE, missing_alt: true });
  assert.deepEqual(m.queryFor("unused", "abc"), { limit: m.PAGE, unused: true, cursor: "abc" });
  assert.ok(m.PAGE <= 100, "the API caps a page at 100");
});

test("a next page is appended and an item that slid across the page edge is not shown twice", () => {
  const first = [item(5), item(4)];
  const merged = m.mergePage(first, { media: [item(4), item(3)], next_cursor: null });
  assert.deepEqual(merged.map((x) => x.id), [5, 4, 3]);
  assert.equal(first.length, 2, "the old list is not changed");
});

test("Delete is offered to the uploader and to the owner, not to another writer or when the uploader is gone", () => {
  assert.equal(m.canDelete(item(1, 7), { id: 7, role: "writer" }), true);
  assert.equal(m.canDelete(item(1, 7), { id: 8, role: "writer" }), false);
  assert.equal(m.canDelete(item(1, 7), { id: 8, role: "owner" }), true);
  assert.equal(m.canDelete(item(1, null), { id: 8, role: "writer" }), false);
  assert.equal(m.canDelete(item(1, null), { id: 8, role: "owner" }), true);
});

test("names and types read plainly", () => {
  assert.equal(m.nameOf("/media/3/heron.avif"), "heron.avif");
  assert.equal(m.nameOf("/media/3/%E1%BA%A3nh.png"), "\u1ea3nh.png");
  assert.equal(m.nameOf("/media/3/100%.png"), "100%.png", "a bad escape is shown as it is");
  assert.equal(m.kindOf("image/avif"), "AVIF");
  assert.equal(m.kindOf("image/svg+xml"), "SVG");
  assert.equal(m.dimensions({ width: 1200, height: 800 }), "1200\u00d7800");
});

test("sizes use 1024 steps and the page language", () => {
  assert.equal(m.formatBytes(0, "en"), "0 byte");
  assert.equal(m.formatBytes(512, "en"), "512 byte");
  assert.equal(m.formatBytes(1536, "en"), "1.5 kB");
  assert.equal(m.formatBytes(24_100, "en"), "23.5 kB");
  assert.equal(m.formatBytes(5 * 1024 * 1024, "en"), "5 MB");
  assert.equal(m.formatBytes(1536, "vi"), "1,5 kB");
});
