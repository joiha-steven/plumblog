/**
 * The same interface as client.ts over web/admin/fixtures, for building screens before their
 * API exists (`scripts/admin-dev`). Query flags force each state for review and screenshots:
 * `?mock=loading|error|offline`, `?role=writer`.
 */

import me from "../../fixtures/me.json";
import { type Api, type Me, fail } from "./client";

export function mockApi(params: URLSearchParams): Api {
  const mode = params.get("mock");
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
  };
}
