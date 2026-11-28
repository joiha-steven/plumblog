/** The media screen's pure parts: filters, paging, who may delete, how sizes read. No DOM. */

import type { MediaItem, MediaListQuery, MediaPage } from "../api/client";
import type { Role } from "../api/model.gen";
import { PERMISSION_ROLES } from "../api/model.gen";

export const FILTERS = ["all", "missing_alt", "unused"] as const;
export type Filter = (typeof FILTERS)[number];

/** Items asked per request; the grid is wide, so a page fills a few rows. */
export const PAGE = 24;

/** The filter in `?show=`; anything else is "all". */
export function filterOf(search: string): Filter {
  const s = new URLSearchParams(search).get("show");
  return FILTERS.find((f) => f === s) ?? "all";
}

/** The request for one page of a filter; `cursor` is the previous page's `next_cursor`. */
export function queryFor(filter: Filter, cursor: string | null): MediaListQuery {
  const q: MediaListQuery = { limit: PAGE };
  if (filter !== "all") q[filter] = true;
  if (cursor !== null) q.cursor = cursor;
  return q;
}

/** `items` plus a page's items, skipping any already shown (an item can slide across a page edge). */
export function mergePage(items: readonly MediaItem[], page: MediaPage): MediaItem[] {
  const seen = new Set(items.map((m) => m.id));
  return [...items, ...page.media.filter((m) => !seen.has(m.id))];
}

/**
 * Whether the delete button is offered: the uploader, or a role with `media.delete_any`. The
 * server decides in the end; this only keeps the button away from people it would refuse.
 */
export function canDelete(m: Pick<MediaItem, "uploaded_by">, me: { id: number; role: Role }): boolean {
  return m.uploaded_by === me.id || PERMISSION_ROLES["media.delete_any"].includes(me.role);
}

/** The file name from `/media/<id>/<name>`. */
export function nameOf(src: string): string {
  const last = src.slice(src.lastIndexOf("/") + 1);
  try {
    return decodeURIComponent(last);
  } catch {
    return last;
  }
}

/** `image/avif` as `AVIF`. */
export const kindOf = (mime: string): string => mime.replace(/^image\//, "").replace(/\+.*$/, "").toUpperCase();

/** 1536 as "1.5 kB" in the page language (1024-based, as file managers show it). */
export function formatBytes(bytes: number, locale: string): string {
  const units = ["byte", "kilobyte", "megabyte", "gigabyte"] as const;
  let n = bytes;
  let i = 0;
  while (n >= 1024 && i < units.length - 1) {
    n /= 1024;
    i++;
  }
  return new Intl.NumberFormat(locale, { style: "unit", unit: units[i], unitDisplay: "short", maximumFractionDigits: i === 0 ? 0 : 1 }).format(n);
}

/** `1200x800` with the multiplication sign (\u00d7). */
export const dimensions = (m: Pick<MediaItem, "width" | "height">): string => `${m.width}\u00d7${m.height}`;

// ---- upload ---------------------------------------------------------------------------

/** What the API takes (`MEDIA_MIMES`), and its size cap (20 MiB). */
export const UPLOAD_TYPES = ["image/avif", "image/webp", "image/jpeg", "image/png", "image/gif"] as const;
export const UPLOAD_MAX_BYTES = 20 * 1024 * 1024;
/** Files in flight at once; the rest wait their turn. */
export const UPLOAD_PARALLEL = 2;

/** Whether the upload controls are offered: the API's `media.upload` permission. */
export const canUpload = (me: { role: Role }): boolean => PERMISSION_ROLES["media.upload"].includes(me.role);

/** Why a file is not sent at all, or null when it may go. */
export function refuseFile(f: Pick<File, "type" | "size">): "type" | "size" | null {
  if (!(UPLOAD_TYPES as readonly string[]).includes(f.type)) return "type";
  return f.size > UPLOAD_MAX_BYTES ? "size" : null;
}

/** Lowercase hex of a digest, the form the API's `sha256` takes. */
export const toHex = (bytes: ArrayBuffer): string => [...new Uint8Array(bytes)].map((b) => b.toString(16).padStart(2, "0")).join("");

/** Hex SHA-256 of a file's bytes; undefined where the browser has no `crypto.subtle` (the upload then goes without it). */
export async function sha256Of(file: Blob): Promise<string | undefined> {
  if (!globalThis.crypto?.subtle) return undefined;
  return toHex(await crypto.subtle.digest("SHA-256", await file.arrayBuffer()));
}

/** How a failed upload reads to the writer; the screen turns each into its own words. */
export type UploadFault = "unreadable" | "too_big" | "offline" | "forbidden" | "refused" | "failed";
export function faultOf(e: { code: string; status: number }): UploadFault {
  if (e.code === "network") return "offline";
  if (e.code === "api.body_too_large") return "too_big";
  if (e.code === "auth.forbidden") return "forbidden";
  // Status 0 is the client's own check: the browser could not read the picture.
  if (e.code === "input.invalid") return e.status === 0 ? "unreadable" : "refused";
  return "failed";
}

/** First in, first out, at most `limit` jobs running; `count` is how many run or wait. */
export class Pool {
  private running = 0;
  private readonly waiting: (() => Promise<void>)[] = [];
  constructor(private readonly limit: number) {}
  /** Jobs running or waiting. */
  get count(): number {
    return this.running + this.waiting.length;
  }
  get idle(): boolean {
    return this.count === 0;
  }
  add(job: () => Promise<void>): void {
    this.waiting.push(job);
    this.pump();
  }
  private pump(): void {
    while (this.running < this.limit) {
      const job = this.waiting.shift();
      if (!job) return;
      this.running++;
      // A job that throws must not stall the queue; its own code reports its failure.
      void job()
        .catch((e: unknown) => console.error("admin media: an upload job threw", e))
        .finally(() => {
          this.running--;
          this.pump();
        });
    }
  }
}
