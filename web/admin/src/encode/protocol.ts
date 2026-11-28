// Messages between encode/index.ts (main thread) and encode/worker.ts. Types only.
import type { Mime, SkipReason } from "./plan.ts";

export interface Job {
  file: Blob;
  wasmUrl: string;
  widths: readonly number[];
  formats: readonly ("avif" | "webp")[];
  avifQuality: number; // 0-100
  avifSpeed: number; // 0-10, libaom cpu-used: higher is faster and larger
  webpQuality: number; // 0-100
  maxPixels: number;
}

export interface Variant {
  mime: Mime;
  width: number;
  height: number;
  bytes: Uint8Array;
  /** Lowercase hex SHA-256 of `bytes`. */
  sha256: string;
}

export interface Skipped {
  reason: SkipReason;
  mime?: Mime;
  width?: number;
  /** For the log, never for the reader. */
  detail?: string;
}

export interface Progress {
  done: number;
  total: number;
  mime: Mime;
  width: number;
}

export type FromWorker =
  | { type: "source"; width: number; height: number }
  | { type: "progress"; progress: Progress }
  | { type: "variant"; variant: Variant }
  | { type: "skipped"; skipped: Skipped }
  // The whole run is over and nothing from it should be used (decode failure, too large).
  | { type: "fatal"; skipped: Skipped }
  | { type: "done" };
