// The sign-in screens (web/admin/src/auth/screens.ts) in happy-dom: the language switch on all
// four kinds, and sign-in handing a site with no owner yet over to setup.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { build } from "esbuild";
import { Window } from "happy-dom";

const win = new Window({ url: "http://localhost/admin/setup" });
for (const k of ["document", "location", "Node", "HTMLElement", "addEventListener", "removeEventListener"]) {
  const v = typeof win[k] === "function" && !/^[A-Z]/.test(k) ? win[k].bind(win) : win[k];
  Object.defineProperty(globalThis, k, { value: v, configurable: true, writable: true });
}
Object.defineProperty(globalThis, "window", { value: win, configurable: true, writable: true });
let reloads = 0;
const moved = [];
Object.defineProperty(globalThis, "location", {
  value: { reload: () => reloads++, replace: (u) => moved.push(u), assign: (u) => moved.push(u), pathname: "/admin/setup", search: "" },
  configurable: true,
  writable: true,
});

const en = {};
{
  let section = "";
  for (const line of readFileSync(new URL("../../locales/en.toml", import.meta.url), "utf8").split("\n")) {
    const s = /^\[(.+)\]$/.exec(line);
    if (s) section = s[1];
    const kv = /^(\w+) = "((?:[^"\\]|\\.)*)"$/.exec(line);
    if (kv) en[`${section}.${kv[1]}`] = kv[2];
  }
}

const root = new URL("../../", import.meta.url);
const src = (f) => new URL(`web/admin/src/${f}`, root).pathname;
const out = await build({
  stdin: {
    contents: `export { mountAuth } from ${JSON.stringify(src("auth/screens.ts"))};
      export { initI18n } from ${JSON.stringify(src("i18n.ts"))};`,
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

const PARAMS = { site_id: "s", stretch_iterations: 600000, turnstile_site_key: null };
const mount = (kind, boot, params = PARAMS) => {
  m.initI18n({ nonce: "", site: "S", ...boot }, { messages: en, fallback: en });
  const root = document.createElement("div");
  const call = async (method, path) => (method === "GET" && path === "/api/auth/params" ? { ok: true, value: params } : { ok: false, error: { code: "network", status: 0 } });
  const api = { call, get: async () => ({ ok: false }), post: async () => ({ ok: false }) };
  m.mountAuth({ api, kind, root, site: "S", query: new URLSearchParams(), turnstile: undefined, passkey: false });
  return root;
};

for (const kind of ["signin", "setup", "invite", "reset"]) {
  test(`${kind}: the switch lists each language, the current one pressed`, () => {
    const root = mount(kind, { lang: "vi", langs: ["en", "vi"] });
    const group = root.querySelector(".auth-lang [role=group]");
    assert.ok(group, "language group");
    assert.equal(group.getAttribute("aria-label"), en["admin.me.language"]);
    const keys = [...group.querySelectorAll("button")];
    assert.deepEqual(keys.map((b) => b.tagName + b.getAttribute("type")), ["BUTTONbutton", "BUTTONbutton"]);
    assert.deepEqual(keys.map((b) => b.getAttribute("aria-pressed")), ["false", "true"]);
  });
}

test("picking the other language sets the cookie and reloads; the current one does nothing", () => {
  const root = mount("setup", { lang: "en", langs: ["en", "vi"] });
  const [enKey, viKey] = root.querySelectorAll(".auth-lang button");
  enKey.click();
  assert.equal(reloads, 0);
  viKey.click();
  assert.equal(reloads, 1);
  assert.match(document.cookie, /pb_lang=vi/);
});

test("one language: no switch", () => {
  const root = mount("signin", { lang: "en", langs: ["en"] });
  assert.equal(root.querySelector(".auth-lang"), null);
});

// Each mount gets a new api object, so the params cache (keyed per api) never carries over.
const settle = () => new Promise((r) => setTimeout(r, 10));

test("sign-in on a site with no owner yet goes to setup", async () => {
  moved.length = 0;
  mount("signin", { lang: "en", langs: ["en", "vi"] }, { ...PARAMS, setup_required: true });
  await settle();
  assert.deepEqual(moved, ["/admin/setup"]);
});

test("sign-in stays when the site has an owner, or the Worker predates the field", async () => {
  moved.length = 0;
  mount("signin", { lang: "en", langs: ["en", "vi"] }, { ...PARAMS, setup_required: false });
  mount("signin", { lang: "en", langs: ["en", "vi"] }, PARAMS);
  await settle();
  assert.deepEqual(moved, []);
});

test("setup, invite and reset never move on setup_required", async () => {
  moved.length = 0;
  for (const kind of ["setup", "invite", "reset"]) mount(kind, { lang: "en", langs: ["en", "vi"] }, { ...PARAMS, setup_required: true });
  await settle();
  assert.deepEqual(moved, []);
});
