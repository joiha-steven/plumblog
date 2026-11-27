/**
 * The signed-out pages: sign in, set up the first owner, accept an invite. Their own chunk,
 * so the shell does not carry them. They never call `me()`; every failure ends as one plain
 * message in the alert region (never a raw error code), and focus moves to it.
 */

import type { Api, ApiError, Result } from "../api/client";
import { h } from "../dom";
import { t } from "../i18n";
import { icon } from "../icons";
import { type NewAccount, acceptInvite, params, setup, signIn, signInPasskey } from "./flows";
import { safeNext } from "./next";

export type Kind = "signin" | "setup" | "invite";

export interface AuthOptions {
  root: HTMLElement;
  api: Api;
  kind: Kind;
  site: string;
  /** The page's own query: `next` and `code`. */
  query: URLSearchParams;
}

const MIN_PASSWORD = 12;
const SCRIPT = "https://challenges.cloudflare.com/turnstile/v0/api.js?render=explicit";

/** The part of Cloudflare's widget API used here. */
interface Turnstile {
  render(el: HTMLElement, o: Record<string, unknown>): string;
  reset(id: string): void;
  remove(id: string): void;
}

let script: Promise<Turnstile> | undefined;

/** Injects the widget script once, and only when a sign-in attempt asked for it. */
function loadTurnstile(): Promise<Turnstile> {
  script ??= new Promise<Turnstile>((resolve, reject) => {
    const el = h("script", { src: SCRIPT, async: true });
    el.onload = () => {
      const api = (window as unknown as { turnstile?: Turnstile }).turnstile;
      if (api) resolve(api);
      else reject(new Error("turnstile script ran but defined no turnstile"));
    };
    el.onerror = () => reject(new Error(`turnstile script did not load: ${SCRIPT}`));
    document.head.append(el);
  });
  // A failed load is not kept, so the next attempt tries again.
  script.catch(() => {
    script = undefined;
    document.querySelector(`script[src="${SCRIPT}"]`)?.remove();
  });
  return script;
}

interface Field {
  el: HTMLElement;
  input: HTMLInputElement;
}

function field(o: { id: string; label: string; type?: string; autocomplete: string; value?: string; hint?: string; mono?: boolean }): Field {
  const hintId = `${o.id}-hint`;
  const input = h("input", {
    class: o.mono ? "input lg mono" : "input lg",
    id: o.id,
    name: o.id,
    type: o.type ?? "text",
    autocomplete: o.autocomplete,
    autocapitalize: "off",
    spellcheck: "false",
    required: true,
    "aria-describedby": o.hint ? hintId : null,
  });
  if (o.value) input.value = o.value;
  input.addEventListener("input", () => input.removeAttribute("aria-invalid"));
  const el = h("div", { class: "field" }, h("label", { for: o.id }, o.label), input, o.hint ? h("span", { class: "hint", id: hintId }, o.hint) : null);
  return { el, input };
}

const chars = (s: string): number => [...s].length;

/** The one message for a failure, or undefined when nothing should be said (a dismissed prompt). */
function message(e: ApiError, passkey: boolean): string | undefined {
  switch (e.code) {
    case "cancelled":
      return undefined;
    case "auth.invalid_credentials":
      return t("admin.auth.err_invalid_credentials");
    case "auth.locked":
    case "auth.turnstile_required":
      return t("admin.auth.err_locked");
    case "settings.origin_missing":
      return t("admin.auth.err_origin");
    case "setup.invalid":
      return t("admin.auth.err_setup_invalid");
    case "invite.invalid":
      return t("admin.auth.err_invite_invalid");
    case "input.invalid":
      return t("admin.auth.err_input");
    case "unsupported":
      return passkey ? t("admin.auth.err_unsupported") : t("admin.auth.err_insecure");
    case "network":
      return t("admin.auth.err_network");
    default:
      return t("admin.auth.err_bad_response");
  }
}

export function mountAuth(o: AuthOptions): void {
  const { api, kind } = o;
  const signin = kind === "signin";
  const title = signin ? t("admin.auth.title_signin", { site: o.site }) : kind === "setup" ? t("admin.auth.title_setup", { site: o.site }) : t("admin.auth.title_invite", { site: o.site });
  const sub = signin ? t("admin.auth.sub_signin", { site: o.site }) : kind === "setup" ? t("admin.auth.sub_setup") : t("admin.auth.sub_invite");
  document.title = `${title}`;

  const code = field({ id: "code", label: kind === "setup" ? t("admin.auth.code_setup") : t("admin.auth.code_invite"), autocomplete: "off", value: o.query.get("code") ?? "", mono: true });
  const login = field({ id: "login", label: t("admin.auth.login"), autocomplete: "username" });
  const slug = field({ id: "slug", label: t("admin.auth.slug"), autocomplete: "off", hint: t("admin.auth.slug_hint"), mono: true });
  const name = field({ id: "display-name", label: t("admin.auth.display_name"), autocomplete: "name" });
  const password = field({
    id: "password",
    label: t("admin.auth.password"),
    type: "password",
    autocomplete: signin ? "current-password" : "new-password",
    ...(signin ? {} : { hint: t("admin.auth.password_hint") }),
  });
  const repeat = field({ id: "password-repeat", label: t("admin.auth.password_repeat"), type: "password", autocomplete: "new-password" });

  const alert = h("div", { class: "auth-alert", role: "alert", tabindex: "-1" });
  const challenge = h("div", { class: "auth-challenge", role: "group", "aria-label": t("admin.auth.challenge"), hidden: true });
  const submit = h("button", { class: "btn primary lg block", type: "submit" }, signin ? t("admin.auth.signin") : t("admin.auth.create"));
  const passkey = signin ? h("button", { class: "btn lg block", type: "button", onclick: () => void attempt(true, (tok) => signInPasskey(api, tok)) }, t("admin.auth.passkey")) : null;

  const say = (text: string | undefined, invalid?: HTMLInputElement): void => {
    alert.textContent = text ?? "";
    invalid?.setAttribute("aria-invalid", "true");
    if (text) alert.focus();
  };

  let busy = false;
  const setBusy = (on: boolean, viaPasskey: boolean): void => {
    busy = on;
    submit.disabled = on;
    if (passkey) passkey.disabled = on;
    form.setAttribute("aria-busy", String(on));
    submit.textContent = signin ? (on && !viaPasskey ? t("admin.auth.signin_busy") : t("admin.auth.signin")) : on ? t("admin.auth.create_busy") : t("admin.auth.create");
    if (passkey) passkey.textContent = on && viaPasskey ? t("admin.auth.passkey_busy") : t("admin.auth.passkey");
  };

  let widget: string | undefined;
  let again: ((token: string) => void) | undefined;

  /** A single-use token is spent by every attempt: the widget starts over before the next. */
  const resetWidget = (): void => {
    const ts = (window as unknown as { turnstile?: Turnstile }).turnstile;
    if (ts && widget !== undefined) ts.reset(widget);
  };

  /** Shows the widget and resubmits by itself when the writer passes it. */
  async function challenged(): Promise<void> {
    const p = await params(api);
    const key = p.ok ? p.value.turnstile_site_key : null;
    // Without a key there is nothing to pass: the writer can only wait.
    if (!key) return say(t("admin.auth.err_locked"));
    say(t("admin.auth.err_turnstile"));
    challenge.hidden = false;
    try {
      const ts = await loadTurnstile();
      if (widget !== undefined) {
        ts.reset(widget);
        return;
      }
      widget = ts.render(challenge, {
        sitekey: key,
        callback: (token: string) => again?.(token),
        "error-callback": (code: unknown) => {
          console.error(`auth: turnstile widget error ${String(code)}`);
          say(t("admin.auth.err_turnstile_load"));
        },
      });
    } catch (e) {
      console.error("auth: turnstile failed to load", e);
      challenge.hidden = true;
      say(t("admin.auth.err_turnstile_load"));
    }
  }

  async function attempt(viaPasskey: boolean, run: (token?: string) => Promise<Result<unknown>>, token?: string): Promise<void> {
    if (busy) return;
    again = (tok) => void attempt(viaPasskey, run, tok);
    say(undefined);
    setBusy(true, viaPasskey);
    const r = await run(token);
    if (r.ok) {
      // Stay busy: the page is about to change.
      location.assign(signin ? safeNext(o.query.get("next")) : "/admin/");
      return;
    }
    setBusy(false, viaPasskey);
    if (token) resetWidget();
    if (r.error.code === "auth.turnstile_required") return challenged();
    say(message(r.error, viaPasskey));
  }

  /** The rules the server checks (crates/worker/src/api/fields.rs), so a mistake is named, not guessed. */
  function invalid(): { text: string; at: HTMLInputElement } | undefined {
    if (!code.input.value.trim()) return { text: t("admin.auth.err_code"), at: code.input };
    const n = chars(login.input.value.trim());
    if (n < 1 || n > 64) return { text: t("admin.auth.err_login"), at: login.input };
    if (!/^[a-z0-9](?:[a-z0-9-]{0,62}[a-z0-9])?$/.test(slug.input.value.trim())) return { text: t("admin.auth.err_slug"), at: slug.input };
    const m = chars(name.input.value.trim());
    if (m < 1 || m > 100) return { text: t("admin.auth.err_name"), at: name.input };
    if (chars(password.input.value) < MIN_PASSWORD) return { text: t("admin.auth.err_password_short"), at: password.input };
    if (password.input.value !== repeat.input.value) return { text: t("admin.auth.err_password_mismatch"), at: repeat.input };
    return undefined;
  }

  const form = h(
    "form",
    {
      novalidate: true,
      onsubmit: (e: Event) => {
        e.preventDefault();
        if (busy) return;
        const user = login.input.value.trim();
        if (signin) {
          void attempt(false, (tok) => signIn(api, user, password.input.value, tok));
          return;
        }
        const bad = invalid();
        if (bad) {
          say(bad.text, bad.at);
          return;
        }
        const account: NewAccount = {
          code: code.input.value.trim(),
          login: user,
          slug: slug.input.value.trim(),
          display_name: name.input.value.trim(),
          password: password.input.value,
        };
        void attempt(false, (tok) => (kind === "setup" ? setup(api, account, tok) : acceptInvite(api, account, tok)));
      },
    },
    alert,
    signin ? [login.el, password.el] : [code.el, login.el, slug.el, name.el, password.el, repeat.el],
    challenge,
    submit,
    passkey ? [h("div", { class: "or" }, t("admin.auth.or")), passkey] : null,
  );

  o.root.replaceChildren(
    h(
      "div",
      { class: "auth solo" },
      h(
        "main",
        { class: "auth-main" },
        h(
          "div",
          { class: "auth-card" },
          h(
            "div",
            { class: "auth-brand" },
            h("div", { class: "auth-id" }, h("div", { class: "mark" }, icon("plum")), h("span", {}, o.site)),
            h("div", { class: "auth-titles" }, h("h1", {}, title), h("p", { class: "auth-sub" }, sub)),
          ),
          form,
        ),
      ),
    ),
  );
}
