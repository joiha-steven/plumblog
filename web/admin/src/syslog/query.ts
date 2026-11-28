/** What the System log screen asks for: its filters, how they live in the URL, and the API query they make. */

import type { LogQuery } from "../api/client";
import { LOG_KINDS, type LogKind } from "../api/model.gen";

export const LEVELS = ["all", "warning", "error"] as const;
export type LevelFilter = (typeof LEVELS)[number];

/** Newest first, so "all" is the open end; the others are a window ending now. */
export const RANGES = ["24h", "7d", "30d", "all"] as const;
export type Range = (typeof RANGES)[number];
const RANGE_MS: Record<Exclude<Range, "all">, number> = { "24h": 86_400_000, "7d": 7 * 86_400_000, "30d": 30 * 86_400_000 };

/** Rows asked per request. */
export const PAGE = 50;
/** The API takes 1-100 characters. */
export const Q_MAX = 100;

export interface Filters {
  level: LevelFilter;
  kind: LogKind | "all";
  range: Range;
  q: string;
}

export const NO_FILTERS: Filters = { level: "all", kind: "all", range: "all", q: "" };

/** The type select's groups; every kind is in exactly one (admin-test checks). */
export const KIND_GROUPS = [
  {
    id: "accounts",
    kinds: [
      "auth.sign_in",
      "auth.sign_in_failed",
      "auth.sign_in_locked",
      "auth.global_cap",
      "auth.password_changed",
      "auth.owner_reset",
      "auth.owner_reset_redeemed",
      "auth.sign_out",
      "auth.passkey_added",
      "auth.passkey_removed",
      "auth.token_created",
      "auth.token_revoked",
      "user.created",
    ],
  },
  { id: "content", kinds: ["content.changed", "comment.created", "comment.moderated"] },
  { id: "settings", kinds: ["settings.changed", "theme.changed", "plugin.changed", "plugin.disabled"] },
  { id: "system", kinds: ["system.deploy", "system.migration", "job.run"] },
  { id: "webhooks", kinds: ["webhook.delivery"] },
  { id: "errors", kinds: ["error"] },
] as const satisfies readonly { id: string; kinds: readonly LogKind[] }[];

/** The filters in `?level=&kind=&range=&q=`; anything unknown counts as "no filter". */
export function filtersOf(search: string): Filters {
  const p = new URLSearchParams(search);
  const level = LEVELS.find((l) => l === p.get("level")) ?? "all";
  const kind = LOG_KINDS.find((k) => k === p.get("kind")) ?? "all";
  const range = RANGES.find((r) => r === p.get("range")) ?? "all";
  return { level, kind, range, q: (p.get("q") ?? "").trim().slice(0, Q_MAX) };
}

/** The query string (with its `?`) for a filter set; "" when nothing is filtered. */
export function searchOf(f: Filters): string {
  const p = new URLSearchParams();
  if (f.level !== "all") p.set("level", f.level);
  if (f.kind !== "all") p.set("kind", f.kind);
  if (f.range !== "all") p.set("range", f.range);
  if (f.q !== "") p.set("q", f.q);
  const s = p.toString();
  return s ? `?${s}` : "";
}

export const isFiltered = (f: Filters): boolean => searchOf(f) !== "";

/** The API query for a filter set at time `now` (ms). `level=all` is no `level`: "info" would mean the same. */
export function queryOf(f: Filters, now: number, cursor?: string): LogQuery {
  return {
    limit: PAGE,
    ...(f.level === "all" ? {} : { level: f.level }),
    ...(f.kind === "all" ? {} : { kind: f.kind }),
    ...(f.range === "all" ? {} : { from: now - RANGE_MS[f.range] }),
    ...(f.q === "" ? {} : { q: f.q }),
    ...(cursor === undefined ? {} : { cursor }),
  };
}
