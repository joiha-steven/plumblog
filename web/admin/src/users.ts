/**
 * Settings > People: who can sign in, their roles, and the invitations that bring more people in.
 * Over `/api/v1/users` and `/api/v1/invites` (crates/worker/src/api/v1/users.rs), session only.
 * Its own chunk, loaded by settings.ts. The invite code comes back once, in the create answer;
 * it lives in this module's DOM until the owner is done with it or leaves, and nowhere else.
 */

import type { Api, ApiError, Result } from "./api/client";
import type { Invite, InviteCreated, InviteList, User, UserPage } from "./api/model.gen";
import { h } from "./dom";
import { getOnce } from "./early";
import { t } from "./i18n";
import { icon } from "./icons";
import { seg } from "./seg";
import { closePop, empty, failed, loadingRows, openPop, toast } from "./ui";

export interface UsersOptions {
  /** The signed-in user's id, for the "you" mark. */
  me: number;
  /** Read at each use: a page that goes offline keeps its list but cannot change anything. */
  offline: () => boolean;
}

const DAY = 86_400_000;
/** The server refuses an invite more than 30 days ahead by its own clock; a minute of slack covers a fast local clock. */
const CLOCK_SLACK = 60_000;
type Role = "owner" | "writer";
const ROLES: readonly Role[] = ["writer", "owner"];
const EXPIRIES = ["1", "7", "30"] as const;
type Expiry = (typeof EXPIRIES)[number];

const dateOf = (ms: number): string => new Intl.DateTimeFormat(document.documentElement.lang, { dateStyle: "medium" }).format(ms);

function roleName(r: string): string {
  return r === "owner" ? t("admin.users.role_owner") : t("admin.users.role_writer");
}

function expiryName(e: Expiry): string {
  return e === "1" ? t("admin.users.inv_exp_1") : e === "7" ? t("admin.users.inv_exp_7") : t("admin.users.inv_exp_30");
}

/** The one plain message for a refused or failed call; `what` picks the wording a conflict needs. */
function explain(e: ApiError, what: "user" | "invite" | "create"): string {
  if (e.code === "user.last_owner") return t("admin.users.err_last_owner");
  if (e.code === "network") return t("admin.users.err_network");
  if (e.code === "auth.forbidden") return t("admin.users.err_forbidden");
  if (e.status === 404) return what === "invite" ? t("admin.users.inv_err_gone") : t("admin.users.err_gone");
  if (e.code === "store.conflict" && what === "user") return t("admin.users.err_has_posts");
  return t("admin.users.err_other", { code: e.code });
}

export function mountUsers(box: HTMLElement, api: Api, o: UsersOptions): void {
  let people: User[] = [];
  let cursor: string | null = null;
  let busy = false;

  const note = h("p", { class: "hint", hidden: true }, t("admin.users.offline"));
  const alert = h("div", { class: "auth-alert", role: "alert", tabindex: "-1" });
  const list = h("div", {});
  const tail = h("div", { class: "btn-row" });
  const invList = h("div", {});
  const linkBox = h("div", {});
  const addBox = h("div", { class: "usr-add" });
  const heading = h("h2", { id: "usr-h", tabindex: "-1" }, t("admin.users.title"));
  const addBtn = h("button", { class: "btn", type: "button", "aria-expanded": "false", onclick: () => openAdd() }, icon("plus"), t("admin.users.inv_add"));

  /** Everything that writes follows `busy` and the network; one place, so a new button cannot forget. */
  function sync(): void {
    const off = o.offline();
    note.hidden = !off;
    for (const b of box.querySelectorAll<HTMLButtonElement | HTMLSelectElement>(".usr select, .usr [data-act], .inv [data-act], .usr-add > button, .usr-add button[type=submit], .usr-more")) b.disabled = off || busy;
  }

  function fail(e: ApiError, what: "user" | "invite" | "create"): void {
    alert.textContent = explain(e, what);
    alert.focus();
    // Gone elsewhere: the message says the list is refreshed, so refresh it.
    if (e.status === 404) void (what === "invite" ? loadInvites() : loadUsers());
  }

  // --- people ---

  async function loadUsers(): Promise<void> {
    list.replaceChildren(loadingRows(3));
    tail.replaceChildren();
    const r = await getOnce<UserPage>(api, "/api/v1/users");
    if (!r.ok) {
      list.replaceChildren(failed(r.error, () => void loadUsers()));
      return;
    }
    people = r.value.users;
    cursor = r.value.next_cursor;
    drawUsers();
  }

  async function loadMore(): Promise<void> {
    if (cursor === null || busy) return;
    busy = true;
    const more = tail.querySelector("button");
    if (more) more.textContent = t("admin.users.loading_more");
    sync();
    const r = await api.call<UserPage>("GET", `/api/v1/users?cursor=${encodeURIComponent(cursor)}`);
    busy = false;
    if (!r.ok) {
      fail(r.error, "user");
      drawUsers();
      return;
    }
    const seen = new Set(people.map((u) => u.id));
    people = [...people, ...r.value.users.filter((u) => !seen.has(u.id))];
    cursor = r.value.next_cursor;
    drawUsers();
  }

  function drawUsers(): void {
    // Only the signed-in owner: the rows are not enough, say what to do next.
    const alone = people.every((u) => u.id === o.me);
    list.replaceChildren(h("div", { class: "pk-list" }, ...people.map(personRow)), ...(alone ? [empty({ icon: "user", title: t("admin.users.empty"), hint: t("admin.users.empty_hint") })] : []));
    tail.replaceChildren(cursor === null ? "" : h("button", { class: "btn usr-more", type: "button", onclick: () => void loadMore() }, t("admin.users.more")));
    sync();
  }

  function replaceUser(u: User): void {
    people = people.map((p) => (p.id === u.id ? u : p));
  }

  /** Redraws one row and puts focus back on the control used, so a keyboard user keeps their place. */
  function redraw(u: User, focus: string): void {
    const old = list.querySelector(`.usr[data-id="${u.id}"]`);
    const fresh = personRow(u);
    old?.replaceWith(fresh);
    sync();
    fresh.querySelector<HTMLElement>(focus)?.focus();
  }

  function personRow(u: User): HTMLElement {
    const nameId = `usr-name-${u.id}`;
    const disabled = u.disabled_at !== null;
    const select = h(
      "select",
      { class: "select fit", "aria-label": t("admin.users.role_of", { name: u.display_name }), "data-focus": "select" },
      ...ROLES.map((r) => h("option", { value: r, selected: r === u.role }, roleName(r))),
    );
    select.value = u.role;
    select.addEventListener("change", () => {
      const to = select.value as Role;
      // The control shows the stored role until the server has agreed.
      select.value = u.role;
      if (to === u.role) return;
      if (u.id === o.me && to !== "owner") {
        askThen(select, { title: t("admin.users.demote_title"), hint: t("admin.users.demote_hint"), submit: t("admin.users.demote_submit"), busy: t("admin.users.demote_busy") }, () => changeRole(u, to));
      } else void changeRole(u, to).then((r) => r !== undefined && fail(r, "user"));
    });
    const toggle = h("button", { class: "btn", type: "button", "data-act": "toggle", "aria-describedby": nameId }, disabled ? t("admin.users.enable") : t("admin.users.disable"));
    toggle.addEventListener("click", () => {
      if (disabled) void setDisabled(u, false).then((r) => r !== undefined && fail(r, "user"));
      else askThen(toggle, { title: t("admin.users.disable_title", { name: u.display_name }), hint: t("admin.users.disable_hint"), submit: t("admin.users.disable_submit"), busy: t("admin.users.disable_busy") }, () => setDisabled(u, true));
    });
    const del = h("button", { class: "btn danger", type: "button", "data-act": "delete", "aria-describedby": nameId }, t("admin.users.delete"));
    del.addEventListener("click", () =>
      askThen(del, { title: t("admin.users.delete_title", { name: u.display_name }), hint: t("admin.users.delete_hint"), submit: t("admin.users.delete_submit"), busy: t("admin.users.delete_busy") }, () => remove(u)),
    );
    return h(
      "div",
      { class: "usr", "data-id": String(u.id) },
      h(
        "div",
        { class: "pk-main" },
        h("span", { class: "pk-name", id: nameId }, u.display_name, u.id === o.me ? h("span", { class: "usr-tag" }, t("admin.users.you")) : null),
        h(
          "span",
          { class: "pk-meta" },
          h("span", { class: "usr-login" }, u.login),
          h("span", { class: disabled ? "usr-off" : "" }, disabled ? t("admin.users.state_disabled") : t("admin.users.state_active")),
          h("span", {}, t("admin.users.joined", { date: dateOf(u.created_at) })),
        ),
      ),
      h("div", { class: "usr-act" }, select, toggle, del),
    );
  }

  /** Runs one change with the page locked; returns the error, or undefined on success. */
  async function guarded<T>(run: () => Promise<Result<T>>): Promise<Result<T>> {
    busy = true;
    alert.textContent = "";
    sync();
    const r = await run();
    busy = false;
    return r;
  }

  async function changeRole(u: User, to: Role): Promise<ApiError | undefined> {
    const r = await guarded(() => api.call<User>("PUT", `/api/v1/users/${u.id}/role`, { role: to }));
    if (!r.ok) {
      sync();
      return r.error;
    }
    replaceUser(r.value);
    redraw(r.value, "select");
    toast(t("admin.users.role_toast", { name: u.display_name, role: roleName(to) }));
    // The server drops an owner's unused invites when they stop being one.
    if (u.role === "owner" && to !== "owner") void loadInvites();
    return undefined;
  }

  async function setDisabled(u: User, off: boolean): Promise<ApiError | undefined> {
    const r = await guarded(() => api.call<User>("PUT", `/api/v1/users/${u.id}/disabled`, { disabled: off }));
    if (!r.ok) {
      sync();
      return r.error;
    }
    replaceUser(r.value);
    redraw(r.value, "[data-act=toggle]");
    const vars = { name: u.display_name };
    toast(off ? t("admin.users.disable_toast", vars) : t("admin.users.enable_toast", vars));
    return undefined;
  }

  async function remove(u: User): Promise<ApiError | undefined> {
    const r = await guarded(() => api.call<undefined>("DELETE", `/api/v1/users/${u.id}`));
    if (!r.ok) {
      sync();
      return r.error;
    }
    people = people.filter((p) => p.id !== u.id);
    drawUsers();
    toast(t("admin.users.delete_toast", { name: u.display_name }));
    heading.focus();
    return undefined;
  }

  /**
   * A confirm popover next to `anchor`. `run` answers an error to show on the page (the popover
   * closes either way), or undefined when it worked.
   */
  function askThen(anchor: HTMLElement, q: { title: string; hint: string; submit: string; busy: string }, run: () => Promise<ApiError | undefined>, what: "user" | "invite" = "user"): void {
    const cancel = h("button", { class: "btn", type: "button", onclick: () => closePop() }, t("admin.common.cancel"));
    const go = h("button", { class: "btn danger", type: "button" }, q.submit);
    go.addEventListener("click", () => {
      if (busy) return;
      cancel.disabled = true;
      go.disabled = true;
      go.textContent = q.busy;
      void run().then((e) => {
        closePop(false);
        if (e) fail(e, what);
      });
    });
    openPop(anchor, h("div", { class: "confirm" }, h("div", { class: "confirm-q" }, h("strong", {}, q.title), h("p", { class: "hint" }, q.hint)), h("div", { class: "btn-row end" }, cancel, go)), q.title);
  }

  // --- invitations ---

  async function loadInvites(): Promise<void> {
    invList.replaceChildren(loadingRows(2));
    const r = await getOnce<InviteList>(api, "/api/v1/invites");
    if (!r.ok) {
      invList.replaceChildren(failed(r.error, () => void loadInvites()));
      return;
    }
    const all = [...r.value.invites].sort((a, b) => b.id - a.id);
    invList.replaceChildren(
      all.length ? h("div", { class: "pk-list" }, ...all.map(inviteRow)) : empty({ icon: "link", title: t("admin.users.inv_empty"), hint: t("admin.users.inv_empty_hint") }),
    );
    sync();
  }

  function inviteRow(i: Invite): HTMLElement {
    const nameId = `inv-name-${i.id}`;
    const expired = i.expires_at <= Date.now();
    const revoke = h("button", { class: "btn danger", type: "button", "data-act": "revoke", "aria-describedby": nameId }, t("admin.users.inv_revoke"));
    revoke.addEventListener("click", () =>
      askThen(
        revoke,
        { title: t("admin.users.inv_revoke_title"), hint: t("admin.users.inv_revoke_hint"), submit: t("admin.users.inv_revoke_submit"), busy: t("admin.users.inv_revoke_busy") },
        async () => {
          const r = await guarded(() => api.call<undefined>("DELETE", `/api/v1/invites/${i.id}`));
          if (!r.ok) {
            sync();
            return r.error;
          }
          toast(t("admin.users.inv_revoke_toast"));
          await loadInvites();
          addBtn.focus();
          return undefined;
        },
        "invite",
      ),
    );
    return h(
      "div",
      { class: "inv", "data-id": String(i.id) },
      h(
        "div",
        { class: "pk-main" },
        h("span", { class: "pk-name", id: nameId }, t("admin.users.inv_invite_for", { role: roleName(i.role) }), expired ? h("span", { class: "usr-tag" }, t("admin.users.inv_expired_chip")) : null),
        h("span", { class: "pk-meta" }, h("span", {}, t("admin.users.inv_created", { date: dateOf(i.created_at) })), h("span", {}, expired ? t("admin.users.inv_expired", { date: dateOf(i.expires_at) }) : t("admin.users.inv_expires", { date: dateOf(i.expires_at) }))),
      ),
      revoke,
    );
  }

  function closeAdd(): void {
    addBox.replaceChildren(addBtn);
    addBtn.setAttribute("aria-expanded", "false");
    addBtn.focus();
    sync();
  }

  function openAdd(): void {
    linkBox.replaceChildren();
    let role: Role = "writer";
    let expiry: Expiry = "7";
    const warn = h("p", { class: "hint inv-warn", id: "inv-warn", role: "status" }, t("admin.users.inv_owner_warning"));
    warn.hidden = true;
    const roles = seg(t("admin.users.inv_role"), ROLES, role, roleName, (v) => {
      role = v;
      warn.hidden = v !== "owner";
    });
    const select = h("select", { class: "select", id: "inv-exp" }, ...EXPIRIES.map((e) => h("option", { value: e, selected: e === expiry }, expiryName(e))));
    select.value = expiry;
    select.addEventListener("change", () => (expiry = select.value as Expiry));
    const cancel = h("button", { class: "btn", type: "button", onclick: closeAdd }, t("admin.common.cancel"));
    const submit = h("button", { class: "btn primary", type: "submit" }, t("admin.users.inv_submit"));
    const form = h(
      "form",
      {
        class: "acct-panel",
        novalidate: true,
        onsubmit: (ev: Event) => {
          ev.preventDefault();
          void go();
        },
      },
      h("div", { class: "field" }, h("div", { class: "label" }, t("admin.users.inv_role")), roles, warn),
      h("div", { class: "field" }, h("label", { for: "inv-exp" }, t("admin.users.inv_expiry")), select),
      h("div", { class: "btn-row end" }, cancel, submit),
    );

    async function go(): Promise<void> {
      if (busy || o.offline()) return;
      busy = true;
      form.setAttribute("aria-busy", "true");
      submit.textContent = t("admin.users.inv_busy");
      alert.textContent = "";
      sync();
      const expires_at = Date.now() + Number(expiry) * DAY - CLOCK_SLACK;
      const r = await api.call<InviteCreated>("POST", "/api/v1/invites", { role, expires_at });
      busy = false;
      form.setAttribute("aria-busy", "false");
      submit.textContent = t("admin.users.inv_submit");
      sync();
      if (!r.ok) return fail(r.error, "create");
      const made = r.value;
      closeAdd();
      await loadInvites();
      showLink(made);
    }

    addBtn.setAttribute("aria-expanded", "true");
    addBox.replaceChildren(form);
    sync();
    addBtn.disabled = false;
    roles.querySelector("button")?.focus();
  }

  function showLink(made: InviteCreated): void {
    const url = `${location.origin}/admin/invite?code=${encodeURIComponent(made.code)}`;
    const status = h("p", { class: "hint", role: "status" });
    const input = h("input", { class: "input mono", type: "text", readonly: true, "aria-labelledby": "inv-link-h", value: url });
    input.value = url;
    const copy = h("button", { class: "btn", type: "button" }, t("admin.users.copy"));
    copy.addEventListener("click", () => {
      void (async () => {
        try {
          await navigator.clipboard.writeText(url);
          status.textContent = t("admin.users.copied");
        } catch (e) {
          console.error("admin users: the clipboard refused the invitation link", e);
          status.textContent = t("admin.users.copy_failed");
          input.focus();
          input.select();
        }
      })();
    });
    const done = h("button", { class: "btn primary", type: "button", onclick: () => (linkBox.replaceChildren(), addBtn.focus()) }, t("admin.users.done"));
    linkBox.replaceChildren(
      h(
        "div",
        { class: "acct-panel", role: "group", "aria-labelledby": "inv-link-h" },
        h("strong", { id: "inv-link-h" }, t("admin.users.link_title", { role: roleName(made.role) })),
        h("p", { class: "hint" }, t("admin.users.link_once")),
        h("div", { class: "copyrow" }, input, copy),
        status,
        h("div", { class: "btn-row end" }, done),
      ),
    );
    input.focus();
    input.select();
  }

  addBox.append(addBtn);
  const peopleSection = h("section", { class: "sec", "aria-labelledby": "usr-h" }, heading, h("p", { class: "hint" }, t("admin.users.intro")), note, alert, list, tail);
  const inviteSection = h("section", { class: "sec", "aria-labelledby": "inv-h" }, h("h2", { id: "inv-h" }, t("admin.users.inv_title")), h("p", { class: "hint" }, t("admin.users.inv_intro")), linkBox, invList, addBox);
  box.replaceChildren(peopleSection);

  // The shell redraws other screens on a network change, which drops this one: stop listening then.
  const onNetwork = (): void => {
    if (!box.isConnected) {
      removeEventListener("online", onNetwork);
      removeEventListener("offline", onNetwork);
      return;
    }
    sync();
  };
  addEventListener("online", onNetwork);
  addEventListener("offline", onNetwork);

  // The invitations join once the list has settled: shown beside a list still loading, they would
  // jump down when it fills in (a layout shift, measured by Lighthouse).
  void loadUsers().then(() => {
    box.append(inviteSection);
    sync();
    void loadInvites();
  });
}
