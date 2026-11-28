// The API tokens section of the account page (web/admin/src/auth/tokens.ts) in happy-dom with a
// fake Api: list, create (body, secret shown once), scope limits, revoke, errors, offline.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { build } from "esbuild";
import { Window } from "happy-dom";

const win = new Window({ url: "http://localhost/admin/account" });
for (const k of ["document", "Node", "HTMLElement", "addEventListener", "removeEventListener"]) {
  const v = typeof win[k] === "function" && !/^[A-Z]/.test(k) ? win[k].bind(win) : win[k];
  Object.defineProperty(globalThis, k, { value: v, configurable: true, writable: true });
}
Object.defineProperty(globalThis, "window", { value: win, configurable: true, writable: true });
let online = true;
let copied;
let copyFails = false;
Object.defineProperty(globalThis, "navigator", {
  value: {
    get onLine() {
      return online;
    },
    clipboard: {
      writeText: async (s) => {
        if (copyFails) throw new Error("denied");
        copied = s;
      },
    },
  },
  configurable: true,
  writable: true,
});

document.documentElement.lang = "en";
const en = {};
{
  let section = "";
  for (const line of readFileSync(new URL("../../locales/en.toml", import.meta.url), "utf8").split("\n")) {
    const s = /^\[(.+)\]$/.exec(line);
    if (s) section = s[1];
    const kv = /^(\w+) = "((?:[^"\\]|\\.)*)"$/.exec(line);
    if (kv) en[`${section}.${kv[1]}`] = kv[2].replace(/\\"/g, '"');
  }
}

const root = new URL("../../", import.meta.url);
const src = (f) => new URL(`web/admin/src/${f}`, root).pathname;
const out = await build({
  stdin: {
    contents: `export { mountTokens } from ${JSON.stringify(src("auth/tokens.ts"))};
      export { mountAccount } from ${JSON.stringify(src("auth/account.ts"))};
      export { startEarly } from ${JSON.stringify(src("early.ts"))};
      export { initI18n } from ${JSON.stringify(src("i18n.ts"))};
      export { closePop, followNetwork, offlineBanner } from ${JSON.stringify(src("ui.ts"))};`,
    resolveDir: new URL("tools/", root).pathname,
    loader: "ts",
  },
  bundle: true,
  write: false,
  format: "esm",
  logLevel: "silent",
  nodePaths: [new URL("tools/node_modules", root).pathname],
});
const m = await import(`data:text/javascript;base64,${Buffer.from(out.outputFiles[0].text).toString("base64")}`);
m.initI18n({ nonce: "", site: "S", lang: "en", langs: ["en"] }, { messages: en, fallback: en });

const settle = () => new Promise((r) => setTimeout(r, 10));
const DAY = 86_400_000;
const T = (id, name, scope, extra = {}) => ({ id, name, scope, created_at: Date.UTC(2026, 9, id), expires_at: null, revoked_at: null, ...extra });

/** A fake Api: `routes` answers by "METHOD path"; every call is recorded. */
function setup({ role = "owner", tokens = [T(1, "CI", "writer"), T(2, "Old", "read", { revoked_at: 5 })], routes = {} } = {}) {
  const calls = [];
  let rows = tokens;
  const call = async (method, path, body) => {
    calls.push({ method, path, body });
    const custom = routes[`${method} ${path}`];
    if (custom) return custom(body);
    if (method === "GET") return { ok: true, value: { tokens: rows } };
    if (method === "POST") {
      const made = T(9, body.name, body.scope, { expires_at: body.expires_at ? Number(body.expires_at) : null });
      rows = [...rows, made];
      return { ok: true, value: { ...made, token: "plb_SECRETSECRETSECRET" } };
    }
    rows = rows.filter((r) => `/api/auth/tokens/${r.id}` !== path);
    return { ok: true, value: undefined };
  };
  const el = m.mountTokens({ id: 1, login: "lan", slug: "lan", display_name: "Lan", bio: "", role }, { call });
  document.body.replaceChildren(el);
  return { el, calls };
}
const $ = (el, q) => el.querySelector(q);
const type = (input, v) => {
  input.value = v;
  input.dispatchEvent(new win.Event("input", { bubbles: true }));
};
const submit = (form) => form.dispatchEvent(new win.Event("submit", { bubbles: true, cancelable: true }));
const openForm = async (el) => {
  await settle();
  $(el, ".tok-add button").click();
  return $(el, "form.acct-panel");
};

test("lists live tokens with scope and expiry; revoked ones are left out", async () => {
  const { el } = setup();
  await settle();
  const rows = el.querySelectorAll(".tok");
  assert.equal(rows.length, 1);
  assert.match(rows[0].textContent, /CI/);
  assert.match(rows[0].textContent, /Writer/);
  assert.match(rows[0].textContent, /Never expires/);
});

test("an expired token says so", async () => {
  const { el } = setup({ tokens: [T(1, "Gone", "read", { expires_at: Date.now() - DAY })] });
  await settle();
  assert.match($(el, ".tok").textContent, /Expired/);
});

test("no tokens: the empty state", async () => {
  const { el } = setup({ tokens: [] });
  await settle();
  assert.equal(el.querySelectorAll(".tok").length, 0);
  assert.ok($(el, ".empty"));
});

test("a failed load shows the error with a retry that loads again", async () => {
  let n = 0;
  const { el, calls } = setup({
    routes: { "GET /api/auth/tokens": async () => (n++ === 0 ? { ok: false, error: { code: "network", status: 0 } } : { ok: true, value: { tokens: [T(1, "CI", "read")] } }) },
  });
  await settle();
  assert.ok($(el, "[role=alert]"));
  $(el, "[role=alert] button").click();
  await settle();
  assert.equal(el.querySelectorAll(".tok").length, 1);
  assert.equal(calls.filter((c) => c.method === "GET").length, 2);
});

test("create sends the trimmed name, the scope and the expiry as a decimal ms string", async () => {
  const { el, calls } = setup();
  const form = await openForm(el);
  type($(form, "input[type=text]"), "  Agent  ");
  [...form.querySelectorAll(".seg button")].find((b) => b.textContent === "Read").click();
  $(form, "select").value = "30";
  $(form, "select").dispatchEvent(new win.Event("change", { bubbles: true }));
  const before = Date.now();
  submit(form);
  await settle();
  const post = calls.find((c) => c.method === "POST");
  assert.equal(post.path, "/api/auth/tokens");
  assert.deepEqual(Object.keys(post.body).sort(), ["expires_at", "name", "scope"]);
  assert.equal(post.body.name, "Agent");
  assert.equal(post.body.scope, "read");
  assert.match(post.body.expires_at, /^\d+$/);
  assert.ok(Math.abs(Number(post.body.expires_at) - (before + 30 * DAY)) < 5000);
});

test("never expires leaves expires_at out", async () => {
  const { el, calls } = setup();
  const form = await openForm(el);
  type($(form, "input[type=text]"), "Agent");
  submit(form);
  await settle();
  assert.equal("expires_at" in calls.find((c) => c.method === "POST").body, false);
});

test("the secret shows once, copies, and is gone after a re-render; the token joins the list", async () => {
  const { el } = setup();
  const form = await openForm(el);
  type($(form, "input[type=text]"), "Agent");
  submit(form);
  await settle();
  const secret = $(el, "input[readonly]");
  assert.equal(secret.value, "plb_SECRETSECRETSECRET");
  assert.match(el.textContent, new RegExp(en["admin.tokens.secret_once"]));
  assert.equal(el.querySelectorAll(".tok").length, 2);
  assert.ok(!$(el, ".tok").textContent.includes("plb_"), "list rows never hold the secret");
  [...el.querySelectorAll("button")].find((b) => b.textContent === en["admin.tokens.copy"]).click();
  await settle();
  assert.equal(copied, "plb_SECRETSECRETSECRET");
  // Leaving the page and coming back is a new mount: no secret.
  const again = setup();
  await settle();
  assert.equal($(again.el, "input[readonly]"), null);
  assert.ok(!again.el.innerHTML.includes("SECRETSECRET"));
});

test("a refused clipboard says to copy by hand", async () => {
  copyFails = true;
  const { el } = setup();
  const form = await openForm(el);
  type($(form, "input[type=text]"), "Agent");
  submit(form);
  await settle();
  [...el.querySelectorAll("button")].find((b) => b.textContent === en["admin.tokens.copy"]).click();
  await settle();
  copyFails = false;
  assert.match($(el, ".tok-secret [role=status]").textContent, new RegExp(en["admin.tokens.copy_failed"]));
});

test("an empty or 65-character name is named inline and nothing is sent", async () => {
  const { el, calls } = setup();
  const form = await openForm(el);
  submit(form);
  const input = $(form, "input[type=text]");
  assert.equal(input.getAttribute("aria-invalid"), "true");
  assert.ok($(form, "#" + input.getAttribute("aria-describedby").split(" ").pop()).textContent.includes(en["admin.tokens.err_name"]));
  type(input, "x".repeat(65));
  submit(form);
  assert.equal(calls.filter((c) => c.method === "POST").length, 0);
  assert.equal(input.getAttribute("aria-invalid"), "true");
});

test("a writer can pick read or writer, never owner; an owner can pick all three", async () => {
  const w = setup({ role: "writer" });
  const wf = await openForm(w.el);
  assert.deepEqual([...wf.querySelectorAll(".seg button")].map((b) => b.textContent), ["Read", "Writer"]);
  const o = setup({ role: "owner" });
  const of = await openForm(o.el);
  assert.deepEqual([...of.querySelectorAll(".seg button")].map((b) => b.textContent), ["Read", "Writer", "Owner"]);
});

test("create: the button is disabled while busy, and a refusal is said in plain words", async () => {
  let release;
  const { el } = setup({
    routes: { "POST /api/auth/tokens": () => new Promise((r) => (release = () => r({ ok: false, error: { code: "auth.forbidden", status: 403 } }))) },
  });
  const form = await openForm(el);
  type($(form, "input[type=text]"), "Agent");
  submit(form);
  assert.equal($(form, "button[type=submit]").disabled, true);
  release();
  await settle();
  assert.equal($(form, "button[type=submit]").disabled, false);
  assert.equal($(form, "[role=alert]").textContent, en["admin.tokens.err_scope"]);
  assert.equal($(el, "input[readonly]"), null);
});

test("revoke asks first, names the token, then sends DELETE and removes the row", async () => {
  const { el, calls } = setup();
  await settle();
  $(el, ".tok button.danger").click();
  const pop = document.querySelector(".pop .confirm");
  assert.ok(pop.textContent.includes("CI"));
  assert.equal(calls.some((c) => c.method === "DELETE"), false);
  pop.querySelector("button.danger").click();
  await settle();
  const del = calls.find((c) => c.method === "DELETE");
  assert.equal(del.path, "/api/auth/tokens/1");
  assert.equal(el.querySelectorAll(".tok").length, 0);
  assert.equal(document.querySelector(".pop"), null);
});

test("a failed revoke keeps the row and says why", async () => {
  const { el } = setup({ routes: { "DELETE /api/auth/tokens/1": async () => ({ ok: false, error: { code: "network", status: 0 } }) } });
  await settle();
  $(el, ".tok button.danger").click();
  const pop = document.querySelector(".pop .confirm");
  pop.querySelector("button.danger").click();
  await settle();
  assert.equal(el.querySelectorAll(".tok").length, 1);
  assert.ok(pop.querySelector("[role=alert]").textContent.length > 0);
  assert.equal(pop.querySelector("button.danger").disabled, false);
  m.closePop(false);
});

test("offline: actions are disabled with a message, and come back online", async () => {
  online = false;
  const { el } = setup();
  await settle();
  assert.equal($(el, ".tok-add button").disabled, true);
  assert.equal($(el, ".tok button.danger").disabled, true);
  assert.ok(el.textContent.includes(en["admin.tokens.offline"]));
  online = true;
  win.dispatchEvent(new win.Event("online"));
  assert.equal($(el, ".tok-add button").disabled, false);
  assert.equal($(el, ".tok button.danger").disabled, false);
});

const make = async (el) => {
  const form = await openForm(el);
  type($(form, "input[type=text]"), "Agent");
  submit(form);
  await settle();
};

test("I've saved it: the secret leaves the page", async () => {
  const { el } = setup();
  await make(el);
  assert.ok(el.innerHTML.includes("SECRETSECRET"));
  [...el.querySelectorAll("button")].find((b) => b.textContent === en["admin.tokens.done"]).click();
  assert.equal($(el, "input[readonly]"), null);
  assert.ok(!el.innerHTML.includes("SECRETSECRET"));
  assert.equal(document.activeElement, $(el, ".tok-add > button"));
});

test("a network blip on the account page keeps the secret: only the banner follows", async () => {
  const { el } = setup();
  const main = document.createElement("main");
  main.append(el);
  document.body.replaceChildren(main);
  await make(el);
  let redrawn = 0;
  // What main.ts does on `offline` then `online` for the account screen.
  online = false;
  win.dispatchEvent(new win.Event("offline"));
  assert.equal(m.followNetwork(main, "account", true, () => redrawn++), undefined);
  assert.ok(main.firstElementChild.classList.contains("banner"));
  online = true;
  win.dispatchEvent(new win.Event("online"));
  m.followNetwork(main, "account", false, () => redrawn++);
  assert.equal(redrawn, 0);
  assert.equal(main.querySelector(".banner"), null);
  assert.equal($(el, "input[readonly]").value, "plb_SECRETSECRETSECRET");
  // Any other screen still redraws.
  m.followNetwork(main, "posts", true, () => redrawn++);
  assert.equal(redrawn, 1);
});

test("a section no longer on the page stops listening to the network", async () => {
  const { el } = setup();
  await settle();
  document.body.replaceChildren();
  online = false;
  win.dispatchEvent(new win.Event("offline"));
  // A detached section is not synced: its buttons keep their last state.
  assert.equal($(el, ".tok-add > button").disabled, false);
  online = true;
  win.dispatchEvent(new win.Event("online"));
});

test("revoking a token that is already gone says so and reloads the list", async () => {
  let gets = 0;
  const { el, calls } = setup({
    routes: {
      "DELETE /api/auth/tokens/1": async () => ({ ok: false, error: { code: "store.not_found", status: 404 } }),
      "GET /api/auth/tokens": async () => ({ ok: true, value: { tokens: gets++ === 0 ? [T(1, "CI", "writer")] : [] } }),
    },
  });
  await settle();
  $(el, ".tok button.danger").click();
  document.querySelector(".pop .confirm button.danger").click();
  await settle();
  assert.equal(calls.filter((c) => c.method === "GET").length, 2);
  assert.equal(el.querySelectorAll(".tok").length, 0);
  m.closePop(false);
});

test("the account page shows tokens only once the passkeys above have settled, so nothing moves", async () => {
  let release;
  const passkeys = new Promise((r) => (release = r));
  const call = async (method, path) => {
    if (path === "/api/auth/passkey") return passkeys;
    if (path === "/api/auth/tokens") return { ok: true, value: { tokens: [T(1, "CI", "writer")] } };
    return { ok: false, error: { code: "network", status: 0 } };
  };
  const box = document.createElement("div");
  document.body.replaceChildren(box);
  m.mountAccount(box, { id: 1, login: "lan", slug: "lan", display_name: "Lan", bio: "", role: "owner" }, { call });
  await settle();
  const tokens = box.querySelector("[aria-labelledby=acct-tok]");
  assert.equal(tokens.hidden, true, "held back while the passkeys list may still change height");
  assert.equal(tokens.querySelectorAll(".tok").length, 1, "but already loaded");
  release({ ok: true, value: { passkeys: [] } });
  await settle();
  assert.equal(tokens.hidden, false);
});

test("lists started early at boot are read once, not fetched again", async () => {
  let gets = 0;
  const call = async (method, path) => {
    if (method === "GET") gets++;
    if (path === "/api/auth/passkey") return { ok: true, value: { passkeys: [] } };
    if (path === "/api/auth/tokens") return { ok: true, value: { tokens: [T(1, "CI", "writer")] } };
    return { ok: false, error: { code: "network", status: 0 } };
  };
  const api = { call };
  m.startEarly(api, ["/api/auth/passkey", "/api/auth/tokens"]);
  assert.equal(gets, 2);
  const box = document.createElement("div");
  document.body.replaceChildren(box);
  m.mountAccount(box, { id: 1, login: "lan", slug: "lan", display_name: "Lan", bio: "", role: "owner" }, api);
  await settle();
  assert.equal(gets, 2, "the account page used the early answers");
  assert.equal(box.querySelectorAll(".tok").length, 1);
});
