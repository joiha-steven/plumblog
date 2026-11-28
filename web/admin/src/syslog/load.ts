/** The System log screen's frame: the Settings header, the lock for writers, and the lazy chunk (screen.ts). */

import type { Api, Me } from "../api/client";
import { PERMISSION_ROLES } from "../api/model.gen";
import { type Child, h } from "../dom";
import { t } from "../i18n";
import { HREF } from "../router";
import { empty, failed, head, loadingRows } from "../ui";

export function syslogScreen(me: Me, api: Api): Child[] {
  if (!PERMISSION_ROLES["syslog.view"].includes(me.role)) {
    // A writer never reaches the API: the lock is drawn before anything is asked.
    return [
      head(t("admin.nav.settings")),
      empty({
        icon: "lock",
        title: t("admin.state.no_permission_title"),
        hint: t("admin.state.no_permission_hint"),
        action: h("a", { class: "btn", href: HREF.posts }, t("admin.state.back_to_posts")),
        full: true,
      }),
    ];
  }
  const find = h("div", { class: "find" });
  // Same box as the Settings sections: the chunk draws the section list and the log inside it.
  const body = h("div", { class: "settings-box" });
  const load = (): void => {
    body.replaceChildren(loadingRows());
    import("./screen").then(
      (m) => m.mountSyslog({ find, body, api, role: me.role }),
      (e: unknown) => {
        // A chunk that failed to load (offline, new deploy): say so instead of a blank screen.
        console.error("admin: system log failed to load", e);
        body.replaceChildren(failed({ code: "network", status: 0 }, load));
      },
    );
  };
  load();
  return [head(t("admin.nav.settings"), find), body];
}
