// The Users section of Settings (web/admin/src/users.ts) in happy-dom against a fake Api: people
// list, role/disable/delete with confirms, the server's refusals in plain words, invites (create,
// link shown once, revoke), offline and paging.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { build } from "esbuild";
import { Window } from "happy-dom";

const win = new Window({ url: "http://localhost/admin/settings/users" });
for (const k of ["document", "Node", "HTMLElement", "addEventListener", "removeEventListener"]) {
  const v = typeof win[k] === "function" && !/^[A-Z]/.test(k) ? win[k].bind(win) : win[k];
  Object.defineProperty(globalThis, k, { value: v, configurable: true, writable: true });
}
Object.defineProperty(globalThis, "window", { value: win, configurable: true, writable: true });
Object.defineProperty(globalThis, "location", { value: win.location, configurable: true, writable: true });
let copied;
Object.defineProperty(globalThis, "navigator", {
  value: { onLine: true, clipboard: { writeText: async (s) => void (copied = s) } },
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
const T = (k, vars = {}) => en[`admin.users.${k}`].replace(/\{(\w+)\}/g, (_, n) => vars[n]);

const root = new URL("../../", import.meta.url);
const src = (f) => new URL(`web/admin/src/${f}`, root).pathname;
const out = await build({
  stdin: {
    contents: `export { mountUsers } from ${JSON.stringify(src("users.ts"))};
      export { mountSettings } from ${JSON.stringify(src("settings.ts"))};
      export { initI18n } from ${JSON.stringify(src("i18n.ts"))};
      export { closePop } from ${JSON.stringify(src("ui.ts"))};`,
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
  for (let i = 0; i < 6; i++) await tick();
};
const DAY = 86_400_000;
const user = (id, login, name, role = "writer", extra = {}) => ({ id, login, slug: login, display_name: name, bio: "", role, disabled_at: null, created_at: Date.UTC(2026, 0, id), updated_at: 0, ...extra });
const USERS = () => [user(1, "lan", "Lan Nguy\u1ec5n", "owner"), user(2, "minh", "Minh Tr\u1ea7n"), user(3, "bao", "B\u1ea3o Ph\u1ea1m", "writer", { disabled_at: 5 })];
const invite = (id, role, expires_at) => ({ id, role, created_by: 1, created_at: Date.now() - DAY, expires_at });
// Newest first: the live writer invite is 2, the expired owner invite 1.
const INVITES = () => [invite(2, "writer", Date.now() + 3 * DAY), invite(1, "owner", Date.now() - DAY)];

/** `refuse["METHOD /path"]` answers that call with an error; `pages` serves users in pages of that size. */
function setup({ users = USERS(), invites: seeded = INVITES(), offline = false, refuse = {}, hold = false, pages } = {}) {
  m.initI18n({ nonce: "", site: "S", lang: "en", langs: ["en", "vi"] }, { messages: en, fallback: en });
  m.closePop(false);
  let off = offline;
  let invites = seeded;
  const calls = [];
  let release;
  const gate = new Promise((r) => (release = r));
  const api = {
    call: async (method, path, body) => {
      calls.push({ method, path, body });
      const key = `${method} ${path}`;
      if (refuse[key]) return { ok: false, error: refuse[key] };
      if (method !== "GET" && hold) await gate;
      let r;
      if ((r = /^GET \/api\/v1\/users(?:\?cursor=(\d+))?$/.exec(key))) {
        const from = Number(r[1] ?? 0);
        const size = pages ?? users.length;
        return { ok: true, value: { users: users.slice(from, from + size), next_cursor: from + size < users.length ? String(from + size) : null } };
      }
      if (key === "GET /api/v1/invites") return { ok: true, value: { invites } };
      if ((r = /^PUT \/api\/v1\/users\/(\d+)\/role$/.exec(key))) return { ok: true, value: { ...users.find((u) => u.id === Number(r[1])), role: body.role } };
      if ((r = /^PUT \/api\/v1\/users\/(\d+)\/disabled$/.exec(key))) return { ok: true, value: { ...users.find((u) => u.id === Number(r[1])), disabled_at: body.disabled ? 99 : null } };
      if ((r = /^DELETE \/api\/v1\/invites\/(\d+)$/.exec(key))) invites = invites.filter((i) => i.id !== Number(r[1]));
      if (method === "DELETE") return { ok: true, value: undefined };
      if (key === "POST /api/v1/invites") return { ok: true, value: { ...invite(9, body.role, body.expires_at ?? 0), code: "CODE-ABC_123" } };
      throw new Error(`unexpected ${key}`);
    },
  };
  const box = document.createElement("div");
  document.body.append(box);
  m.mountUsers(box, api, { me: 1, offline: () => off });
  return {
    box,
    calls,
    release,
    api,
    writes: () => calls.filter((c) => c.method !== "GET"),
    go: (v) => {
      off = v;
      win.dispatchEvent(new win.Event(v ? "offline" : "online"));
    },
  };
}
const $ = (el, s) => el.querySelector(s);
const row = (box, login) => [...box.querySelectorAll(".usr")].find((r) => $(r, ".usr-login").textContent === login);
const btn = (root, text) => [...root.querySelectorAll("button")].find((b) => b.textContent === text);
const pop = () => document.querySelector(".pop");
const choose = (select, v) => {
  select.value = v;
  select.dispatchEvent(new win.Event("change", { bubbles: true }));
};
const ready = async (o) => {
  const s = setup(o);
  await settle();
  return s;
};

test("lists people with login, role, state and joined date; the signed-in one is marked you", async () => {
  const { box } = await ready();
  assert.equal(box.querySelectorAll(".usr").length, 3);
  const lan = row(box, "lan");
  assert.match($(lan, ".pk-name").textContent, /Lan Nguy\u1ec5n/);
  assert.match($(lan, ".pk-name").textContent, new RegExp(T("you")));
  assert.equal($(row(box, "minh"), ".pk-name").textContent.includes(T("you")), false);
  assert.equal($(lan, "select").value, "owner");
  assert.equal($(row(box, "minh"), "select").value, "writer");
  assert.match(row(box, "bao").textContent, new RegExp(T("state_disabled")));
  assert.match(row(box, "minh").textContent, new RegExp(T("state_active")));
  assert.match(lan.textContent, /2026/);
  assert.ok(btn(row(box, "bao"), T("enable")) && btn(row(box, "minh"), T("disable")));
  assert.equal($(lan, "select").getAttribute("aria-label"), T("role_of", { name: "Lan Nguy\u1ec5n" }));
});

test("a role change sends PUT and updates the row; promoting leaves invites alone", async () => {
  const s = await ready();
  choose($(row(s.box, "minh"), "select"), "owner");
  await settle();
  assert.deepEqual(s.writes(), [{ method: "PUT", path: "/api/v1/users/2/role", body: { role: "owner" } }]);
  assert.equal($(row(s.box, "minh"), "select").value, "owner");
  assert.equal(s.calls.filter((c) => c.path === "/api/v1/invites").length, 1);
});

test("demoting an owner reloads the invites list (the server drops their unused invites)", async () => {
  const users = [user(1, "lan", "Lan", "owner"), user(2, "minh", "Minh", "owner")];
  const s = await ready({ users });
  choose($(row(s.box, "minh"), "select"), "writer");
  await settle();
  assert.equal(s.writes()[0].path, "/api/v1/users/2/role");
  assert.equal(s.calls.filter((c) => c.path === "/api/v1/invites").length, 2);
});

test("demoting yourself asks first; nothing is sent until you confirm", async () => {
  const users = [user(1, "lan", "Lan", "owner"), user(2, "minh", "Minh", "owner")];
  const s = await ready({ users });
  choose($(row(s.box, "lan"), "select"), "writer");
  await settle();
  assert.equal(s.writes().length, 0);
  assert.equal($(row(s.box, "lan"), "select").value, "owner", "the control shows the real role until it is confirmed");
  assert.match(pop().textContent, new RegExp(T("demote_title")));
  btn(pop(), T("demote_submit")).click();
  await settle();
  assert.deepEqual(s.writes(), [{ method: "PUT", path: "/api/v1/users/1/role", body: { role: "writer" } }]);
});

test("disable asks first and names the person; then sends PUT and the row turns disabled", async () => {
  const s = await ready();
  btn(row(s.box, "minh"), T("disable")).click();
  assert.match(pop().textContent, new RegExp("Minh Tr\u1ea7n"));
  assert.equal(s.writes().length, 0);
  btn(pop(), T("disable_submit")).click();
  await settle();
  assert.deepEqual(s.writes(), [{ method: "PUT", path: "/api/v1/users/2/disabled", body: { disabled: true } }]);
  assert.match(row(s.box, "minh").textContent, new RegExp(T("state_disabled")));
  assert.equal(pop(), null);
});

test("enabling needs no confirm", async () => {
  const s = await ready();
  btn(row(s.box, "bao"), T("enable")).click();
  await settle();
  assert.deepEqual(s.writes(), [{ method: "PUT", path: "/api/v1/users/3/disabled", body: { disabled: false } }]);
  assert.match(row(s.box, "bao").textContent, new RegExp(T("state_active")));
});

test("delete asks first, sends DELETE and removes the row", async () => {
  const s = await ready();
  btn(row(s.box, "minh"), T("delete")).click();
  assert.match(pop().textContent, new RegExp("Minh Tr\u1ea7n"));
  assert.equal(s.writes().length, 0);
  btn(pop(), T("delete_submit")).click();
  await settle();
  assert.deepEqual(s.writes(), [{ method: "DELETE", path: "/api/v1/users/2", body: undefined }]);
  assert.equal(row(s.box, "minh"), undefined);
  assert.equal(s.box.querySelectorAll(".usr").length, 2);
});

test("cancelling the confirm sends nothing", async () => {
  const s = await ready();
  btn(row(s.box, "minh"), T("delete")).click();
  btn(pop(), en["admin.common.cancel"]).click();
  await settle();
  assert.equal(s.writes().length, 0);
  assert.ok(row(s.box, "minh"));
});

test("the last owner refusal is said in plain words and the row is unchanged", async () => {
  const refuse = { "PUT /api/v1/users/1/role": { code: "user.last_owner", status: 409 }, "DELETE /api/v1/users/1": { code: "user.last_owner", status: 409 } };
  const s = await ready({ refuse });
  choose($(row(s.box, "lan"), "select"), "writer");
  await settle();
  btn(pop(), T("demote_submit")).click();
  await settle();
  const alert = $(s.box, "[role=alert]");
  assert.equal(alert.textContent, T("err_last_owner"));
  assert.equal($(row(s.box, "lan"), "select").value, "owner");
  btn(row(s.box, "lan"), T("delete")).click();
  btn(pop(), T("delete_submit")).click();
  await settle();
  assert.equal(alert.textContent, T("err_last_owner"));
  assert.ok(row(s.box, "lan"));
  assert.equal(pop(), null);
});

test("deleting someone who still has posts says so, and keeps the row", async () => {
  const s = await ready({ refuse: { "DELETE /api/v1/users/2": { code: "store.conflict", status: 409 } } });
  btn(row(s.box, "minh"), T("delete")).click();
  btn(pop(), T("delete_submit")).click();
  await settle();
  assert.equal($(s.box, "[role=alert]").textContent, T("err_has_posts"));
  assert.ok(row(s.box, "minh"));
});

test("a person who is already gone: 404 says so and reloads the list", async () => {
  const s = await ready({ refuse: { "PUT /api/v1/users/2/disabled": { code: "user.not_found", status: 404 } } });
  btn(row(s.box, "minh"), T("disable")).click();
  btn(pop(), T("disable_submit")).click();
  await settle();
  assert.equal($(s.box, "[role=alert]").textContent, T("err_gone"));
  assert.equal(s.calls.filter((c) => c.method === "GET" && c.path.startsWith("/api/v1/users")).length, 2);
});

test("403 and a lost connection get their own messages", async () => {
  const s = await ready({ refuse: { "PUT /api/v1/users/2/role": { code: "auth.forbidden", status: 403 }, "DELETE /api/v1/users/2": { code: "network", status: 0 } } });
  choose($(row(s.box, "minh"), "select"), "owner");
  await settle();
  assert.equal($(s.box, "[role=alert]").textContent, T("err_forbidden"));
  assert.equal($(row(s.box, "minh"), "select").value, "writer");
  btn(row(s.box, "minh"), T("delete")).click();
  btn(pop(), T("delete_submit")).click();
  await settle();
  assert.equal($(s.box, "[role=alert]").textContent, T("err_network"));
});

test("while a change is in flight the buttons are off; the confirm button says it is busy", async () => {
  const s = await ready({ hold: true });
  btn(row(s.box, "minh"), T("delete")).click();
  const go = btn(pop(), T("delete_submit"));
  go.click();
  await settle();
  assert.equal(go.disabled, true);
  assert.equal(go.textContent, T("delete_busy"));
  assert.equal(btn(row(s.box, "minh"), T("disable")).disabled, true);
  go.click();
  assert.equal(s.writes().length, 1, "a second click sends nothing");
  s.release();
  await settle();
  assert.equal(btn(row(s.box, "lan"), T("disable")).disabled, false);
});

test("an invite is created with the role and an expiry in UTC ms; the link shows once", async () => {
  const s = await ready();
  btn(s.box, T("inv_add")).click();
  const form = $(s.box, "form");
  [...form.querySelectorAll(".seg button")].find((b) => b.textContent === T("role_writer")).click();
  const exp = $(form, "select");
  assert.equal(exp.value, "7", "7 days by default");
  assert.deepEqual([...exp.options].map((o) => o.textContent), [T("inv_exp_1"), T("inv_exp_7"), T("inv_exp_30")]);
  const before = Date.now();
  form.dispatchEvent(new win.Event("submit", { bubbles: true, cancelable: true }));
  await settle();
  const post = s.writes()[0];
  assert.equal(post.path, "/api/v1/invites");
  assert.equal(post.body.role, "writer");
  assert.equal(typeof post.body.expires_at, "number");
  assert.ok(Math.abs(post.body.expires_at - (before + 7 * DAY)) < 120_000);
  const link = $(s.box, "input[readonly]");
  assert.equal(link.value, "http://localhost/admin/invite?code=CODE-ABC_123");
  assert.match(s.box.textContent, new RegExp(T("link_once")));
  btn(s.box, T("copy")).click();
  await settle();
  assert.equal(copied, link.value);
  assert.equal(s.calls.filter((c) => c.path === "/api/v1/invites" && c.method === "GET").length, 2, "the list reloads with the new invite");
  const again = await ready();
  assert.equal($(again.box, "input[readonly]"), null);
  assert.ok(!again.box.innerHTML.includes("CODE-ABC"));
  btn(s.box, T("done")).click();
  assert.equal($(s.box, "input[readonly]"), null);
});

test("an owner invite shows a warning; a writer invite does not; 30 days stays inside the server's limit", async () => {
  const s = await ready();
  btn(s.box, T("inv_add")).click();
  const form = $(s.box, "form");
  const warn = () => !!$(form, ".inv-warn") && !$(form, ".inv-warn").hidden;
  assert.equal(warn(), false);
  [...form.querySelectorAll(".seg button")].find((b) => b.textContent === T("role_owner")).click();
  assert.equal(warn(), true);
  assert.match($(form, ".inv-warn").textContent, new RegExp(T("inv_owner_warning")));
  choose($(form, "select"), "30");
  const before = Date.now();
  form.dispatchEvent(new win.Event("submit", { bubbles: true, cancelable: true }));
  await settle();
  const post = s.writes()[0];
  assert.equal(post.body.role, "owner");
  assert.ok(post.body.expires_at > before + 29 * DAY && post.body.expires_at < before + 30 * DAY);
});

test("pending invites show role and dates; an expired one is marked", async () => {
  const { box } = await ready();
  const rows = box.querySelectorAll(".inv");
  assert.equal(rows.length, 2);
  assert.doesNotMatch(rows[0].textContent, new RegExp(T("inv_expired_chip")));
  assert.match(rows[1].textContent, new RegExp(T("inv_expired_chip")));
  assert.match(rows[1].textContent, new RegExp(T("role_owner")));
});

test("revoking an invite asks first, then sends DELETE and drops it", async () => {
  const s = await ready();
  btn(s.box.querySelectorAll(".inv")[0], T("inv_revoke")).click();
  assert.equal(s.writes().length, 0);
  btn(pop(), T("inv_revoke_submit")).click();
  await settle();
  assert.deepEqual(s.writes(), [{ method: "DELETE", path: "/api/v1/invites/2", body: undefined }]);
  assert.equal(s.box.querySelectorAll(".inv").length, 1);
});

test("a revoke that finds the invite gone says so and reloads", async () => {
  const s = await ready({ refuse: { "DELETE /api/v1/invites/2": { code: "store.not_found", status: 404 } } });
  btn(s.box.querySelectorAll(".inv")[0], T("inv_revoke")).click();
  btn(pop(), T("inv_revoke_submit")).click();
  await settle();
  assert.equal($(s.box, "[role=alert]").textContent, T("inv_err_gone"));
  assert.equal(s.calls.filter((c) => c.method === "GET" && c.path === "/api/v1/invites").length, 2);
});

test("empty states: only you, and no invitations", async () => {
  const { box } = await ready({ users: [user(1, "lan", "Lan", "owner")], invites: [] });
  assert.equal(box.querySelectorAll(".usr").length, 1);
  assert.match(box.textContent, new RegExp(T("empty")));
  assert.match(box.textContent, new RegExp(T("inv_empty")));
});

test("a failed load shows a retry that loads again", async () => {
  const s = await ready({ refuse: { "GET /api/v1/users": { code: "internal", status: 500 } } });
  assert.ok($(s.box, "[role=alert]"));
  assert.equal(s.box.querySelectorAll(".usr").length, 0);
  assert.equal(s.box.querySelectorAll(".inv").length, 2, "the invitations load on their own");
});

test("offline: the list stays, every action is off with the reason shown, and it follows the network", async () => {
  const s = await ready({ offline: true });
  const acts = () => [...s.box.querySelectorAll(".usr select, .usr button, .inv button")].concat(btn(s.box, T("inv_add")) ?? []);
  assert.ok(acts().length >= 8);
  assert.ok(acts().every((b) => b.disabled));
  const shown = () => [...s.box.querySelectorAll("p")].some((p) => !p.hidden && p.textContent === T("offline"));
  assert.equal(shown(), true);
  s.go(false);
  assert.ok(acts().every((b) => !b.disabled));
  assert.equal(shown(), false);
});

test("more people load on request with the cursor, and the button goes when there are no more", async () => {
  const users = Array.from({ length: 5 }, (_, i) => user(i + 1, `u${i + 1}`, `U ${i + 1}`, i === 0 ? "owner" : "writer"));
  const s = await ready({ users, pages: 2 });
  assert.equal(s.box.querySelectorAll(".usr").length, 2);
  btn(s.box, T("more")).click();
  await settle();
  assert.equal(s.box.querySelectorAll(".usr").length, 4);
  assert.ok(s.calls.some((c) => c.path === "/api/v1/users?cursor=2"));
  btn(s.box, T("more")).click();
  await settle();
  assert.equal(s.box.querySelectorAll(".usr").length, 5);
  assert.equal(btn(s.box, T("more")), undefined);
});

test("Settings lists People for an owner at /admin/settings/users and draws it", async () => {
  const s = setup();
  const box = document.createElement("div");
  document.body.append(box);
  m.mountSettings(box, s.api, { role: "owner", userId: 1, section: "users", offline: () => false });
  await settle();
  await settle();
  assert.equal($(box, ".subnav a[aria-current=page]").textContent, en["admin.settings.users"]);
  assert.equal($(box, ".subnav a[aria-current=page]").getAttribute("href"), "/admin/settings/users");
  assert.ok(box.querySelectorAll(".usr").length === 3);
});
