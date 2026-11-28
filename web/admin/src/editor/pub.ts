/**
 * What the editor knows about a post's publishing, shared by the editor (the Publish button's
 * label, the status chip) and the lazily loaded publish flow (publish.ts).
 */

import type { Post } from "../api/model.gen";
import { t } from "../i18n";

export interface Pub {
  /** The post's id; undefined until a new post is created. */
  id: number | undefined;
  /** The server's status; a trashed post is never edited here. */
  status: Exclude<Post["status"], "trashed">;
  /** The server's publish time (ms), or the scheduled one. */
  publishedAt: number | null;
  /** The server's slug; "" for a post not created yet. */
  slug: string;
  /**
   * The title the server slug was last seen with (load, create, publish). Autosave never
   * changes it, so a draft's slug follows a retitle while it still matches this title.
   */
  slugTitle: string;
  /** The slug the writer typed in the settings panel; undefined until they do. */
  typed: string | undefined;
  /** "Later" is picked: the button says Schedule. */
  later: boolean;
  /** The chosen time as a `datetime-local` value (local time), "" until one is picked. */
  at: string;
}

/** A `datetime-local` value (local time) for a time in ms. */
export const localValue = (ms: number): string => new Date(ms - new Date(ms).getTimezoneOffset() * 60000).toISOString().slice(0, 16);

/** The server's copy `p` is what `pub` now says; what the writer typed for the slug stays. */
export const fromPost = (pub: Pub, p: Post): Pub => {
  const status = p.status === "trashed" ? "draft" : p.status;
  const later = status === "scheduled";
  // A scheduled post's time is its publish time, so Schedule works before the panel is drawn.
  const at = later && p.published_at ? localValue(p.published_at) : "";
  return Object.assign(pub, { id: p.id, status, publishedAt: p.published_at, slug: p.slug, slugTitle: p.title, later, at });
};

/** The post was just created from the draft: only what names it; "later" and its time stay as picked. */
export const created = (pub: Pub, p: Post): Pub => Object.assign(pub, { id: p.id, slug: p.slug, slugTitle: p.title });

export const newPub = (post: Post | undefined): Pub => {
  const pub: Pub = { id: undefined, status: "draft", publishedAt: null, slug: "", slugTitle: "", typed: undefined, later: false, at: "" };
  return post ? fromPost(pub, post) : pub;
};

/** The Publish button: Publish for a draft, Update for a live post, Schedule when "later". */
export const label = (p: Pub): string =>
  p.later ? t("admin.editor.pub.schedule") : p.status === "published" ? t("admin.editor.pub.update") : t("admin.editor.pub.publish");
