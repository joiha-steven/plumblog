// The read-only routes the admin builds its Appearance and plugin tabs from: installed themes
// and option declarations, through the real Worker, in the shapes generated from crates/model.
// The Worker bundles no plugin (plugins/ is empty), so plugin declarations are covered here
// only for their gate and 404; crates/render/tests/extensions.rs covers a loaded plugin.
import assert from "node:assert/strict";
import { createHash, randomBytes } from "node:crypto";
import { readFileSync } from "node:fs";
import { after, before, test } from "node:test";

import * as gen from "../../web/admin/src/api/model.gen.ts";
import { checkShape } from "../shape-check.mjs";
import { startWorker } from "./harness.mjs";

const { SHAPES, ...rest } = gen;
const lists = Object.fromEntries(Object.entries(rest).filter(([, v]) => Array.isArray(v)));
const check = (name, value) => checkShape(SHAPES, lists, name, value);

const T = Date.now();
const DAY = 86_400_000;
const sha256hex = (s) => createHash("sha256").update(s).digest("hex");
const b64u = (buf) => Buffer.from(buf).toString("base64url");
const OWNER = b64u(randomBytes(32));
const WRITER = b64u(randomBytes(32));
const READ_TOKEN = `plb_${b64u(randomBytes(32))}`;

const theme = (f) => readFileSync(new URL(`../../themes/minimal/${f}`, import.meta.url), "utf8");
/** `[options.<key>]` tables of theme.toml and their `type`, without a TOML parser. */
const declared = theme("theme.toml")
  .split(/^\[/m)
  .filter((t) => t.startsWith("options."))
  .map((t) => [t.match(/^options\.(\w+)\]/)[1], t.match(/^type = "(\w+)"$/m)[1]]);
/** One `[options]` string of a locale file. */
const localeText = (lang, key) => {
  const section = theme(`locales/${lang}.toml`).split(/^\[options\]$/m)[1];
  return section.match(new RegExp(`^${key} = "(.*)"$`, "m"))[1];
};

let w;
before(async () => {
  w = await startWorker({
    seedSql: [
      `INSERT INTO users (id, login, slug, display_name, role, password_hash, disabled_at, created_at, updated_at) VALUES
        (1, 'olga', 'olga', 'Olga', 'owner', NULL, NULL, ${T}, ${T}),
        (2, 'will', 'will', 'Will', 'writer', NULL, NULL, ${T}, ${T});`,
      `INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES
        ('${sha256hex(OWNER)}', 1, ${T}, ${T + DAY}),
        ('${sha256hex(WRITER)}', 2, ${T}, ${T + DAY});`,
      `INSERT INTO api_tokens (user_id, name, token_hash, scope, created_at, expires_at, revoked_at) VALUES
        (1, 'reader', '${sha256hex(READ_TOKEN)}', 'read', ${T}, NULL, NULL);`,
    ].join("\n"),
  });
});
after(async () => {
  await w?.stop();
});

async function get(path, session = OWNER) {
  const headers = { origin: w.url };
  if (session) headers.cookie = `__Host-plumblog_session=${session}`;
  const res = await fetch(w.url + path, { headers });
  const text = await res.text();
  return { status: res.status, json: text ? JSON.parse(text) : null, text };
}
const shaped = (name, r) => {
  assert.equal(r.status, 200, r.text);
  assert.deepEqual(check(name, r.json), [], r.text);
  return r.json;
};
const refused = (r, status, code) => {
  assert.equal(r.status, status, r.text);
  assert.equal(r.json.error, code, r.text);
};
async function mcp(method, params) {
  const res = await fetch(`${w.url}/mcp`, {
    method: "POST",
    headers: { "content-type": "application/json", accept: "application/json", authorization: `Bearer ${READ_TOKEN}` },
    body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, ...(params ? { params } : {}) }),
  });
  return (await res.json()).result;
}

test("the theme list shows the embedded default theme, active", async () => {
  const { themes } = shaped("theme_list", await get("/api/v1/themes"));
  const version = theme("theme.toml").match(/^version = "(.*)"$/m)[1];
  assert.deepEqual(themes, [{ name: "minimal", version, active: true, source: "embedded", available: true }]);
  // Any signed-in caller, like theme option values.
  shaped("theme_list", await get("/api/v1/themes", WRITER));
});

test("the default theme's declarations match its theme.toml", async () => {
  assert.ok(declared.length >= 5, `found ${declared.length} options in theme.toml`);
  const d = shaped("option_declarations", await get("/api/v1/themes/minimal/declarations"));
  assert.equal(d.name, "minimal");
  assert.equal(d.lang, "en", "English without ?lang");
  assert.deepEqual(
    d.options.map((o) => [o.key, o.type]),
    [...declared].sort(([a], [b]) => (a < b ? -1 : 1)),
  );
  const by = Object.fromEntries(d.options.map((o) => [o.key, o]));
  assert.deepEqual([by.excerpt_words.min, by.excerpt_words.max, by.excerpt_words.step], [0, 120, 5]);
  assert.equal(by.excerpt_words.default, 55);
  assert.equal(by.accent.token, "accent");
  assert.deepEqual(by.style.choices.map((c) => c.value), ["writer"], "draft styles are not offered");
  assert.equal(by.sidenotes.label, localeText("en", "sidenotes"));
  assert.deepEqual(d.groups.map((g) => g.key).sort(), ["look", "reading"]);
  // Writers read it as they read the values.
  shaped("option_declarations", await get("/api/v1/themes/minimal/declarations", WRITER));
});

test("labels come in the asked language, English for one the theme lacks", async () => {
  const vi = shaped("option_declarations", await get("/api/v1/themes/minimal/declarations?lang=vi"));
  assert.equal(vi.lang, "vi");
  const style = vi.options.find((o) => o.key === "style");
  assert.equal(style.label, localeText("vi", "style"));
  assert.equal(style.help, localeText("vi", "style_help"));
  assert.equal(style.label_key, "options.style");
  assert.equal(vi.groups.find((g) => g.key === "look").label, localeText("vi", "group_look"));
  const region = shaped("option_declarations", await get("/api/v1/themes/minimal/declarations?lang=vi-VN"));
  assert.equal(region.lang, "vi");
  const fr = shaped("option_declarations", await get("/api/v1/themes/minimal/declarations?lang=fr"));
  assert.equal(fr.lang, "en");
  assert.equal(fr.options.find((o) => o.key === "style").label, localeText("en", "style"));
});

test("unknown themes and plugins are 404; plugin declarations are for plugin managers only", async () => {
  refused(await get("/api/v1/themes/nope/declarations"), 404, "store.not_found");
  refused(await get("/api/v1/plugins/nope/declarations"), 404, "store.not_found");
  // Access before lookup: a writer learns nothing, not even whether the plugin exists.
  refused(await get("/api/v1/plugins/nope/declarations", WRITER), 403, "auth.forbidden");
  refused(await get("/api/v1/themes/Bad/declarations"), 400, "input.invalid");
  for (const path of ["/api/v1/themes", "/api/v1/themes/minimal/declarations"]) {
    refused(await get(path, null), 401, "auth.unauthenticated");
  }
});

test("a read token gets theme declarations over MCP, but not plugin ones", async () => {
  const r = await mcp("tools/call", { name: "theme_declarations", arguments: { name: "minimal", lang: "vi" } });
  assert.equal(r.isError, false, JSON.stringify(r));
  assert.deepEqual(check("option_declarations", r.structuredContent), []);
  assert.equal(r.structuredContent.lang, "vi");
  const names = (await mcp("tools/list")).tools.map((t) => t.name);
  assert.ok(names.includes("theme_list") && names.includes("theme_declarations"), names.join());
  assert.ok(!names.includes("plugin_declarations"), "plugin.manage is beyond a read token");
  // Unlisted is not enough: calling it anyway is refused before any lookup.
  const called = await mcp("tools/call", { name: "plugin_declarations", arguments: { name: "hello" } });
  assert.equal(called.isError, true, JSON.stringify(called));
  assert.deepEqual(called.structuredContent, { error: "auth.forbidden" });
});

test("a language tag is read case-insensitively, with _ as -", async () => {
  for (const lang of ["VI", "vi_VN", "Vi-vn"]) {
    const r = await mcp("tools/call", { name: "theme_declarations", arguments: { name: "minimal", lang } });
    assert.equal(r.structuredContent.lang, "vi", lang);
  }
});
