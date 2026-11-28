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
