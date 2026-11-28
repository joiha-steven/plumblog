// Runs in a Web Worker, off the main thread: decodes the picture, makes each width and writes
// AVIF and WebP. Loaded by encode/index.ts on the first upload only (a separate asset, never
// part of the admin shell). Browsers' own canvas encoders are used where they really work;
// AVIF otherwise comes from libavif compiled to wasm (@jsquash/avif, see tools/package.json).
import avifFactory from "@jsquash/avif/codec/enc/avif_enc.js";
import type { AVIFModule } from "@jsquash/avif/codec/enc/avif_enc.js";
import { HEADER_BYTES, heightFor, imageSize, pickWidths, refuse, sniff, type Mime } from "./plan.ts";
import type { FromWorker, Job, Skipped, Variant } from "./protocol.ts";

const scope = self as unknown as {
  onmessage: ((e: MessageEvent<Job>) => void) | null;
  postMessage(m: FromWorker, transfer?: Transferable[]): void;
};
const say = (m: FromWorker, transfer?: Transferable[]) => scope.postMessage(m, transfer);

const MIME: Record<"avif" | "webp", Mime> = { avif: "image/avif", webp: "image/webp" };

// One decision per worker: does canvas really write this type? Browsers answer a type they
// cannot write with a PNG, so the bytes are sniffed, not the blob's `type`.
const nativeProbe = new Map<Mime, Promise<boolean>>();
function nativeWrites(mime: Mime): Promise<boolean> {
  let p = nativeProbe.get(mime);
  if (!p) {
    p = (async () => {
      try {
        const c = new OffscreenCanvas(8, 8);
        c.getContext("2d")?.fillRect(0, 0, 4, 4);
        const blob = await c.convertToBlob({ type: mime, quality: 0.5 });
        return sniff(new Uint8Array(await blob.arrayBuffer())) === mime;
      } catch {
        return false;
      }
    })();
    nativeProbe.set(mime, p);
  }
  return p;
}

let avifModule: Promise<AVIFModule> | undefined;
function loadAvif(wasmUrl: string): Promise<AVIFModule> {
  // Compiled from bytes, not streamed: whatever content type the host gives .wasm is fine.
  avifModule ??= (async () => {
    const res = await fetch(wasmUrl);
    if (!res.ok) throw new Error(`${wasmUrl}: HTTP ${res.status}`);
    const module = await WebAssembly.compile(await res.arrayBuffer());
    return avifFactory({
      noInitialRun: true,
      instantiateWasm: (imports: WebAssembly.Imports, done: (i: WebAssembly.Instance) => void) => {
        const instance = new WebAssembly.Instance(module, imports);
        done(instance);
        return instance.exports;
      },
    });
  })();
  // A failed load is not cached: the next job in this worker fetches again (a flaky network is the likely cause).
  avifModule.catch(() => (avifModule = undefined));
  return avifModule;
}

const hex = (b: ArrayBuffer) => [...new Uint8Array(b)].map((x) => x.toString(16).padStart(2, "0")).join("");

async function variant(mime: Mime, width: number, height: number, bytes: Uint8Array): Promise<Variant> {
  return { mime, width, height, bytes, sha256: hex(await crypto.subtle.digest("SHA-256", bytes as BufferSource)) };
}

async function run(job: Job): Promise<void> {
  // The size the header declares, checked before any decoder allocates for it: a small file
  // can declare a picture of tens of gigapixels.
  const head = imageSize(new Uint8Array(await job.file.slice(0, HEADER_BYTES).arrayBuffer()));
  const declared = head && refuse(head.width, head.height, job.maxPixels);
  if (head && declared) {
    say({ type: "fatal", skipped: { reason: declared, detail: `${head.width}x${head.height} declared` } });
    return;
  }
  let bitmap: ImageBitmap;
  try {
    bitmap = await createImageBitmap(job.file, { imageOrientation: "from-image" });
  } catch (err) {
    say({ type: "fatal", skipped: { reason: "decode_failed", detail: String(err) } });
    return;
  }
  const refused = refuse(bitmap.width, bitmap.height, job.maxPixels);
  if (refused) {
    say({ type: "fatal", skipped: { reason: refused, detail: `${bitmap.width}x${bitmap.height}` } });
    bitmap.close();
    return;
  }
  say({ type: "source", width: bitmap.width, height: bitmap.height });

  const widths = pickWidths(bitmap.width, job.widths);
  const total = widths.length * job.formats.length;
  let done = 0;
  const skip = (s: Skipped) => say({ type: "skipped", skipped: s });
  const useNative = new Map<Mime, boolean>();
  for (const f of job.formats) {
    const mime = MIME[f];
    const native = await nativeWrites(mime);
    useNative.set(mime, native);
    if (!native && f === "webp") {
      skip({ reason: "native_unsupported", mime });
      done += widths.length;
    }
  }
  let avifDown = false;

  for (const width of widths) {
    const height = heightFor(bitmap.width, bitmap.height, width);
    const canvas = new OffscreenCanvas(width, height);
    const ctx = canvas.getContext("2d", { willReadFrequently: true });
    if (!ctx) {
      // Every format still to make at this width is skipped, and counted, so progress stays whole.
      for (const f of job.formats) {
        const mime = MIME[f];
        if ((f === "webp" && !useNative.get(mime)) || (f === "avif" && avifDown)) continue;
        skip({ reason: "encode_failed", mime, width, detail: "no 2d context" });
        done++;
      }
      continue;
    }
    ctx.imageSmoothingQuality = "high";
    ctx.drawImage(bitmap, 0, 0, width, height);
    for (const f of job.formats) {
      const mime = MIME[f];
      if (f === "webp" && !useNative.get(mime)) continue;
      if (f === "avif" && avifDown) continue;
      say({ type: "progress", progress: { done, total, mime, width } });
      try {
        let bytes: Uint8Array;
        if (useNative.get(mime)) {
          const q = (f === "avif" ? job.avifQuality : job.webpQuality) / 100;
          bytes = new Uint8Array(await (await canvas.convertToBlob({ type: mime, quality: q })).arrayBuffer());
        } else {
          let mod: AVIFModule;
          try {
            mod = await loadAvif(job.wasmUrl);
          } catch (err) {
            avifDown = true;
            skip({ reason: "encoder_unavailable", mime, detail: String(err) });
            done += widths.length - widths.indexOf(width);
            continue;
          }
          const rgba = ctx.getImageData(0, 0, width, height).data;
          const out = mod.encode(rgba, width, height, {
            quality: job.avifQuality,
            qualityAlpha: -1,
            denoiseLevel: 0,
            tileRowsLog2: 0,
            tileColsLog2: 0,
            speed: job.avifSpeed,
            subsample: 1,
            chromaDeltaQ: false,
            sharpness: 0,
            enableSharpYUV: false,
            tune: 0,
            bitDepth: 8,
          });
          if (!out) throw new Error("the encoder returned nothing");
          bytes = out.slice(); // a copy: `out` views the wasm heap, which a later encode reuses
        }
        if (sniff(bytes) !== mime) throw new Error(`not ${mime}`);
        const v = await variant(mime, width, height, bytes);
        say({ type: "variant", variant: v }, [bytes.buffer]);
      } catch (err) {
        skip({ reason: "encode_failed", mime, width, detail: String(err) });
      }
      done++;
    }
  }
  bitmap.close();
  say({ type: "progress", progress: { done: total, total, mime: MIME[job.formats[0] ?? "avif"], width: widths.at(-1) ?? 0 } });
  say({ type: "done" });
}

scope.onmessage = (e) => {
  run(e.data).catch((err) => {
    console.error("encode worker failed", err);
    say({ type: "fatal", skipped: { reason: "encode_failed", detail: String(err) } });
  });
};
