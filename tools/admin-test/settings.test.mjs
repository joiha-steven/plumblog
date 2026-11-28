// The Settings screen (web/admin/src/settings.ts) in happy-dom against a fake Api: values shown,
// only changed keys sent, emptied fields reset to the default, client and server errors on the
// right field, and the Save button's states.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { build } from "esbuild";
import { Window } from "happy-dom";

const win = new Window({ url: "http://localhost/admin/settings" });
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
    contents: `export { mountSettings, settingsNav } from ${JSON.stringify(src("settings.ts"))};
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
const STORED = { site_title: "S\u1ed5 tay c\u1ee7a Lan", site_description: "Ghi ch\u00e9p v\u1ec1 n\u1ea5u \u0103n", site_lang: "vi", site_per_page: 12 };

/** `hold` makes writes wait for `release()`; `fail[key]` answers that key's write with an error. */
async function mount({ settings = STORED, offline: off = false, loadFails = false, fail = {}, hold = false } = {}) {
  m.initI18n({ nonce: "", site: "S", lang: "en", langs: ["en", "vi"] }, { messages: en, fallback: en });
  let offline = off;
  const calls = [];
  let release;
  const gate = new Promise((r) => (release = r));
  let loads = 0;
  const api = {
    call: async (method, path, body) => {
      calls.push({ method, path, body });
      if (method === "GET") return ++loads === 1 && loadFails ? { ok: false, error: { code: "network", status: 0 } } : { ok: true, value: { settings } };
      if (hold) await gate;
      const key = path.split("/").pop();
      return fail[key] ? { ok: false, error: fail[key] } : { ok: true, value: undefined };
    },
  };
  const box = document.createElement("div");
  document.body.append(box);
  m.mountSettings(box, api, { role: "owner", offline: () => offline });
  await tick();
  await tick();
  const $ = (s) => box.querySelector(s);
  const type = (id, v) => {
    const el = $(`#set-${id}`);
    el.value = v;
    el.dispatchEvent(new win.Event(el.tagName === "SELECT" ? "change" : "input", { bubbles: true }));
  };
  const save = () => $("button[type=submit]");
  const submit = async () => {
    $("form").dispatchEvent(new win.Event("submit", { bubbles: true, cancelable: true }));
    await tick();
    await tick();
  };
  return { box, $, go: (v) => { offline = v; win.dispatchEvent(new win.Event(v ? "offline" : "online")); }, type, save, submit, calls, release, writes: () => calls.filter((c) => c.method !== "GET") };
}

test("shows the stored values, and the defaults for keys that are absent", async () => {
  const s = await mount();
  assert.equal(s.$("#set-site_title").value, "S\u1ed5 tay c\u1ee7a Lan");
  assert.equal(s.$("#set-site_description").value, "Ghi ch\u00e9p v\u1ec1 n\u1ea5u \u0103n");
  assert.equal(s.$("#set-site_footer").value, "");
  assert.equal(s.$("#set-site_lang").value, "vi");
  assert.equal(s.$("#set-site_per_page").value, "12");
  assert.ok(s.$("#set-site_title").getAttribute("placeholder"));
  assert.equal(s.$("label[for=set-site_title]").textContent, "Site title");
  assert.equal(s.save().disabled, true, "nothing changed yet");
  assert.equal(s.$(".subnav a[aria-current=page]").textContent, "General");
});

test("sends only the changed keys, one write each", async () => {
  const s = await mount();
  s.type("site_title", "  B\u1ebfp nh\u00e0 Lan ");
  assert.equal(s.save().disabled, false);
  await s.submit();
  assert.deepEqual(s.writes(), [{ method: "PUT", path: "/api/v1/settings/site_title", body: { value: "B\u1ebfp nh\u00e0 Lan" } }]);
  assert.match(s.$("[role=status]").textContent, /Saved/);
  assert.equal(s.save().disabled, true, "the saved values are the new baseline");
});

test("an emptied field deletes the key; per page is sent as a number", async () => {
  const s = await mount();
  s.type("site_description", "");
  s.type("site_per_page", "20");
  s.type("site_footer", "");
  await s.submit();
  assert.deepEqual(s.writes(), [
    { method: "DELETE", path: "/api/v1/settings/site_description", body: undefined },
    { method: "PUT", path: "/api/v1/settings/site_per_page", body: { value: 20 } },
  ]);
});

test("per page outside 1 to 50 is refused before anything is sent", async () => {
  for (const bad of ["0", "51", "2.5", "-3"]) {
    const s = await mount();
    s.type("site_per_page", bad);
    await s.submit();
    assert.equal(s.writes().length, 0, bad);
    const input = s.$("#set-site_per_page");
    assert.equal(input.getAttribute("aria-invalid"), "true", bad);
    const err = s.$(`#${input.getAttribute("aria-describedby").split(" ").find((i) => i.endsWith("-err"))}`);
    assert.match(err.textContent, /1 to 50/, bad);
    s.type("site_per_page", "30");
    assert.equal(input.hasAttribute("aria-invalid"), false, "typing clears the mark");
  }
});

test("Save is off while saving", async () => {
  const s = await mount({ hold: true });
  s.type("site_title", "X");
  const done = s.submit();
  await tick();
  assert.equal(s.save().disabled, true);
  s.release();
  await done;
  await tick();
  assert.equal(s.writes().length, 1);
  assert.match(s.$("[role=status]").textContent, /Saved/);
});

test("a server error lands on its own field; the other changes still save", async () => {
  const s = await mount({ fail: { site_per_page: { code: "input.invalid", status: 400, field: "value" } } });
  s.type("site_title", "M\u1edbi");
  s.type("site_per_page", "30");
  await s.submit();
  assert.equal(s.writes().length, 2);
  assert.equal(s.$("#set-site_per_page").getAttribute("aria-invalid"), "true");
  assert.ok(s.$("#set-site_per_page-err").textContent.length > 0);
  assert.equal(s.$("#set-site_title").hasAttribute("aria-invalid"), false);
  assert.equal(s.save().disabled, false, "the failed key is still unsaved");
  await s.submit();
  assert.deepEqual(s.writes().slice(2).map((c) => c.path), ["/api/v1/settings/site_per_page"], "only the failed one is retried");
});

test("offline: values stay readable, writing is off, and the page says so", async () => {
  const s = await mount({ offline: true });
  assert.equal(s.$("#set-site_title").value, "S\u1ed5 tay c\u1ee7a Lan");
  s.type("site_title", "X");
  assert.equal(s.save().disabled, true);
  assert.match(s.$("[role=status]").textContent, /offline/i);
});

test("a failed load offers a retry", async () => {
  const s = await mount({ loadFails: true });
  assert.ok(s.$("[role=alert]"));
  s.$("[role=alert] button").dispatchEvent(new win.Event("click", { bubbles: true }));
  await tick();
  await tick();
  assert.equal(s.$("#set-site_title").value, "S\u1ed5 tay c\u1ee7a Lan");
});

test("going offline and back while open switches Save and the message", async () => {
  const s = await mount();
  s.type("site_title", "X");
  assert.equal(s.save().disabled, false);
  s.go(true);
  assert.equal(s.save().disabled, true);
  assert.match(s.$("[role=status]").textContent, /offline/i);
  s.go(false);
  assert.equal(s.save().disabled, false);
  assert.equal(s.$("[role=status]").textContent, "");
});

test("offline at load: fields stay editable and come back to life when online", async () => {
  const s = await mount({ offline: true });
  assert.equal(s.$("#set-site_title").hasAttribute("readonly"), false);
  assert.equal(s.$("#set-site_lang").disabled, false);
  s.type("site_title", "X");
  s.go(false);
  assert.equal(s.save().disabled, false);
});

test("editing after a save clears the Saved message", async () => {
  const s = await mount();
  s.type("site_title", "A");
  await s.submit();
  assert.match(s.$("[role=status]").textContent, /Saved/);
  s.type("site_title", "B");
  assert.equal(s.$("[role=status]").textContent, "");
});

test("the section list is reusable: another screen can draw it with its own entry current", async () => {
  m.initI18n({ nonce: "", site: "S", lang: "en", langs: ["en", "vi"] }, { messages: en, fallback: en });
  const nav = m.settingsNav("owner", "general");
  assert.equal(nav.getAttribute("aria-label"), "Settings sections");
  assert.equal(nav.querySelector("a[aria-current=page]").getAttribute("href"), "/admin/settings");
  assert.equal(m.settingsNav("writer", "general").querySelectorAll("a").length, 0, "owners only");
});
