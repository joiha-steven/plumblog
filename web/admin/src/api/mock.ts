/**
 * The same interface as client.ts over web/admin/fixtures, for building screens before their
 * API exists (`scripts/admin-dev`). Query flags force each state for review and screenshots:
 * `?mock=loading|error|offline|empty`, `?role=writer`; `?upload=fail` makes uploads fail
 * halfway and `?upload=slow` takes 20 s.
 */

import me from "../../fixtures/me.json";
import posts from "../../fixtures/posts.json";
import longPost from "../../fixtures/post-1.txt";
import { type Api, type Me, type Media, type PostDraft, type PostSummary, fail } from "./client";

export function mockApi(params: URLSearchParams): Api {
  const mode = params.get("mock");
  const up = params.get("upload");
  let mediaId = 100;
  const role = params.get("role") === "writer" ? "writer" : "owner";
  const settle = async () => {
    if (mode === "loading") await new Promise(() => undefined);
    await new Promise((r) => setTimeout(r, 150));
  };
  return {
    async me() {
      await settle();
      if (mode === "error") return fail("internal", 500);
      if (mode === "offline") return fail("network", 0);
      const user: Me = { ...(me.user as Me), role };
      return { ok: true, value: user };
    },
    async posts() {
      await settle();
      if (mode === "error") return fail("internal", 500);
      if (mode === "offline") return fail("network", 0);
      return { ok: true, value: mode === "empty" ? [] : (posts as PostSummary[]) };
    },
    async post(id) {
      await settle();
      if (mode === "offline") return fail("network", 0);
      const s = (posts as PostSummary[]).find((p) => p.id === id);
      if (!s) return fail("post.not_found", 404);
      const body = id === 1 ? longPost : `${s.excerpt}\n`;
      const post: PostDraft = { id: s.id, title: s.title, body, lang: s.lang, status: s.status, updated_at: s.updated_at };
      return { ok: true, value: post };
    },
    // The dev Worker's real renderer (crates/worker/src/contract.rs, test builds only).
    previewUrl: "/__preview",
    // Sends nothing: progress over a second or two, then the picture from this browser.
    async upload(file, progress) {
      if (mode === "offline") return fail("network", 0);
      const steps = 12;
      for (let i = 1; i <= steps; i++) {
        await new Promise((r) => setTimeout(r, up === "slow" ? 1700 : 120));
        if (up === "fail" && i > steps / 2) return fail("internal", 500);
        progress(i / steps);
      }
      const url = URL.createObjectURL(file);
      const size = await new Promise<[number, number]>((resolve) => {
        const img = new Image();
        img.onload = () => resolve([img.naturalWidth, img.naturalHeight]);
        img.onerror = () => resolve([0, 0]);
        img.src = url;
      });
      const media: Media = { id: ++mediaId, url, width: size[0], height: size[1] };
      return { ok: true, value: media };
    },
  };
}
