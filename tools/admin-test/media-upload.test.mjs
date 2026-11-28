// The media screen's upload logic (web/admin/src/media/logic.ts): what is refused before sending,
// the hex digest the API's `sha256` takes, who sees the upload controls, how a failure is named,
// and the queue that keeps at most two files in flight.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { test } from "node:test";
import { build } from "esbuild";

const root = new URL("../../", import.meta.url);
const out = await build({ entryPoints: [new URL("web/admin/src/media/logic.ts", root).pathname], bundle: true, write: false, format: "esm", logLevel: "silent" });
const m = await import(`data:text/javascript;base64,${Buffer.from(out.outputFiles[0].text).toString("base64")}`);

test("only the five picture types the API takes are sent, up to 20 MiB", () => {
  for (const type of ["image/avif", "image/webp", "image/jpeg", "image/png", "image/gif"]) assert.equal(m.refuseFile({ type, size: 1 }), null, type);
  for (const type of ["image/svg+xml", "image/bmp", "application/pdf", "text/plain", ""]) assert.equal(m.refuseFile({ type, size: 1 }), "type", type);
  assert.equal(m.refuseFile({ type: "image/png", size: 20 * 1024 * 1024 }), null, "exactly 20 MiB is allowed");
  assert.equal(m.refuseFile({ type: "image/png", size: 20 * 1024 * 1024 + 1 }), "size");
  // A wrong type is reported before a wrong size.
  assert.equal(m.refuseFile({ type: "text/plain", size: 99 * 1024 * 1024 }), "type");
});

test("the digest is lowercase hex, two characters per byte, as the API's sha256 field wants", async () => {
  assert.equal(m.toHex(new Uint8Array([0, 1, 15, 16, 255]).buffer), "00010f10ff");
  const bytes = Buffer.from("plumblog picture");
  const want = createHash("sha256").update(bytes).digest("hex");
  assert.equal(await m.sha256Of(new Blob([bytes])), want);
  assert.match(want, /^[0-9a-f]{64}$/);
});

test("upload controls are offered to those with media.upload, so owners and writers", () => {
  assert.equal(m.canUpload({ role: "owner" }), true);
  assert.equal(m.canUpload({ role: "writer" }), true);
  assert.equal(m.canUpload({ role: "reader" }), false);
});

test("a failed upload is named by what the writer can do about it", () => {
  assert.equal(m.faultOf({ code: "network", status: 0 }), "offline");
  assert.equal(m.faultOf({ code: "api.body_too_large", status: 413 }), "too_big");
  assert.equal(m.faultOf({ code: "auth.forbidden", status: 403 }), "forbidden");
  assert.equal(m.faultOf({ code: "input.invalid", status: 0 }), "unreadable", "the client could not read it");
  assert.equal(m.faultOf({ code: "input.invalid", status: 400 }), "refused", "the server refused it");
  assert.equal(m.faultOf({ code: "internal", status: 500 }), "failed");
});

test("the queue runs at most N jobs at once, in order, and goes on after a failing job", async () => {
  const pool = new m.Pool(2);
  assert.equal(m.UPLOAD_PARALLEL, 2);
  let running = 0;
  let peak = 0;
  const started = [];
  const gates = [];
  const jobs = Array.from({ length: 5 }, (_, i) => () =>
    new Promise((resolve, reject) => {
      running++;
      peak = Math.max(peak, running);
      started.push(i);
      gates.push(() => {
        running--;
        // The third job fails; the queue must not stall on it.
        i === 2 ? reject(new Error("boom")) : resolve();
      });
    }),
  );
  const logged = console.error;
  console.error = () => undefined;
  try {
    for (const j of jobs) pool.add(j);
    assert.deepEqual(started, [0, 1], "two start at once");
    assert.equal(pool.idle, false);
    gates.shift()(); // job 0 ends
    await new Promise((r) => setImmediate(r));
    assert.deepEqual(started, [0, 1, 2]);
    gates.shift()(); // job 1
    await new Promise((r) => setImmediate(r));
    gates.shift()(); // job 2 fails
    await new Promise((r) => setImmediate(r));
    assert.deepEqual(started, [0, 1, 2, 3, 4]);
    while (gates.length) {
      gates.shift()();
      await new Promise((r) => setImmediate(r));
    }
  } finally {
    console.error = logged;
  }
  assert.equal(peak, 2);
  assert.equal(pool.idle, true);
});
