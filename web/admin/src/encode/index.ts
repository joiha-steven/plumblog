// Makes the AVIF and WebP sizes of a picture in the browser, for upload (docs/ARCHITECTURE.md,
// Images). The work runs in a Web Worker; this module is the small main-thread side, and the
// worker and the AVIF wasm are separate files fetched only when makeVariants first runs.
//
//   const r = await makeVariants(file, { signal, onProgress });
//   if (r.variants.length === 0) -> upload the original alone and tell the writer (r.skipped[0].reason)
//
// Contract: a run either yields `variants` (possibly fewer than asked, with `skipped` saying
// what is missing and why) or, when it cannot be trusted at all (unreadable picture, too many
// pixels, timeout, dead worker), `variants` is empty and `skipped` holds the one reason. It
// never throws except `AbortError` when the caller's signal fires.
//
// Calls are queued and run one at a time in one shared worker, so the wasm compiles once and a
// burst of uploads never encodes in parallel. The worker is ended only by an abort, a timeout
// or a crash; the next call starts a new one.
import { TIMEOUT_MS, MAX_PIXELS, WIDTHS, deadline } from "./plan.ts";
import type { FromWorker, Job, Progress, Skipped, Variant } from "./protocol.ts";

export type { Progress, Skipped, Variant } from "./protocol.ts";
export { WIDTHS } from "./plan.ts";
export type { Mime, SkipReason } from "./plan.ts";

/** Set by scripts/admin-build (content-hashed, so cached for good). */
declare const ENCODER_WORKER_URL: string;
declare const ENCODER_WASM_URL: string;

export interface Options {
  widths?: readonly number[];
  formats?: readonly ("avif" | "webp")[];
  /** 0-100. Default 50. */
  avifQuality?: number;
  /** libaom speed 0-10: higher is faster and larger. Default 8. */
  avifSpeed?: number;
  /** 0-100. Default 80. */
  webpQuality?: number;
  timeoutMs?: number;
  maxPixels?: number;
  signal?: AbortSignal;
  onProgress?: (p: Progress) => void;
}

export interface Result {
  variants: Variant[];
  skipped: Skipped[];
  /** The picture's size as displayed (EXIF turn applied); absent if it could not be read. */
  source?: { width: number; height: number };
}

const abortError = () => new DOMException("The picture was not encoded: aborted.", "AbortError");

interface Pending {
  file: Blob;
  opts: Options;
  resolve: (r: Result) => void;
  reject: (e: unknown) => void;
  /** Removes the queue-time abort listener. */
  detach: () => void;
}

const queue: Pending[] = [];
let running = false;
let shared: Worker | undefined;

export function makeVariants(file: Blob, opts: Options = {}): Promise<Result> {
  const { signal } = opts;
  if (signal?.aborted) return Promise.reject(abortError());
  return new Promise<Result>((resolve, reject) => {
    const job: Pending = { file, opts, resolve, reject, detach: () => signal?.removeEventListener("abort", onAbort) };
    // A job still waiting leaves the queue; one already running handles its own abort.
    const onAbort = () => {
      const at = queue.indexOf(job);
      if (at < 0) return;
      queue.splice(at, 1);
      reject(abortError());
    };
    signal?.addEventListener("abort", onAbort, { once: true });
    queue.push(job);
    pump();
  });
}

function pump(): void {
  if (running) return;
  const job = queue.shift();
  if (!job) return;
  running = true;
  job.detach();
  execute(job).finally(() => {
    running = false;
    pump();
  });
}

function endWorker(): void {
  shared?.terminate(); // also stops a wasm encode that cannot be interrupted any other way
  shared = undefined;
}

/** Runs one job on the shared worker; settles `job` and resolves when it is over. */
function execute({ file, opts, resolve, reject }: Pending): Promise<void> {
  const { signal } = opts;
  const started = performance.now();
  const timeoutMs = opts.timeoutMs ?? TIMEOUT_MS;
  if (signal?.aborted) {
    reject(abortError());
    return Promise.resolve();
  }
  return new Promise<void>((over) => {
    const result: Result = { variants: [], skipped: [] };
    let worker: Worker;
    try {
      worker = shared ??= new Worker(ENCODER_WORKER_URL, { type: "module" });
    } catch (err) {
      console.error("encode: the worker did not start", err);
      resolve({ variants: [], skipped: [{ reason: "worker_failed", detail: String(err) }] });
      over();
      return;
    }
    let timer: ReturnType<typeof setTimeout> | undefined;
    const finish = (settle: () => void, keepWorker: boolean) => {
      clearTimeout(timer);
      signal?.removeEventListener("abort", onAbort);
      worker.onmessage = worker.onerror = null;
      if (!keepWorker) endWorker();
      settle();
      over();
    };
    const fail = (s: Skipped, keepWorker: boolean) =>
      finish(() => resolve({ variants: [], skipped: [s], ...(result.source && { source: result.source }) }), keepWorker);
    const onAbort = () => finish(() => reject(abortError()), false);
    signal?.addEventListener("abort", onAbort, { once: true });
    const tick = () => {
      if (deadline(performance.now() - started, timeoutMs) === "timeout") {
        console.error(`encode: stopped after ${timeoutMs} ms`);
        fail({ reason: "timeout", detail: `${timeoutMs} ms` }, false);
      } else timer = setTimeout(tick, Math.max(1, timeoutMs - (performance.now() - started)));
    };
    timer = setTimeout(tick, timeoutMs);

    worker.onmessage = (e: MessageEvent<FromWorker>) => {
      const m = e.data;
      switch (m.type) {
        case "source":
          result.source = { width: m.width, height: m.height };
          break;
        case "progress":
          opts.onProgress?.(m.progress);
          break;
        case "variant":
          result.variants.push(m.variant);
          break;
        case "skipped":
          console.warn("encode: skipped", m.skipped);
          result.skipped.push(m.skipped);
          break;
        case "fatal":
          console.error("encode: gave up", m.skipped);
          fail(m.skipped, true); // the worker is fine: the picture was not
          break;
        case "done":
          finish(() => resolve(result), true);
          break;
      }
    };
    worker.onerror = (e) => {
      console.error("encode: the worker failed", e.message);
      fail({ reason: "worker_failed", detail: e.message }, false);
    };
    const job: Job = {
      file,
      wasmUrl: ENCODER_WASM_URL,
      widths: opts.widths ?? WIDTHS,
      formats: opts.formats ?? ["avif", "webp"],
      avifQuality: opts.avifQuality ?? 50,
      avifSpeed: opts.avifSpeed ?? 8,
      webpQuality: opts.webpQuality ?? 80,
      maxPixels: opts.maxPixels ?? MAX_PIXELS,
    };
    worker.postMessage(job);
  });
}
