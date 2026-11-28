/**
 * The account page's API tokens section: the signed-in user's own tokens (list, create, revoke).
 * `/api/auth/tokens` takes the session only and asks for no re-authentication. The token text
 * comes back once, in the create answer; it lives in this module's DOM until the writer is done
 * with it or leaves the page, and is never kept anywhere else.
 */

import type { Api, ApiError, Me } from "../api/client";
import { h } from "../dom";
import { getOnce } from "../early";
import { t } from "../i18n";
import { icon } from "../icons";
import { seg } from "../seg";
import { closePop, empty, failed, loadingRows, openPop, toast } from "../ui";
import { chars, field, message } from "./screens";

const MAX_NAME = 64;
const DAY = 86_400_000;

type Scope = "read" | "writer" | "owner";
const SCOPES: readonly Scope[] = ["read", "writer", "owner"];

/** One row of `GET /api/auth/tokens` (crates/worker/src/api/tokens.rs `token_json`). */
interface Token {
  id: number;
  name: string;
  scope: Scope;
  created_at: number;
  expires_at: number | null;
  revoked_at: number | null;
}

type Expiry = "never" | "30" | "90" | "365" | "custom";
const EXPIRIES: readonly Expiry[] = ["never", "30", "90", "365", "custom"];

const dateOf = (ms: number): string => new Intl.DateTimeFormat(document.documentElement.lang, { dateStyle: "medium" }).format(ms);

function scopeName(s: Scope): string {
  return s === "read" ? t("admin.tokens.scope_read") : s === "writer" ? t("admin.tokens.scope_writer") : t("admin.tokens.scope_owner");
}

function expiryName(e: Expiry): string {
  switch (e) {
    case "never":
      return t("admin.tokens.exp_never");
    case "30":
      return t("admin.tokens.exp_30");
    case "90":
      return t("admin.tokens.exp_90");
    case "365":
      return t("admin.tokens.exp_365");
    case "custom":
      return t("admin.tokens.exp_custom");
  }
}

/** The one plain message for a failure; `what` picks the wording a refusal needs. */
function explain(e: ApiError, what: "create" | "revoke"): string {
  switch (e.code) {
    case "auth.forbidden":
      return what === "create" ? t("admin.tokens.err_scope") : t("admin.tokens.err_gone");
    case "store.not_found":
      return t("admin.tokens.err_gone");
    case "input.invalid":
      return t("admin.tokens.err_input");
    default:
      return message(e, false) ?? t("admin.auth.err_bad_response");
  }
}

/** The section, ready to place in the account page. */
export function mountTokens(me: Me, api: Api): HTMLElement {
  const list = h("div", {});
  const secretBox = h("div", {});
  const addBox = h("div", { class: "tok-add" });
  const note = h("p", { class: "hint", id: "tok-offline", hidden: true }, t("admin.tokens.offline"));
  const addBtn = h("button", { class: "btn", type: "button", "aria-expanded": "false", onclick: () => openAdd() }, icon("plus"), t("admin.tokens.add"));
  const allowed = SCOPES.slice(0, me.role === "owner" ? 3 : 2);
  let loaded = false;
  let busy = false;

  const offline = (): boolean => !navigator.onLine;
  /** Offline the list stays readable; everything that writes is off, with the reason shown. */
  function sync(root: ParentNode = section): void {
    const off = offline();
    note.hidden = !off;
    for (const b of root.querySelectorAll<HTMLButtonElement>(".tok-add > button, .tok button.danger")) b.disabled = off || busy;
    for (const b of root.querySelectorAll<HTMLButtonElement>(".tok-add form button[type=submit]")) b.disabled = off || busy;
  }

  async function load(): Promise<void> {
    list.replaceChildren(loadingRows(2));
    addBox.hidden = true;
    const r = await getOnce<{ tokens: Token[] }>(api, "/api/auth/tokens");
    if (!r.ok) {
      loaded = false;
      list.replaceChildren(failed(r.error, () => void load()));
      return;
    }
    loaded = true;
    addBox.hidden = false;
    draw(r.value.tokens);
  }

  function draw(all: Token[]): void {
    // A revoked token is gone for the writer: it works nowhere and offers nothing to do.
    const live = all.filter((x) => x.revoked_at === null).sort((a, b) => b.id - a.id);
    list.replaceChildren(
      live.length
        ? h("div", { class: "tok-list" }, ...live.map(item))
        : empty({ icon: "lock", title: t("admin.tokens.empty"), hint: t("admin.tokens.empty_hint") }),
    );
    sync();
  }

  function item(tok: Token): HTMLElement {
    const nameId = `tok-name-${tok.id}`;
    const expired = tok.expires_at !== null && tok.expires_at <= Date.now();
    const exp = tok.expires_at === null ? t("admin.tokens.never_expires") : expired ? t("admin.tokens.expired", { date: dateOf(tok.expires_at) }) : t("admin.tokens.expires", { date: dateOf(tok.expires_at) });
    const revoke = h("button", { class: "btn danger tok-revoke", type: "button", "aria-describedby": nameId }, t("admin.tokens.revoke"));
    revoke.addEventListener("click", () => confirmRevoke(tok, revoke));
    return h(
      "div",
      { class: "tok" },
      h(
        "div",
        { class: "pk-main" },
        h("span", { class: "pk-name", id: nameId }, tok.name),
        h("span", { class: "pk-meta" }, h("span", { class: "tok-scope" }, scopeName(tok.scope)), h("span", {}, t("admin.tokens.created", { date: dateOf(tok.created_at) })), h("span", {}, exp)),
      ),
      revoke,
    );
  }

  function confirmRevoke(tok: Token, anchor: HTMLElement): void {
    const alert = h("div", { class: "auth-alert", role: "alert" });
    const cancel = h("button", { class: "btn", type: "button", onclick: () => closePop() }, t("admin.common.cancel"));
    const go = h("button", { class: "btn danger", type: "button" }, t("admin.tokens.revoke_submit"));
    go.addEventListener("click", () => {
      if (busy) return;
      busy = true;
      alert.textContent = "";
      cancel.disabled = true;
      go.disabled = true;
      go.textContent = t("admin.tokens.revoke_busy");
      void api.call("DELETE", `/api/auth/tokens/${tok.id}`).then((r) => {
        busy = false;
        cancel.disabled = false;
        go.disabled = false;
        go.textContent = t("admin.tokens.revoke_submit");
        if (!r.ok) {
          alert.textContent = explain(r.error, "revoke");
          // Already gone (revoked elsewhere): the message says the list is refreshed, so refresh it.
          if (r.error.code === "store.not_found" || r.error.code === "auth.forbidden") void load();
          return;
        }
        closePop(false);
        toast(t("admin.tokens.revoke_toast"));
        // The server may know of tokens this page does not (another tab): reload the truth.
        void load().then(() => addBtn.focus());
      });
    });
    openPop(
      anchor,
      h("div", { class: "confirm" }, h("div", { class: "confirm-q" }, h("strong", {}, t("admin.tokens.revoke_title", { name: tok.name })), h("p", { class: "hint" }, t("admin.tokens.revoke_hint"))), alert, h("div", { class: "btn-row end" }, cancel, go)),
      t("admin.tokens.revoke"),
    );
  }

  function showSecret(name: string, secret: string): void {
    const status = h("p", { class: "hint", role: "status" });
    const input = h("input", { class: "input mono", type: "text", readonly: true, "aria-labelledby": "tok-secret-h", value: secret });
    input.value = secret;
    const copy = h("button", { class: "btn", type: "button" }, t("admin.tokens.copy"));
    copy.addEventListener("click", () => {
      void (async () => {
        try {
          await navigator.clipboard.writeText(secret);
          status.textContent = t("admin.tokens.copied");
        } catch (e) {
          console.error("admin tokens: the clipboard refused the token text", e);
          status.textContent = t("admin.tokens.copy_failed");
          input.focus();
          input.select();
        }
      })();
    });
    const done = h("button", { class: "btn primary", type: "button", onclick: () => (secretBox.replaceChildren(), addBtn.focus()) }, t("admin.tokens.done"));
    secretBox.replaceChildren(
      h(
        "div",
        { class: "acct-panel tok-secret", role: "group", "aria-labelledby": "tok-secret-h" },
        h("strong", { id: "tok-secret-h" }, t("admin.tokens.secret_title", { name })),
        h("p", { class: "hint" }, t("admin.tokens.secret_once")),
        h("div", { class: "copyrow" }, input, copy),
        status,
        h("div", { class: "btn-row end" }, done),
      ),
    );
    input.focus();
    input.select();
  }

  function closeAdd(): void {
    addBox.replaceChildren(addBtn);
    addBtn.setAttribute("aria-expanded", "false");
    addBtn.focus();
    sync();
  }

  function openAdd(): void {
    secretBox.replaceChildren();
    const name = field({ id: "tok-name", label: t("admin.tokens.name"), autocomplete: "off", hint: t("admin.tokens.name_hint") });
    const nameErr = h("span", { class: "tok-err", id: "tok-name-err" });
    name.el.append(nameErr);
    let scope: Scope = "read";
    let expiry: Expiry = "never";
    const when = field({ id: "tok-date", label: t("admin.tokens.exp_date"), type: "date", autocomplete: "off" });
    when.el.hidden = true;
    const select = h("select", { class: "select", id: "tok-exp" }, ...EXPIRIES.map((e) => h("option", { value: e }, expiryName(e))));
    select.addEventListener("change", () => {
      expiry = select.value as Expiry;
      when.el.hidden = expiry !== "custom";
      alert.textContent = "";
    });
    const alert = h("div", { class: "auth-alert", role: "alert", tabindex: "-1" });
    const cancel = h("button", { class: "btn", type: "button", onclick: closeAdd }, t("admin.common.cancel"));
    const submit = h("button", { class: "btn primary", type: "submit" }, t("admin.tokens.add_submit"));
    const scopes = seg(t("admin.tokens.scope"), allowed, scope, scopeName, (v) => (scope = v));

    const bad = (text: string, at: HTMLInputElement, errEl?: HTMLElement): void => {
      at.setAttribute("aria-invalid", "true");
      if (errEl) {
        errEl.textContent = text;
        at.setAttribute("aria-describedby", `${at.id}-hint ${errEl.id}`);
      } else alert.textContent = text;
      at.focus();
    };

    /** The expiry as the API wants it (decimal ms string), or undefined for never, or a mistake. */
    const expiresAt = (): { at?: string } | { err: string } => {
      if (expiry === "never") return {};
      if (expiry !== "custom") return { at: String(Date.now() + Number(expiry) * DAY) };
      const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(when.input.value);
      // The end of the chosen day in the writer's own time zone.
      const ms = m ? new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]), 23, 59, 59).getTime() : Number.NaN;
      return Number.isFinite(ms) && ms > Date.now() ? { at: String(ms) } : { err: t("admin.tokens.err_date") };
    };

    async function go(): Promise<void> {
      if (busy || offline()) return;
      nameErr.textContent = "";
      alert.textContent = "";
      name.input.removeAttribute("aria-invalid");
      when.input.removeAttribute("aria-invalid");
      const n = name.input.value.trim();
      if (chars(n) < 1 || chars(n) > MAX_NAME) return bad(t("admin.tokens.err_name"), name.input, nameErr);
      const e = expiresAt();
      if ("err" in e) return bad(e.err, when.input, undefined);
      busy = true;
      form.setAttribute("aria-busy", "true");
      submit.disabled = true;
      cancel.disabled = true;
      submit.textContent = t("admin.tokens.add_busy");
      const body: { name: string; scope: Scope; expires_at?: string } = { name: n, scope };
      if (e.at !== undefined) body.expires_at = e.at;
      const r = await api.call<Token & { token: string }>("POST", "/api/auth/tokens", body);
      busy = false;
      form.setAttribute("aria-busy", "false");
      submit.disabled = offline();
      cancel.disabled = false;
      submit.textContent = t("admin.tokens.add_submit");
      if (!r.ok) {
        alert.textContent = explain(r.error, "create");
        alert.focus();
        return;
      }
      const secret = r.value.token;
      addBox.replaceChildren(addBtn);
      addBtn.setAttribute("aria-expanded", "false");
      await load();
      showSecret(n, secret);
    }

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
      name.el,
      h("div", { class: "field" }, h("div", { class: "label" }, t("admin.tokens.scope")), scopes, h("span", { class: "hint" }, t("admin.tokens.scope_hint"))),
      h("div", { class: "field" }, h("label", { for: "tok-exp" }, t("admin.tokens.expiry")), select),
      when.el,
      alert,
      h("div", { class: "btn-row end" }, cancel, submit),
    );
    addBtn.setAttribute("aria-expanded", "true");
    addBox.replaceChildren(form);
    sync();
    name.input.focus();
  }

  addBox.append(addBtn);
  const section = h(
    "section",
    { class: "sec", "aria-labelledby": "acct-tok" },
    h("h2", { id: "acct-tok" }, t("admin.tokens.title")),
    h("p", { class: "hint" }, t("admin.tokens.intro")),
    note,
    secretBox,
    list,
    addBox,
  );
  // The shell redraws other screens on a network change, which drops this section: stop listening
  // then, or every visit to the account page would leave a listener behind.
  const onNetwork = (): void => {
    if (!section.isConnected) {
      window.removeEventListener("online", onNetwork);
      window.removeEventListener("offline", onNetwork);
      return;
    }
    if (loaded) sync();
  };
  window.addEventListener("online", onNetwork);
  window.addEventListener("offline", onNetwork);
  void load();
  return section;
}
