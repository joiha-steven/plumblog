/**
 * The signed-in user's account: who they are, their passkeys (add, remove), API tokens (tokens.ts), and sign-out.
 * Its own chunk (with the sign-in screens it shares); the shell loads it on first use.
 * Every failure ends as one plain message in the form's alert region, never a raw code.
 */

import type { Api, ApiError, Me, Result } from "../api/client";
import { type Child, h } from "../dom";
import { countDrafts } from "../editor/drafts";
import { t } from "../i18n";
import { icon } from "../icons";
import { closePop, empty, failed, loadingRows, openPop, toast } from "../ui";
import { type Passkey, type Reauth, addPasskey, listPasskeys, removePasskey, signOut } from "./flows";
import { chars, field, message } from "./screens";
import { mountTokens } from "./tokens";

const MAX_NAME = 64;

// --- sign-out ---

/**
 * Signs out, after asking when this browser holds drafts that sign-out deletes. A failure says
 * so and deletes nothing (flows.signOut clears drafts only after the server ended the session).
 */
export function signOutFlow(o: { anchor: HTMLElement; api: Api; me: Me }): void {
  const finish = async (): Promise<boolean> => {
    const r = await signOut(o.api, o.me.id);
    if (r.ok) {
      // A full load: the sign-in page runs under its own CSP.
      location.assign("/admin/signin");
      return true;
    }
    toast(t("admin.account.err_signout"), 5000);
    return false;
  };
  void (async () => {
    const n = await countDrafts(o.me.id);
    if (n === 0) {
      await finish();
      return;
    }
    const cancel = h("button", { class: "btn", type: "button", onclick: () => closePop() }, t("admin.common.cancel"));
    const go = h("button", { class: "btn danger", type: "button" }, t("admin.account.signout_delete"));
    go.addEventListener("click", () => {
      cancel.disabled = true;
      go.disabled = true;
      go.textContent = t("admin.account.signout_busy");
      void finish().then((done) => {
        if (done) return;
        cancel.disabled = false;
        go.disabled = false;
        go.textContent = t("admin.account.signout_delete");
      });
    });
    openPop(
      o.anchor,
      h("div", { class: "confirm" }, h("p", {}, t("admin.account.signout_drafts", { count: n })), h("div", { class: "btn-row end" }, cancel, go)),
      t("admin.account.signout"),
    );
  })();
}

// --- re-authentication form, shared by adding and removing a passkey ---

type Kind = "add" | "remove";

/** The one message for a failure, or undefined when nothing should be said (a dismissed prompt). */
function explain(e: ApiError, kind: Kind): string | undefined {
  switch (e.code) {
    case "cancelled":
      return undefined;
    case "auth.invalid_credentials":
      return t("admin.account.err_wrong_password");
    case "auth.reauth_required":
      return t("admin.account.err_reauth");
    case "auth.locked":
      return t("admin.auth.err_locked");
    case "store.conflict":
      return kind === "remove" ? t("admin.account.err_conflict") : message(e, true);
    case "settings.origin_missing":
      return t("admin.account.err_origin");
    case "unsupported":
      return t("admin.account.err_unsupported");
    default:
      return message(e, true);
  }
}

interface FormOptions {
  api: Api;
  me: Me;
  kind: Kind;
  /** Makes element ids unique when two forms could be on the page. */
  id: string;
  cls: string;
  /** Above the choice: a title, a hint, the name field. */
  lead: Child[];
  submit: string;
  busy: string;
  danger?: boolean;
  /** A mistake to name before anything is sent. */
  validate?: () => { text: string; at: HTMLInputElement } | undefined;
  run: (how: Reauth) => Promise<Result<unknown>>;
  done: () => void;
  cancel: () => void;
}

/** A form asking the writer to prove it is them (password or passkey), then running the action. */
function reauthForm(o: FormOptions): HTMLFormElement {
  let how: "password" | "passkey" = "password";
  const pw = field({ id: `${o.id}-password`, label: t("admin.auth.password"), type: "password", autocomplete: "current-password" });
  const note = h("p", { class: "hint", hidden: true }, t("admin.account.confirm_passkey_hint"));
  const choices = (["password", "passkey"] as const).map((v) =>
    h(
      "button",
      {
        type: "button",
        "aria-pressed": String(v === how),
        onclick: () => {
          how = v;
          for (const b of choices) b.setAttribute("aria-pressed", String(b === choices[v === "password" ? 0 : 1]));
          pw.el.hidden = v !== "password";
          note.hidden = v !== "passkey";
          say(undefined);
        },
      },
      v === "password" ? t("admin.account.confirm_password") : t("admin.account.confirm_passkey"),
    ),
  );
  const label = h("div", { class: "label", id: `${o.id}-who` }, t("admin.account.confirm_who"));
  const alert = h("div", { class: "auth-alert", role: "alert", tabindex: "-1" });
  const cancel = h("button", { class: "btn", type: "button", onclick: o.cancel }, t("admin.common.cancel"));
  const submit = h("button", { class: o.danger ? "btn danger" : "btn primary", type: "submit" }, o.submit);

  const say = (text: string | undefined, invalid?: HTMLInputElement): void => {
    alert.textContent = text ?? "";
    invalid?.setAttribute("aria-invalid", "true");
    if (text) alert.focus();
  };

  let busy = false;

  async function go(): Promise<void> {
    if (busy) return;
    say(undefined);
    const bad = o.validate?.();
    if (bad) return say(bad.text, bad.at);
    let proof: Reauth;
    if (how === "passkey") proof = "passkey";
    else {
      if (!pw.input.value) return say(t("admin.account.err_password_empty"), pw.input);
      proof = { password: pw.input.value, login: o.me.login };
    }
    busy = true;
    form.setAttribute("aria-busy", "true");
    submit.disabled = true;
    cancel.disabled = true;
    submit.textContent = o.busy;
    const r = await o.run(proof);
    busy = false;
    form.setAttribute("aria-busy", "false");
    submit.disabled = false;
    cancel.disabled = false;
    submit.textContent = o.submit;
    if (r.ok) return o.done();
    // Signed-in pages keep the strict CSP, which has no room for Turnstile's script: after
    // too many wrong proofs the writer waits out the window, as when locked.
    if (r.error.code === "auth.turnstile_required") return say(t("admin.auth.err_locked"));
    const wrong = r.error.code === "auth.invalid_credentials" ? pw.input : undefined;
    say(explain(r.error, o.kind), wrong);
    wrong?.select();
  }

  pw.input.addEventListener("input", () => pw.input.removeAttribute("aria-invalid"));
  const form = h(
    "form",
    {
      class: o.cls,
      novalidate: true,
      onsubmit: (e: Event) => {
        e.preventDefault();
        void go();
      },
    },
    ...o.lead,
    h("div", { class: "reauth" }, label, h("div", { class: "reauth-seg" }, h("div", { class: "seg", role: "group", "aria-labelledby": `${o.id}-who` }, ...choices)), pw.el, note),
    alert,
    h("div", { class: "btn-row end" }, cancel, submit),
  );
  return form;
}

// --- the screen ---

/** A shared piece with its flush-left modifier (components.css). */
const flush = (el: HTMLElement, cls: string): HTMLElement => (el.classList.add(cls), el);

const dateOf = (ms: number): string => new Intl.DateTimeFormat(document.documentElement.lang, { dateStyle: "medium" }).format(ms);

/** Draws the account into `box` (the page's content area). */
export function mountAccount(box: HTMLElement, me: Me, api: Api): void {
  const list = h("div", {});
  const addBox = h("div", { class: "acct-add" });
  const addBtn = h("button", { class: "btn", type: "button", "aria-expanded": "false", onclick: () => openAdd() }, icon("plus"), t("admin.account.add"));

  async function load(): Promise<void> {
    list.replaceChildren(flush(loadingRows(2), "flush"));
    addBox.hidden = true;
    const r = await listPasskeys(api);
    // From here the passkeys list has its height: the tokens section below can show without moving.
    tokens.hidden = false;
    if (!r.ok) {
      list.replaceChildren(failed(r.error, () => void load()));
      return;
    }
    addBox.hidden = false;
    list.replaceChildren(
      r.value.length
        ? h("div", { class: "pk-list" }, ...r.value.map(item))
        : flush(empty({ icon: "lock", title: t("admin.account.passkeys_empty"), hint: t("admin.account.passkeys_empty_hint") }), "start"),
    );
  }

  function item(p: Passkey): HTMLElement {
    const nameId = `pk-name-${p.id}`;
    const last = p.last_used_at === null ? t("admin.account.never_used") : t("admin.account.last_used", { date: dateOf(p.last_used_at) });
    const remove = h("button", { class: "btn danger", type: "button", "aria-describedby": nameId }, t("admin.account.remove"));
    remove.addEventListener("click", () => {
      const form = reauthForm({
        api,
        me,
        kind: "remove",
        id: "pk-remove",
        cls: "confirm",
        lead: [h("div", { class: "confirm-q" }, h("strong", {}, t("admin.account.remove_title", { name: p.name })), h("p", { class: "hint" }, t("admin.account.remove_hint")))],
        submit: t("admin.account.remove_submit"),
        busy: t("admin.account.remove_busy"),
        danger: true,
        run: (how) => removePasskey(api, p.id, how),
        done: () => {
          closePop(false);
          toast(t("admin.account.remove_toast"));
          void load().then(() => addBtn.focus());
        },
        cancel: () => closePop(),
      });
      openPop(remove, form, t("admin.account.remove"));
      form.querySelector<HTMLInputElement>("input")?.focus();
    });
    return h(
      "div",
      { class: "pk" },
      h(
        "div",
        { class: "pk-main" },
        h("span", { class: "pk-name", id: nameId }, p.name),
        h("span", { class: "pk-meta" }, h("span", {}, t("admin.account.added", { date: dateOf(p.created_at) })), h("span", {}, last)),
      ),
      remove,
    );
  }

  function closeAdd(): void {
    addBox.replaceChildren(addBtn);
    addBtn.setAttribute("aria-expanded", "false");
    addBtn.focus();
  }

  function openAdd(): void {
    const name = field({
      id: "pk-name",
      label: t("admin.account.add_name"),
      autocomplete: "off",
      hint: t("admin.account.add_name_hint"),
      value: t("admin.account.add_name_default"),
    });
    const form = reauthForm({
      api,
      me,
      kind: "add",
      id: "pk-add",
      cls: "acct-panel",
      lead: [name.el],
      submit: t("admin.account.add_submit"),
      busy: t("admin.account.add_busy"),
      validate: () => {
        const n = chars(name.input.value.trim());
        return n < 1 || n > MAX_NAME ? { text: t("admin.account.err_name"), at: name.input } : undefined;
      },
      run: (how) => addPasskey(api, name.input.value.trim(), how),
      done: () => {
        toast(t("admin.account.add_toast"));
        closeAdd();
        void load();
      },
      cancel: closeAdd,
    });
    addBtn.setAttribute("aria-expanded", "true");
    addBox.replaceChildren(form);
    name.input.focus();
    name.input.select();
  }

  addBox.append(addBtn);
  // Loaded at once but out of the layout until the passkeys list above settles: two loading rows
  // collapsing into "no passkeys" would otherwise push it up (Lighthouse CLS 0.03).
  const tokens = mountTokens(me, api);
  tokens.hidden = true;
  // i18n: admin.role.owner admin.role.writer
  const role = t(`admin.role.${me.role}`);
  box.replaceChildren(
    h(
      "section",
      { class: "sec", "aria-labelledby": "acct-you" },
      h("h2", { id: "acct-you" }, t("admin.account.you")),
      h(
        "div",
        { class: "acct-kv" },
        h("div", { class: "prow" }, h("span", { class: "k" }, t("admin.account.name")), h("span", { class: "v" }, me.display_name)),
        h("div", { class: "prow" }, h("span", { class: "k" }, t("admin.account.login")), h("span", { class: "v" }, me.login)),
        h("div", { class: "prow" }, h("span", { class: "k" }, t("admin.account.role")), h("span", { class: "v" }, role)),
      ),
    ),
    h(
      "section",
      { class: "sec", "aria-labelledby": "acct-pk" },
      h("h2", { id: "acct-pk" }, t("admin.account.passkeys")),
      list,
      addBox,
    ),
    tokens,
  );
  void load();
}
