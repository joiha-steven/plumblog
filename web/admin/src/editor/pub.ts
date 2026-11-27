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
  /** The slug the writer typed in the settings panel; undefined until they do. */
  typed: string | undefined;
  /** "Later" is picked: the button says Schedule. */
  later: boolean;
  /** The chosen time as a `datetime-local` value (local time), "" until one is picked. */
  at: string;
}

export const newPub = (post: Post | undefined): Pub => {
  const status = post?.status === "trashed" ? "draft" : (post?.status ?? "draft");
  return { id: post?.id, status, publishedAt: post?.published_at ?? null, slug: post?.slug ?? "", typed: undefined, later: status === "scheduled", at: "" };
};

/** The Publish button: Publish for a draft, Update for a live post, Schedule when "later". */
export const label = (p: Pub): string =>
  p.later ? t("admin.editor.pub.schedule") : p.status === "published" ? t("admin.editor.pub.update") : t("admin.editor.pub.publish");
