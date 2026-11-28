// The image encoder (web/admin/src/encode) in the system Chrome, served by the real Worker so
// the shell's strict CSP applies: real JPEGs go in, AVIF and WebP of the right size come out;
// abort, timeout, a failing encoder load and an unreadable file each give a plain answer; and
// the page load never fetches the encoder.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync, readdirSync } from "node:fs";
import { after, before, test } from "node:test";
import { build } from "esbuild";
import { chromium } from "playwright-core";
import { startWorker } from "./harness.mjs";

const root = new URL("../../", import.meta.url);
const encDir = new URL("target/admin/encode/", root);
const [workerFile] = readdirSync(encDir).filter((f) => f.startsWith("worker-"));
const [wasmFile] = readdirSync(encDir).filter((f) => f.endsWith(".wasm"));

// encode/index.ts as a stand-alone module, told the URLs the build made (as scripts/admin-build does).
const built = await build({
  entryPoints: [new URL("web/admin/src/encode/index.ts", root).pathname],
  bundle: true,
  write: false,
  format: "esm",
  logLevel: "silent",
  define: { ENCODER_WORKER_URL: `"/admin/encode/${workerFile}"`, ENCODER_WASM_URL: `"/admin/encode/${wasmFile}"` },
});
const moduleSource = built.outputFiles[0].text;

let worker;
let browser;
before(async () => {
  worker = await startWorker();
  browser = await chromium.launch({ channel: "chrome", headless: true });
});
after(async () => {
  await browser?.close();
  await worker?.stop();
});

// A page of the real shell with the admin script blanked (it would send us to sign-in), the
// encoder module served beside it, and helpers that make real JPEGs with a canvas.
async function openPage() {
  const page = await browser.newPage({ locale: "en-US" });
  const problems = [];
  const requests = [];
  page.on("pageerror", (e) => problems.push(`page error: ${e}`));
  page.on("console", (m) => {
    if (/Content Security Policy|Refused to/.test(m.text())) problems.push(`csp: ${m.text()}`);
  });
  page.on("request", (r) => requests.push(new URL(r.url()).pathname));
  await page.route("**/admin/app.js", (r) => r.fulfill({ contentType: "text/javascript", body: "" }));
  await page.route("**/admin/__encode.js", (r) => r.fulfill({ contentType: "text/javascript", body: moduleSource }));
  await page.goto(`${worker.url}/admin/`);
  await page.evaluate(() => {
    // A photo-like picture (soft shapes over a gradient) as a real JPEG; `orientation` adds an
    // EXIF block so the browser must turn it.
    window.jpeg = async (w, h, orientation = 1) => {
      const c = new OffscreenCanvas(w, h);
      const g = c.getContext("2d");
      const grad = g.createLinearGradient(0, 0, w, h);
      grad.addColorStop(0, "#3a6ea5");
      grad.addColorStop(1, "#e8c77a");
      g.fillStyle = grad;
      g.fillRect(0, 0, w, h);
      for (let i = 0; i < 40; i++) {
        g.fillStyle = `hsla(${(i * 47) % 360},60%,50%,0.5)`;
        g.beginPath();
        g.arc(((i * 7919) % w) | 0, ((i * 104729) % h) | 0, 20 + ((i * 31) % 120), 0, 7);
        g.fill();
      }
      const bytes = new Uint8Array(await (await c.convertToBlob({ type: "image/jpeg", quality: 0.9 })).arrayBuffer());
      if (orientation === 1) return new Blob([bytes], { type: "image/jpeg" });
      const exif = [0x45, 0x78, 0x69, 0x66, 0, 0, 0x4d, 0x4d, 0, 0x2a, 0, 0, 0, 8, 0, 1, 0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, orientation, 0, 0, 0, 0, 0, 0];
      const head = [0xff, 0xe1, 0, exif.length + 2, ...exif];
      return new Blob([bytes.subarray(0, 2), new Uint8Array(head), bytes.subarray(2)], { type: "image/jpeg" });
    };
    // makeVariants, with each variant's bytes as base64 so Node can look at them.
    window.run = async (blob, opts = {}) => {
      const { makeVariants } = await import("/admin/__encode.js");
      const progress = [];
      const r = await makeVariants(blob, { ...opts, onProgress: (p) => (progress.push(p), opts.onProgress?.(p)) });
      const b64 = (u8) => {
        let s = "";
        for (let i = 0; i < u8.length; i += 8192) s += String.fromCharCode(...u8.subarray(i, i + 8192));
        return btoa(s);
      };
      return { ...r, progress, variants: r.variants.map((v) => ({ ...v, bytes: b64(v.bytes) })) };
    };
  });
  return { page, problems, requests };
}

const u32 = (b, o) => b.readUInt32BE(o);
/** Width and height a file declares, read from its own header, or throws. */
function declaredSize(mime, b) {
  if (mime === "image/avif") {
    assert.equal(b.toString("latin1", 4, 12), "ftypavif");
    const at = b.indexOf("ispe", 0, "latin1");
    assert.ok(at > 0, "an ispe box");
    assert.ok(b.indexOf("auxC", 0, "latin1") < 0, "no alpha plane for a picture with none");
    return { width: u32(b, at + 8), height: u32(b, at + 12) };
  }
  assert.equal(b.toString("latin1", 0, 4), "RIFF");
  assert.equal(b.toString("latin1", 8, 12), "WEBP");
  const kind = b.toString("latin1", 12, 16);
  if (kind === "VP8 ") return { width: b.readUInt16LE(26) & 0x3fff, height: b.readUInt16LE(28) & 0x3fff };
  if (kind === "VP8X") return { width: 1 + b.readUIntLE(24, 3), height: 1 + b.readUIntLE(27, 3) };
  assert.equal(kind, "VP8L");
  const bits = b.readUInt32LE(21);
  return { width: 1 + (bits & 0x3fff), height: 1 + ((bits >> 14) & 0x3fff) };
}

const summary = (r) => r.variants.map((v) => `${v.mime.slice(6)} ${v.width}x${v.height}`).sort();

test("a wide JPEG gets AVIF and WebP at all four widths, each as real as it says", async () => {
  const { page, problems } = await openPage();
  try {
    const r = await page.evaluate(async () => window.run(await window.jpeg(2500, 1000)));
    assert.deepEqual(r.skipped, []);
    assert.deepEqual(r.source, { width: 2500, height: 1000 });
    const expected = [];
    for (const [w, h] of [[480, 192], [960, 384], [1600, 640], [2400, 960]]) for (const m of ["avif", "webp"]) expected.push(`${m} ${w}x${h}`);
    assert.deepEqual(summary(r), expected.sort());
    for (const v of r.variants) {
      const bytes = Buffer.from(v.bytes, "base64");
      assert.deepEqual(declaredSize(v.mime, bytes), { width: v.width, height: v.height }, `${v.mime} ${v.width}`);
      assert.equal(v.sha256, createHash("sha256").update(bytes).digest("hex"), `sha256 of ${v.mime} ${v.width}`);
      assert.ok(bytes.length > 100 && bytes.length < 400_000, `${v.mime} ${v.width}: ${bytes.length} bytes`);
    }
    // Progress reaches the end and never goes back.
    const done = r.progress.map((p) => p.done);
    assert.deepEqual(done, [...done].sort((a, b) => a - b));
    assert.equal(r.progress.at(-1).done, r.progress.at(-1).total);
    assert.deepEqual(problems, []); // wasm compiled under the shell's CSP: no violation
  } finally {
    await page.close();
  }
});

test("a smaller picture gets fewer sizes, never upscaled; a tiny one its own width", async () => {
  const { page } = await openPage();
  try {
    const mid = await page.evaluate(async () => window.run(await window.jpeg(1000, 500)));
    assert.deepEqual(summary(mid), ["avif 480x240", "avif 960x480", "webp 480x240", "webp 960x480"]);
    const exact = await page.evaluate(async () => window.run(await window.jpeg(480, 300)));
    assert.deepEqual(summary(exact), ["avif 480x300", "webp 480x300"]);
    const tiny = await page.evaluate(async () => window.run(await window.jpeg(300, 200)));
    assert.deepEqual(summary(tiny), ["avif 300x200", "webp 300x200"]);
    for (const v of tiny.variants) assert.deepEqual(declaredSize(v.mime, Buffer.from(v.bytes, "base64")), { width: 300, height: 200 });
  } finally {
    await page.close();
  }
});

test("EXIF orientation is applied: a picture turned a quarter comes out upright", async () => {
  const { page } = await openPage();
  try {
    const r = await page.evaluate(async () => window.run(await window.jpeg(1000, 600, 6)));
    assert.deepEqual(r.source, { width: 600, height: 1000 });
    assert.deepEqual(summary(r), ["avif 480x800", "webp 480x800"]);
    for (const v of r.variants) assert.deepEqual(declaredSize(v.mime, Buffer.from(v.bytes, "base64")), { width: 480, height: 800 });
  } finally {
    await page.close();
  }
});

test("an abort stops the run and rejects with AbortError, before or during the work", async () => {
  const { page } = await openPage();
  try {
    const out = await page.evaluate(async () => {
      const blob = await window.jpeg(2500, 1000);
      const name = async (p) => p.then(() => "resolved", (e) => e.name);
      const before = new AbortController();
      before.abort();
      const during = new AbortController();
      const running = name(window.run(blob, { signal: during.signal }));
      setTimeout(() => during.abort(), 30);
      const t0 = performance.now();
      const result = await running;
      // The next run still works: nothing is left behind.
      const again = await window.run(await window.jpeg(500, 300));
      return { early: await name(window.run(blob, { signal: before.signal })), during: result, stopped: performance.now() - t0, again: again.variants.length };
    });
    assert.equal(out.early, "AbortError");
    assert.equal(out.during, "AbortError");
    assert.ok(out.stopped < 5000, `stopped in ${out.stopped} ms`);
    assert.equal(out.again, 2);
  } finally {
    await page.close();
  }
});

test("an encoder that fails to load is skipped with a reason; WebP still comes", async () => {
  const { page } = await openPage();
  try {
    await page.route("**/admin/encode/*.wasm", (r) => r.abort());
    const r = await page.evaluate(async () => window.run(await window.jpeg(1000, 500)));
    assert.deepEqual(summary(r), ["webp 480x240", "webp 960x480"]);
    assert.equal(r.skipped.length, 1);
    assert.equal(r.skipped[0].reason, "encoder_unavailable");
    assert.equal(r.skipped[0].mime, "image/avif");
    // AVIF alone, with the encoder down: nothing to upload but the original.
    const only = await page.evaluate(async () => window.run(await window.jpeg(1000, 500), { formats: ["avif"] }));
    assert.deepEqual(only.variants, []);
    assert.equal(only.skipped[0].reason, "encoder_unavailable");
    // Back up, the next upload works again (a failed load is not kept).
    await page.unroute("**/admin/encode/*.wasm");
    const ok = await page.evaluate(async () => window.run(await window.jpeg(1000, 500)));
    assert.equal(ok.variants.length, 4);
    assert.deepEqual(ok.skipped, []);
  } finally {
    await page.close();
  }
});

test("an unreadable file, a huge picture and a timeout all fall back to no variants", async () => {
  const { page } = await openPage();
  try {
    const out = await page.evaluate(async () => {
      const junk = await window.run(new Blob([new Uint8Array(500).fill(7)], { type: "image/jpeg" }));
      const huge = await window.run(await window.jpeg(1000, 500), { maxPixels: 1000 });
      const late = await window.run(await window.jpeg(2500, 1000), { timeoutMs: 1 });
      return { junk, huge, late };
    });
    for (const [name, r, reason] of [["junk", out.junk, "decode_failed"], ["huge", out.huge, "too_large"], ["late", out.late, "timeout"]]) {
      assert.deepEqual(r.variants, [], name);
      assert.equal(r.skipped.length, 1, name);
      assert.equal(r.skipped[0].reason, reason, name);
    }
  } finally {
    await page.close();
  }
});

test("a worker script that fails to load gives worker_failed, and a later call starts a new worker", async () => {
  const { page } = await openPage();
  try {
    await page.route("**/admin/encode/worker-*.js", (r) => r.abort());
    const dead = await page.evaluate(async () => window.run(await window.jpeg(500, 300)));
    assert.deepEqual(dead.variants, []);
    assert.equal(dead.skipped[0].reason, "worker_failed");
    await page.unroute("**/admin/encode/worker-*.js");
    const ok = await page.evaluate(async () => window.run(await window.jpeg(500, 300)));
    assert.equal(ok.variants.length, 2);
  } finally {
    await page.close();
  }
});

test("a small file that declares 40000x40000 pixels is refused from its header, fast, without decoding", async () => {
  const { page } = await openPage();
  try {
    const out = await page.evaluate(async () => {
      const be32 = (n) => [(n >>> 24) & 255, (n >> 16) & 255, (n >> 8) & 255, n & 255];
      const png = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 13, 10, 26, 10, ...be32(13), 0x49, 0x48, 0x44, 0x52, ...be32(40000), ...be32(40000), 8, 0, 0, 0, 0, 0, 0, 0, 0]);
      const t0 = performance.now();
      const r = await window.run(new Blob([png], { type: "image/png" }));
      return { r, ms: performance.now() - t0 };
    });
    assert.deepEqual(out.r.variants, []);
    assert.equal(out.r.skipped.length, 1);
    assert.equal(out.r.skipped[0].reason, "too_large");
    assert.match(out.r.skipped[0].detail, /40000x40000 declared/);
    assert.ok(out.ms < 2000, `${out.ms} ms`);
  } finally {
    await page.close();
  }
});

test("calls queue: three at once all finish, one at a time, on one worker that fetches the wasm once", async () => {
  const { page, requests } = await openPage();
  try {
    const spans = await page.evaluate(async () => {
      const blobs = [await window.jpeg(1000, 500), await window.jpeg(1000, 600), await window.jpeg(1000, 700)];
      const run = async (blob) => {
        const span = { first: undefined, end: 0, n: 0 };
        const r = await window.run(blob, { onProgress: () => (span.first ??= performance.now()) });
        span.end = performance.now();
        span.n = r.variants.length;
        return span;
      };
      return Promise.all(blobs.map(run));
    });
    assert.deepEqual(spans.map((s) => s.n), [4, 4, 4]);
    // Which call goes first is the page's business; no two may overlap.
    spans.sort((a, b) => a.first - b.first);
    for (let i = 1; i < spans.length; i++) assert.ok(spans[i].first >= spans[i - 1].end, `job ${i} began before job ${i - 1} ended: ${JSON.stringify(spans)}`);
    assert.equal(requests.filter((p) => p.endsWith(".wasm")).length, 1, "the wasm is fetched once");
    assert.equal(requests.filter((p) => p.includes("/admin/encode/worker-")).length, 1, "one worker script");
  } finally {
    await page.close();
  }
});

test("aborting a call that is still queued removes it; the running one and the rest finish", async () => {
  const { page } = await openPage();
  try {
    const out = await page.evaluate(async () => {
      const big = await window.jpeg(1000, 500);
      const small = await window.jpeg(500, 300);
      const ac = new AbortController();
      const queuedProgress = [];
      const first = window.run(big);
      const queued = window.run(small, { signal: ac.signal, onProgress: (p) => queuedProgress.push(p) }).then(() => "resolved", (e) => e.name);
      const third = window.run(small);
      ac.abort();
      return { queued: await queued, first: (await first).variants.length, third: (await third).variants.length, queuedProgress: queuedProgress.length };
    });
    assert.equal(out.queued, "AbortError");
    assert.equal(out.queuedProgress, 0, "the removed job never ran");
    assert.equal(out.first, 4);
    assert.equal(out.third, 2);
  } finally {
    await page.close();
  }
});

test("opening the admin never fetches the encoder, and no bundle of the admin script contains it", async () => {
  const page = await browser.newPage({ locale: "en-US" });
  const requests = [];
  page.on("request", (r) => requests.push(new URL(r.url()).pathname));
  try {
    await page.goto(`${worker.url}/admin/`, { waitUntil: "networkidle" });
    assert.ok(requests.some((p) => p === "/admin/app.js"), "the admin script loaded");
    assert.deepEqual(requests.filter((p) => p.startsWith("/admin/encode/")), []);
  } finally {
    await page.close();
  }
  const html = await (await fetch(`${worker.url}/admin/`)).text();
  assert.ok(!html.includes("/admin/encode/"), "the shell HTML names no encoder file");

  // Everything the encoder worker bundles is absent from the admin script's bundle (shell and chunks).
  const enc = JSON.parse(readFileSync(new URL("target/admin.encode.meta.json", root), "utf8")).inputs;
  const main = JSON.parse(readFileSync(new URL("target/admin.meta.json", root), "utf8"));
  const codec = Object.keys(enc).filter((f) => /node_modules\/@jsquash\//.test(f));
  assert.ok(codec.length > 0 && Object.keys(enc).some((f) => /encode\/worker\.ts$/.test(f)), "the worker bundle holds the codec");
  for (const f of Object.keys(enc)) assert.ok(!(f in main.inputs), `${f} is in the admin script bundle`);
  for (const [out, info] of Object.entries(main.outputs)) {
    assert.ok(!Object.keys(info.inputs).some((f) => /node_modules\/@jsquash\/|encode\/worker\.ts$/.test(f)), `${out} holds encoder code`);
  }
  // Nor does any script the admin loads name the wasm file (until the upload code imports the
  // encode module, which then carries only the URLs, never the codec).
  for (const out of Object.keys(main.outputs)) {
    if (!out.endsWith(".js")) continue;
    assert.ok(!readFileSync(new URL(out, root), "utf8").includes(wasmFile), `${out} names the wasm`);
  }
});
