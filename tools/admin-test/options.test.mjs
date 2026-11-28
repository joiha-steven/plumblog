// Settings > Appearance and Plugins (web/admin/src/options/) in happy-dom against a fake Api:
// each field type with its limits, only changed keys sent with typed values, per-field and
// whole-form reset, server errors on the field, offline, plugin switches, and the empty and
// unavailable states.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { build } from "esbuild";
import { Window } from "happy-dom";

const win = new Window({ url: "http://localhost/admin/settings/appearance" });
for (const k of ["document", "Node", "HTMLElement", "addEventListener", "removeEventListener"]) {
  const v = typeof win[k] === "function" && !/^[A-Z]/.test(k) ? win[k].bind(win) : win[k];
  Object.defineProperty(globalThis, k, { value: v, configurable: true, writable: true });
}
Object.defineProperty(globalThis, "window", { value: win, configurable: true, writable: true });

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
    contents: `export { mountSettings } from ${JSON.stringify(src("settings.ts"))};
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

const tick = () => new Promise((r) => setTimeout(r, 0));
const settle = async () => {
  for (let i = 0; i < 8; i++) await tick();
};

const opt = (key, type, extra = {}) => ({
  key,
  label: `L ${key}`,
  help: null,
  type,
  label_key: `options.${key}`,
  help_key: null,
  group: null,
  icon: null,
  token: null,
  min: null,
  max: null,
  step: null,
  default: null,
  choices: [],
  max_len: null,
  max_items: null,
  ...extra,
});
const choices = (...v) => v.map((value) => ({ label: `C ${value}`, value, label_key: `options.${value}` }));
const DECLS = (name, options, groups = []) => ({ name, version: "1.0.0", lang: "en", groups, options });
const THEME = DECLS(
  "minimal",
  [
    opt("title", "short_text", { default: "Hello", max_len: 60, group: "look" }),
    opt("bio", "long_text", { default: "", max_len: 300, group: "look" }),
    opt("size", "number", { default: 55, min: 0, max: 120, step: 5, group: "reading" }),
    opt("dense", "toggle", { default: true, group: "reading" }),
    opt("style", "choice", { default: "a", choices: choices("a", "b", "c"), group: "look", help: "Pick one" }),
    opt("menu", "choice", { default: "a", choices: choices("a", "b", "c", "d") }),
    opt("accent", "color", { default: { light: "#8a2d6f", dark: "#e2a3d2" }, group: "look" }),
    opt("logo", "image", { default: "" }),
    opt("links", "list", { default: ["x"], max_len: 10, max_items: 3 }),
  ],
  [
    { key: "look", label: "Look", label_key: "g.look", icon: null },
    { key: "reading", label: "Reading", label_key: "g.reading", icon: null },
  ],
);
const ACTIVE = { name: "minimal", version: "0.2.0", active: true, source: "embedded", available: true };

/** `routes` maps "METHOD path" to a result; GETs of the lists default to the fixtures above. */
async function mount(section, { themes = [ACTIVE], decls = THEME, stored = {}, plugins = [], pluginDecls = {}, pluginStored = {}, offline: off = false, fail = {}, hold = false } = {}) {
  m.initI18n({ nonce: "", site: "S", lang: "en", langs: ["en", "vi"] }, { messages: en, fallback: en });
  let offline = off;
  const calls = [];
  let release;
  const gate = new Promise((r) => (release = r));
  const ok = (value) => ({ ok: true, value });
  const api = {
    call: async (method, path, body) => {
      calls.push({ method, path, body });
      const key = `${method} ${path.split("?")[0]}`;
      if (fail[key]) return { ok: false, error: fail[key] };
      if (method === "GET") {
        if (path === "/api/v1/themes") return ok({ themes });
        if (path === "/api/v1/plugins") return ok({ plugins });
        let hit = /^\/api\/v1\/themes\/([^/]+)\/declarations/.exec(path);
        if (hit) return ok(decls);
        if (/^\/api\/v1\/themes\/[^/]+\/options$/.test(path)) return ok({ options: stored });
        hit = /^\/api\/v1\/plugins\/([^/]+)\/declarations/.exec(path);
        if (hit) return pluginDecls[hit[1]] ? ok(pluginDecls[hit[1]]) : { ok: false, error: { code: "store.not_found", status: 404 } };
        hit = /^\/api\/v1\/plugins\/([^/]+)\/options$/.exec(path);
        if (hit) return ok({ options: pluginStored[hit[1]] ?? {} });
      }
      if (hold) await gate;
      return ok(undefined);
    },
  };
  const box = document.createElement("div");
  document.body.append(box);
  m.mountSettings(box, api, { role: "owner", offline: () => offline, section });
  await settle();
  const $ = (s) => box.querySelector(s);
  const type = (sel, v) => {
    const el = $(sel);
    el.value = v;
    el.dispatchEvent(new win.Event(el.tagName === "SELECT" ? "change" : "input", { bubbles: true }));
  };
  const flip = (sel) => {
    const el = $(sel);
    el.checked = !el.checked;
    el.dispatchEvent(new win.Event("change", { bubbles: true }));
  };
  const click = (el) => el.dispatchEvent(new win.MouseEvent("click", { bubbles: true, cancelable: true }));
  const save = () => $("button[type=submit]");
  const submit = async () => {
    $("form").dispatchEvent(new win.Event("submit", { bubbles: true, cancelable: true }));
    await settle();
  };
  const writes = () => calls.filter((c) => c.method !== "GET");
  return { box, $, type, flip, click, save, submit, calls, writes, release, go: (v) => { offline = v; win.dispatchEvent(new win.Event(v ? "offline" : "online")); } };
}

test("Appearance: each field type renders with its limits", async () => {
  const s = await mount("appearance");
  assert.match(s.$("header.sec").textContent, /Appearance/);
  assert.match(s.$("header.sec").textContent, /Theme minimal, version 0\.2\.0/);
  const title = s.$("#app-title");
  assert.equal(title.getAttribute("maxlength"), "60");
  assert.equal(title.getAttribute("placeholder"), "Hello");
  assert.equal(s.$("label[for=app-title]").textContent, "L title");
  assert.equal(s.$("#app-bio").tagName, "TEXTAREA");
  assert.equal(s.$("#app-bio").getAttribute("maxlength"), "300");
  const size = s.$("#app-size");
  assert.deepEqual([size.type, size.getAttribute("min"), size.getAttribute("max"), size.getAttribute("step")], ["number", "0", "120", "5"]);
  assert.equal(size.value, "55", "an absent key shows its default");
  const dense = s.$("#app-dense");
  assert.deepEqual([dense.type, dense.getAttribute("role"), dense.checked], ["checkbox", "switch", true]);
  assert.equal(s.$("#app-style").querySelectorAll("button").length, 3, "up to three choices are a segmented control");
  assert.equal(s.$("#app-menu").tagName, "SELECT", "more choices are a menu");
  assert.equal(s.$("#app-menu").querySelectorAll("option").length, 4);
  assert.equal(s.$("#app-accent-light").value, "#8a2d6f");
  assert.equal(s.$("#app-accent-dark").value, "#e2a3d2");
  assert.equal(s.box.querySelectorAll("#app-accent input[type=color]").length, 2);
  assert.equal(s.$("#app-logo").type, "number");
  assert.equal(s.$("#app-links").querySelectorAll("input").length, 1);
  assert.match(s.$("#app-links").parentElement.textContent, /Up to 3 items/);
  assert.equal(s.save().disabled, true, "nothing changed yet");
});

test("Appearance: groups are sections with headings, in the declared order; help and default are described", async () => {
  const s = await mount("appearance");
  const heads = [...s.box.querySelectorAll("form h3.grp")].map((x) => x.textContent);
  assert.deepEqual(heads, ["Look", "Reading"]);
  const secs = [...s.box.querySelectorAll("form section")];
  assert.equal(secs[1].getAttribute("aria-labelledby"), s.$("form h3.grp").id);
  assert.equal(secs[0].getAttribute("aria-labelledby"), null);
  // Options without a group come first, in their own unlabelled section.
  assert.ok(s.$("form section:first-child").querySelector("#app-menu"));
  assert.deepEqual([...secs[1].querySelectorAll("label[for]")].map((x) => x.getAttribute("for")).slice(0, 2), ["app-title", "app-bio"]);
  assert.match(s.$("#app-style").getAttribute("aria-describedby"), /app-style-hint/);
  assert.match(s.$("#app-style-def").textContent, /Default: C a/);
  assert.match(s.$("#app-size-def").textContent, /Default: 55/);
  assert.match(s.$("#app-dense-def").textContent, /Default: On/);
});

test("Appearance: only changed keys are sent, with typed values", async () => {
  const s = await mount("appearance", { stored: { size: 20 } });
  assert.equal(s.$("#app-size").value, "20");
  s.type("#app-size", "35");
  s.flip("#app-dense");
  s.type("#app-title", "  Hi there ");
  s.click(s.$("#app-style").querySelectorAll("button")[1]);
  s.type("#app-accent-light", "#112233");
  s.type("#app-logo", "7");
  s.click(s.$("#app-links").querySelector(".btn"));
  s.box.querySelectorAll("#app-links input")[1].value = "second";
  s.box.querySelectorAll("#app-links input")[1].dispatchEvent(new win.Event("input", { bubbles: true }));
  assert.equal(s.save().disabled, false);
  await s.submit();
  const w = s.writes();
  const sent = Object.fromEntries(w.map((c) => [c.path.split("/").pop(), c.body.value]));
  assert.deepEqual(sent, { accent: { light: "#112233", dark: "#e2a3d2" }, dense: false, links: ["x", "second"], logo: 7, size: 35, style: "b", title: "Hi there" });
  assert.ok(w.every((c) => c.method === "PUT" && c.path.startsWith("/api/v1/themes/minimal/options/")));
  assert.equal(typeof w.find((c) => c.path.endsWith("/size")).body.value, "number");
  assert.match(s.$("[role=status]").textContent, /Saved/);
  assert.equal(s.save().disabled, true, "saved: nothing left to send");
  await s.submit();
  assert.equal(s.writes().length, 7, "a second save sends nothing");
});

test("Appearance: a value the server would refuse stays on the page, with the message on its field", async () => {
  const s = await mount("appearance");
  s.type("#app-size", "500");
  await s.submit();
  assert.equal(s.writes().length, 0);
  assert.equal(s.$("#app-size").getAttribute("aria-invalid"), "true");
  assert.match(s.$("#app-size").getAttribute("aria-describedby"), /app-size-err/);
  assert.match(s.$("#app-size-err").textContent, /from 0 to 120/);
  s.type("#app-size", "7");
  assert.equal(s.$("#app-size-err").hidden, true, "editing clears the message");
  await s.submit();
  assert.match(s.$("#app-size-err").textContent, /steps of 5/);
  s.type("#app-size", "");
  await s.submit();
  assert.match(s.$("#app-size-err").textContent, /Enter a number/);
  s.type("#app-size", "55");
  s.type("#app-accent-dark", "red");
  await s.submit();
  assert.match(s.$("#app-accent-err").textContent, /color code/);
  assert.equal(s.writes().length, 0);
});

test("Appearance: a server error lands on its field, the other keys still save", async () => {
  const s = await mount("appearance", { fail: { "PUT /api/v1/themes/minimal/options/title": { code: "input.invalid", status: 400 } } });
  s.type("#app-title", "x");
  s.flip("#app-dense");
  await s.submit();
  assert.equal(s.writes().length, 2);
  assert.equal(s.$("#app-title-err").hidden, false);
  assert.equal(s.$("#app-title").getAttribute("aria-invalid"), "true");
  assert.match(s.$("#app-title").getAttribute("aria-describedby"), /app-title-err/);
  assert.equal(s.$("#app-dense-err").hidden, true);
  assert.match(s.$("[role=status]").textContent, /not saved/);
  assert.equal(s.save().disabled, false, "the failed key can be sent again");
});

test("Appearance: per-field reset writes the declared default", async () => {
  const s = await mount("appearance", { stored: { size: 20, accent: { dark: "#000000", light: "#ffffff" } } });
  const reset = (id) => s.$(`#app-${id}`).closest(".field").querySelector("button.linkbtn");
  assert.equal(reset("title").disabled, true, "already the default");
  s.click(reset("size"));
  await settle();
  assert.deepEqual(s.writes().map((c) => [c.path, c.body]), [["/api/v1/themes/minimal/options/size", { value: 55 }]]);
  assert.equal(s.$("#app-size").value, "55");
  assert.equal(reset("size").disabled, true);
  s.click(s.box.querySelector("#app-accent").closest(".field").querySelector("button.linkbtn"));
  await settle();
  assert.deepEqual(s.writes()[1].body, { value: { light: "#8a2d6f", dark: "#e2a3d2" } });
  assert.match(reset("size").getAttribute("aria-label"), /Reset L size to its default/);
});

test("Appearance: reset all asks first, then deletes every stored option", async () => {
  const s = await mount("appearance", { stored: { size: 20, title: "Mine" } });
  const ask = [...s.box.querySelectorAll("form button")].find((b) => b.textContent === "Reset all to defaults");
  s.click(ask);
  assert.equal(s.writes().length, 0, "nothing is sent before the answer");
  const q = s.$("[role=group][aria-label='Reset all to defaults']");
  assert.equal(q.hidden, false);
  const buttons = [...q.querySelectorAll("button")];
  s.click(buttons[1]);
  assert.equal(s.writes().length, 0, "cancel sends nothing");
  s.click(ask);
  s.click(s.$("[role=group][aria-label='Reset all to defaults']").querySelector("button.danger"));
  await settle();
  assert.deepEqual(s.writes().map((c) => [c.method, c.path]), [["DELETE", "/api/v1/themes/minimal/options"]]);
  assert.equal(s.$("#app-size").value, "55");
  assert.equal(s.$("#app-title").value, "Hello");
  assert.match(s.$("[role=status]").textContent, /back to its default/);
});

test("Appearance: offline turns Save off with a message, and back on when the network returns", async () => {
  const s = await mount("appearance", { offline: true });
  assert.match(s.$("[role=status]").textContent, /offline/i);
  s.type("#app-title", "x");
  assert.equal(s.save().disabled, true);
  s.go(false);
  assert.equal(s.$("[role=status]").textContent, "");
  assert.equal(s.save().disabled, false);
  s.go(true);
  assert.equal(s.save().disabled, true);
  assert.match(s.$("[role=status]").textContent, /offline/i);
});

test("Appearance: a list takes up to max_items and drops blank lines", async () => {
  const s = await mount("appearance", { stored: { links: ["a", "b"] } });
  const add = () => s.$("#app-links").querySelector(".btn");
  assert.equal(s.box.querySelectorAll("#app-links input")[0].getAttribute("maxlength"), "10");
  s.click(add());
  assert.equal(add().disabled, true, "three is the limit");
  assert.equal(s.box.querySelectorAll("#app-links input")[2].getAttribute("aria-label"), "Item 3");
  s.click(s.$("#app-links").querySelector("button.ib"));
  assert.equal(add().disabled, false);
  assert.equal(s.box.querySelectorAll("#app-links input").length, 2);
  s.box.querySelectorAll("#app-links input")[1].value = "  ";
  s.box.querySelectorAll("#app-links input")[1].dispatchEvent(new win.Event("input", { bubbles: true }));
  await s.submit();
  assert.deepEqual(s.writes()[0].body, { value: ["b"] });
});

test("Appearance: a theme with no options, a failed load and a retry", async () => {
  const none = await mount("appearance", { decls: DECLS("minimal", []) });
  assert.match(none.box.textContent, /This theme has no options/);
  assert.equal(none.$("form"), null);

  let fails = true;
  m.initI18n({ nonce: "", site: "S", lang: "en", langs: ["en"] }, { messages: en, fallback: en });
  const calls = [];
  const api = {
    call: async (method, path) => {
      calls.push(path);
      if (fails) return { ok: false, error: { code: "network", status: 0 } };
      if (path === "/api/v1/themes") return { ok: true, value: { themes: [ACTIVE] } };
      if (path.includes("declarations")) return { ok: true, value: THEME };
      return { ok: true, value: { options: {} } };
    },
  };
  const box = document.createElement("div");
  document.body.append(box);
  m.mountSettings(box, api, { role: "owner", offline: () => false, section: "appearance" });
  await settle();
  assert.ok(box.querySelector("[role=alert]"));
  fails = false;
  [...box.querySelectorAll("button")].find((b) => b.textContent === "Try again" || b.className.includes("btn")).click();
  await settle();
  assert.ok(box.querySelector("form"), "retry draws the form");
});

test("Appearance: a bound theme that did not load is said plainly, with no form", async () => {
  const bad = { name: "journal", version: null, active: false, source: "pushed", available: false };
  const a = await mount("appearance", { themes: [ACTIVE, bad] });
  assert.match(a.$("[role=alert]").textContent, /The theme journal did not load/);
  assert.equal(a.$("form"), null);
  const b = await mount("appearance", { fail: { "GET /api/v1/themes/minimal/declarations": { code: "theme.unavailable", status: 409 } } });
  assert.match(b.$("[role=alert]").textContent, /The theme minimal did not load/);
  assert.equal(b.$("form"), null);
});

test("Settings list shows Appearance and Plugins for the owner only", async () => {
  const s = await mount("appearance");
  assert.deepEqual([...s.box.querySelectorAll(".subnav a")].map((a) => a.textContent), ["General", "People", "Appearance", "Plugins", "System log"]);
  assert.equal(s.$(".subnav a[aria-current=page]").textContent, "Appearance");
  const box = document.createElement("div");
  m.mountSettings(box, { call: async () => ({ ok: false, error: { code: "network", status: 0 } }) }, { role: "writer", offline: () => false });
  assert.deepEqual([...box.querySelectorAll(".subnav a")].map((a) => a.textContent), []);
});

const PLUGINS = [
  { name: "news", enabled: true, disabled_reason: null, updated_at: 1 },
  { name: "bar", enabled: false, disabled_reason: "it failed", updated_at: 2 },
  { name: "quiet", enabled: false, disabled_reason: null, updated_at: 3 },
];
const PDECLS = {
  news: DECLS("news", [opt("heading", "short_text", { default: "Join", max_len: 40 })]),
  bar: DECLS("bar", [opt("thin", "toggle", { default: false })]),
  quiet: DECLS("quiet", []),
};

test("Plugins: each plugin has a switch; a disabled reason is told in plain words", async () => {
  const s = await mount("plugins", { plugins: PLUGINS, pluginDecls: PDECLS });
  const rows = [...s.box.querySelectorAll("section.sec label.opt")];
  assert.equal(rows.length, 3);
  assert.deepEqual(rows.map((r) => r.querySelector("input").checked), [true, false, false]);
  assert.match(rows[0].textContent, /news.*On/);
  assert.match(rows[1].textContent, /Turned off automatically: it failed/);
  assert.match(rows[2].textContent, /quiet.*Off/);
  assert.equal(rows[0].querySelector("input").getAttribute("role"), "switch");
});

test("Plugins: switching sends PUT, clears the reason, and a refused switch goes back with a message", async () => {
  const s = await mount("plugins", { plugins: PLUGINS, pluginDecls: PDECLS, fail: { "PUT /api/v1/plugins/quiet": { code: "internal", status: 500 } } });
  const sw = (i) => s.box.querySelectorAll("section.sec label.opt input")[i];
  sw(1).checked = true;
  sw(1).dispatchEvent(new win.Event("change", { bubbles: true }));
  await settle();
  assert.deepEqual(s.writes().map((c) => [c.method, c.path, c.body]), [["PUT", "/api/v1/plugins/bar", { enabled: true }]]);
  const row = s.box.querySelectorAll("section.sec label.opt")[1];
  assert.doesNotMatch(row.textContent, /failed/);
  assert.match(row.textContent, /On/);
  sw(2).checked = true;
  sw(2).dispatchEvent(new win.Event("change", { bubbles: true }));
  await settle();
  assert.equal(sw(2).checked, false, "reverted");
  assert.match(s.box.querySelectorAll("section.sec .err")[2].textContent, /quiet was not changed/);
  s.go(true);
  assert.equal(sw(0).disabled, true, "no switching offline");
});

test("Plugins: one tab per plugin, each with its own form and paths", async () => {
  const s = await mount("plugins", { plugins: PLUGINS, pluginDecls: PDECLS, pluginStored: { news: { heading: "Hello" } } });
  const tabs = [...s.$(".seg").querySelectorAll("button")];
  assert.deepEqual(tabs.map((b) => b.textContent), ["news", "bar", "quiet"]);
  assert.equal(s.$("#plg-news-heading").value, "Hello");
  assert.equal(s.$("[role=group][aria-label='Options of news']") !== null, true);
  s.type("#plg-news-heading", "Changed");
  s.click(tabs[1]);
  await settle();
  assert.equal(s.$("#plg-bar-thin").type, "checkbox");
  assert.equal(s.$("#plg-news-heading"), null);
  s.click(tabs[0]);
  assert.equal(s.$("#plg-news-heading").value, "Changed", "edits survive a tab change");
  await s.submit();
  assert.deepEqual(s.writes().map((c) => [c.path, c.body]), [["/api/v1/plugins/news/options/heading", { value: "Changed" }]]);
  s.click(tabs[2]);
  await settle();
  assert.match(s.box.textContent, /This plugin has no options/);
});

test("Plugins: reset all of a plugin deletes under the plugin's path; a missing plugin says so", async () => {
  const s = await mount("plugins", { plugins: PLUGINS, pluginDecls: { news: PDECLS.news }, pluginStored: { news: { heading: "Hello" } } });
  s.click([...s.box.querySelectorAll("form button")].find((b) => b.textContent === "Reset all to defaults"));
  s.click(s.$("[role=group][aria-label='Reset all to defaults']").querySelector("button.danger"));
  await settle();
  assert.deepEqual(s.writes().map((c) => [c.method, c.path]), [["DELETE", "/api/v1/plugins/news/options"]]);
  s.click([...s.$(".seg").querySelectorAll("button")][1]);
  await settle();
  assert.match(s.box.textContent, /not installed on the site/);
});

test("Plugins: no plugins is an empty state, a failed list can be retried", async () => {
  const s = await mount("plugins", { plugins: [] });
  assert.match(s.box.textContent, /No plugins installed/);
  assert.equal(s.$(".seg"), null);
  const f = await mount("plugins", { fail: { "GET /api/v1/plugins": { code: "internal", status: 500 } } });
  assert.ok(f.$("[role=alert]"));
});
