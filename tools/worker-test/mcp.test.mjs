// MCP at POST /mcp over HTTP against the real Worker (issue #14): transport refusals, JSON-RPC
// shape, tools generated from the /api/v1 table and run through the same dispatch (permissions,
// conflicts), and the guides as resources.
import assert from "node:assert/strict";
import { createHash, randomBytes } from "node:crypto";
import { readFileSync } from "node:fs";
import { after, before, describe, test } from "node:test";

import { startWorker } from "./harness.mjs";

const COOKIE = "__Host-plumblog_session";
const DAY = 86_400_000;
const T = Date.now();
const sha256hex = (s) => createHash("sha256").update(s).digest("hex");
const b64u = (buf) => Buffer.from(buf).toString("base64url");
const sq = (s) => `'${String(s).replaceAll("'", "''")}'`;
const newSecret = () => `plb_${b64u(randomBytes(32))}`;
// crates/model MCP_MAX_BODY: MAX_POST_BODY (160 KiB) + 4 KiB.
const MCP_MAX_BODY = 164 * 1024;

const TOK = { owner: newSecret(), read: newSecret(), writer: newSecret() };
const SESSION = b64u(randomBytes(32));

let w;
before(async () => {
  const seed = [
    `INSERT INTO users (id, login, slug, display_name, role, password_hash, disabled_at, created_at, updated_at) VALUES
      (1, 'olga', 'olga', 'Olga', 'owner', NULL, NULL, ${T}, ${T}),
      (2, 'will', 'will', 'Will', 'writer', NULL, NULL, ${T}, ${T});`,
    `INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES
      (${sq(sha256hex(SESSION))}, 1, ${T}, ${T + 29 * DAY});`,
    `INSERT INTO api_tokens (user_id, name, token_hash, scope, created_at, expires_at, revoked_at) VALUES
      (1, 'owner', ${sq(sha256hex(TOK.owner))}, 'owner', ${T}, NULL, NULL),
      (1, 'reader', ${sq(sha256hex(TOK.read))}, 'read', ${T}, NULL, NULL),
      (2, 'writer', ${sq(sha256hex(TOK.writer))}, 'writer', ${T}, NULL, NULL);`,
  ].join("\n");
  w = await startWorker({ seedSql: seed });
});

after(async () => {
  await w?.stop();
});

/** One POST /mcp. `body` is sent as is when a string. `bearer: null` sends no token. */
async function post(body, { bearer = TOK.owner, headers = {}, method = "POST" } = {}) {
  const h = { "content-type": "application/json", accept: "application/json, text/event-stream", ...headers };
  if (bearer) h.authorization = `Bearer ${bearer}`;
  const payload = body === undefined ? undefined : typeof body === "string" ? body : JSON.stringify(body);
  const res = await fetch(`${w.url}/mcp`, { method, headers: h, body: payload });
  const text = await res.text();
  return { status: res.status, headers: res.headers, text, json: text ? JSON.parse(text) : null };
}

let nextId = 1;
const rpc = (method, params, opts) => post({ jsonrpc: "2.0", id: nextId++, method, ...(params ? { params } : {}) }, opts);
const callTool = (name, args, opts) => rpc("tools/call", { name, arguments: args }, opts);

/** A transport refusal: HTTP status and a JSON-RPC error with no id. */
function assertRefused(r, status, code, error) {
  assert.equal(r.status, status, r.text);
  assert.equal(r.json.jsonrpc, "2.0");
  assert.equal(r.json.id, null);
  assert.equal(r.json.error.code, code);
  assert.equal(r.json.error.data.error, error);
}

/** A tool result. */
function result(r) {
  assert.equal(r.status, 200, r.text);
  assert.equal(r.json.error, undefined, r.text);
  const res = r.json.result;
  assert.equal(JSON.stringify(res.structuredContent), JSON.stringify(JSON.parse(res.content[0].text)));
  assert.equal(res.content[0].type, "text");
  return res;
}

const newPost = (slug) => ({ lang: "en", slug, title: `Title ${slug}`, markdown: `Body of ${slug}.` });

describe("mcp transport", () => {
  test("initialize negotiates the version", async () => {
    const r = await rpc("initialize", { protocolVersion: "2025-06-18", capabilities: {}, clientInfo: { name: "t", version: "1" } });
    assert.equal(r.status, 200, r.text);
    assert.match(r.headers.get("content-type"), /^application\/json/);
    assert.equal(r.headers.get("mcp-session-id"), null);
    assert.equal(r.json.id, nextId - 1);
    assert.equal(r.json.result.protocolVersion, "2025-06-18");
    assert.equal(r.json.result.serverInfo.name, "plumblog");
    assert.deepEqual(r.json.result.capabilities, { tools: { listChanged: false }, resources: { listChanged: false } });
    const old = await rpc("initialize", { protocolVersion: "2024-11-05", capabilities: {} });
    assert.equal(old.json.result.protocolVersion, "2025-11-25");
    const bad = await rpc("ping", undefined, { headers: { "mcp-protocol-version": "1999-01-01" } });
    assertRefused(bad, 400, -32600, "input.invalid");
    assert.equal(bad.json.error.data.field, "mcp-protocol-version");
    const ok = await rpc("ping", undefined, { headers: { "mcp-protocol-version": "2025-06-18" } });
    assert.deepEqual(ok.json.result, {});
  });

  test("notifications and responses are 202 with no body", async () => {
    const n = await post({ jsonrpc: "2.0", method: "notifications/initialized" });
    assert.equal(n.status, 202);
    assert.equal(n.text, "");
    const resp = await post({ jsonrpc: "2.0", id: 9, result: {} });
    assert.equal(resp.status, 202);
  });

  test("GET and DELETE are 405 with Allow: POST", async () => {
    for (const method of ["GET", "DELETE"]) {
      const r = await post(undefined, { method });
      assert.equal(r.status, 405, r.text);
      assert.equal(r.headers.get("allow"), "POST");
      assert.equal(r.json.error.data.error, "api.not_found");
    }
  });

  test("no token, a session cookie, a bad token: 401", async () => {
    assertRefused(await rpc("ping", undefined, { bearer: null }), 401, -32600, "auth.unauthenticated");
    // A valid owner session on its own is not enough, even with the site's own Origin.
    const cookie = await rpc("tools/list", undefined, {
      bearer: null,
      headers: { cookie: `${COOKIE}=${SESSION}`, origin: w.url },
    });
    assertRefused(cookie, 401, -32600, "auth.unauthenticated");
    assertRefused(await rpc("ping", undefined, { bearer: newSecret() }), 401, -32600, "auth.unauthenticated");
  });

  test("a foreign Origin is 403; the site's own is fine", async () => {
    assertRefused(await rpc("ping", undefined, { headers: { origin: "https://evil.example" } }), 403, -32600, "auth.forbidden");
    const own = await rpc("ping", undefined, { headers: { origin: w.url } });
    assert.equal(own.status, 200, own.text);
  });

  test("size and shape refusals", async () => {
    const big = JSON.stringify({ jsonrpc: "2.0", id: 1, method: "ping", params: { pad: "x".repeat(MCP_MAX_BODY) } });
    assertRefused(await post(big), 413, -32600, "api.body_too_large");
    // 4500 values in containers under 1000 items each: the whole-document value cap.
    const many = { jsonrpc: "2.0", id: 1, method: "ping", params: { a: [], b: [], c: [], d: [], e: [] } };
    for (const k of "abcde") many.params[k] = Array(900).fill(1);
    assertRefused(await post(many), 413, -32700, "input.too_complex");
    // Deeper than 16 levels: the strict parser refuses it as invalid JSON for us.
    let deep = "1";
    for (let i = 0; i < 20; i++) deep = `[${deep}]`;
    assertRefused(await post(`{"jsonrpc":"2.0","id":1,"method":"ping","params":{"a":${deep}}}`), 400, -32700, "input.invalid");
    assertRefused(await post("{nope"), 400, -32700, "input.invalid");
    assertRefused(await post([{ jsonrpc: "2.0", id: 1, method: "ping" }]), 400, -32600, "input.invalid");
    const unknown = await rpc("prompts/list");
    assert.equal(unknown.status, 200);
    assert.equal(unknown.json.error.code, -32601);
    assert.equal(unknown.json.error.data.error, "api.not_found");
  });
});

describe("mcp tools", () => {
  test("an owner sees every /api/v1 route but the reference and the raw upload", async () => {
    const ref = await fetch(`${w.url}/api/v1/reference`, { headers: { authorization: `Bearer ${TOK.owner}` } }).then((r) => r.json());
    const want = ref.routes
      .map((r) => `${r.method} ${r.path}`)
      .filter((k) => k !== "GET /api/v1/reference" && k !== "POST /api/v1/media")
      .sort();
    const r = await rpc("tools/list");
    const tools = r.json.result.tools;
    const got = tools.map((t) => t.description.split(";")[0]).sort();
    assert.deepEqual(got, want);
    assert.equal(new Set(tools.map((t) => t.name)).size, tools.length);
    const update = tools.find((t) => t.name === "post_update");
    assert.equal(update.inputSchema.additionalProperties, false);
    assert.deepEqual(update.inputSchema.required, ["id", "expected_version"]);
    assert.deepEqual(update.inputSchema.properties.published_at, { type: ["integer", "null"] });
    assert.equal(tools.find((t) => t.name === "post_get").annotations.readOnlyHint, true);
    assert.equal(tools.find((t) => t.name === "post_delete").annotations.destructiveHint, true);
  });

  test("a read token lists no write tool", async () => {
    const tools = (await rpc("tools/list", undefined, { bearer: TOK.read })).json.result.tools;
    assert.ok(tools.length > 0);
    assert.deepEqual(tools.filter((t) => !t.annotations.readOnlyHint).map((t) => t.name), []);
    // Plugin options are owner only, reads included.
    assert.ok(!tools.some((t) => t.name === "plugin_options"));
  });

  test("create, read, and a stale update is a conflict result", async () => {
    const made = result(await callTool("post_create", newPost("mcp-one")));
    assert.equal(made.isError, false);
    const post = made.structuredContent;
    assert.equal(post.slug, "mcp-one");

    const got = result(await callTool("post_get", { id: post.id }, { bearer: TOK.read }));
    assert.equal(got.isError, false);
    assert.equal(got.structuredContent.title, "Title mcp-one");

    const ok = result(await callTool("post_update", { id: post.id, expected_version: post.version, title: "New" }));
    assert.equal(ok.isError, false);
    assert.equal(ok.structuredContent.title, "New");

    const stale = result(await callTool("post_update", { id: post.id, expected_version: post.version, title: "Lost" }));
    assert.equal(stale.isError, true);
    assert.equal(stale.structuredContent.error, "store.conflict");
    assert.equal(stale.structuredContent.version, ok.structuredContent.version);

    const after = result(await callTool("post_get", { id: post.id }));
    assert.equal(after.structuredContent.title, "New");

    // A 204 route answers an empty object.
    const del = result(await callTool("tag_create", { slug: "t1", name: "T1" }));
    const gone = result(await callTool("tag_delete", { id: del.structuredContent.id }));
    assert.deepEqual(gone, { content: [{ type: "text", text: "{}" }], structuredContent: {}, isError: false });
  });

  test("refusals are tool results naming the code; nothing changes", async () => {
    const post = result(await callTool("post_create", newPost("mcp-owned"))).structuredContent;

    const readWrite = result(await callTool("post_create", newPost("mcp-read"), { bearer: TOK.read }));
    assert.equal(readWrite.isError, true);
    assert.deepEqual(readWrite.structuredContent, { error: "auth.forbidden" });

    // A writer token cannot do owner work (writers may edit any post by design, so the
    // boundary is site settings), and does not even see the tool.
    const writerSetting = result(await callTool("setting_put", { key: "title", value: "Taken" }, { bearer: TOK.writer }));
    assert.equal(writerSetting.isError, true);
    assert.deepEqual(writerSetting.structuredContent, { error: "auth.forbidden" });
    const writerTools = (await rpc("tools/list", undefined, { bearer: TOK.writer })).json.result.tools.map((t) => t.name);
    assert.ok(writerTools.includes("post_update") && !writerTools.includes("setting_put"));
    const settings = result(await callTool("setting_list", {})).structuredContent;
    assert.ok(!JSON.stringify(settings).includes("Taken"));

    // Session-only settings stay session only through MCP.
    const origin = result(await callTool("setting_put", { key: "origin", value: "https://evil.example" }));
    assert.equal(origin.structuredContent.error, "auth.forbidden");

    const unknownArg = result(await callTool("post_get", { id: post.id, nope: 1 }));
    assert.deepEqual(unknownArg.structuredContent, { error: "input.invalid", field: "nope" });
    const badType = result(await callTool("post_update", { id: post.id, expected_version: "1" }));
    assert.deepEqual(badType.structuredContent, { error: "input.invalid", field: "expected_version" });

    const still = result(await callTool("post_get", { id: post.id }));
    assert.equal(still.structuredContent.title, "Title mcp-owned");
    assert.equal(still.structuredContent.version, post.version);
    const list = result(await callTool("post_list", { limit: 100 })).structuredContent;
    assert.ok(!JSON.stringify(list).includes("mcp-read"));
  });

  test("unknown tool and bad arguments shape are -32602", async () => {
    const tool = await callTool("reference", {});
    assert.equal(tool.json.error.code, -32602);
    assert.deepEqual(tool.json.error.data, { error: "input.invalid", field: "name" });
    const args = await rpc("tools/call", { name: "post_get", arguments: [1] });
    assert.equal(args.json.error.code, -32602);
    assert.equal(args.json.error.data.field, "arguments");
  });
});

describe("mcp resources", () => {
  test("guides are listed and read", async () => {
    const list = (await rpc("resources/list")).json.result.resources;
    const setup = list.find((r) => r.uri === "plumblog://guides/setup");
    assert.ok(setup, JSON.stringify(list));
    assert.equal(setup.mimeType, "text/markdown");
    assert.equal(setup.name, "setup");
    const file = readFileSync(new URL("../../guides/setup.md", import.meta.url), "utf8");
    assert.equal(`# ${setup.title}`, file.split("\n")[0]);
    const read = await rpc("resources/read", { uri: setup.uri }, { bearer: TOK.read });
    assert.deepEqual(read.json.result.contents, [{ uri: setup.uri, mimeType: "text/markdown", text: file }]);
    const missing = await rpc("resources/read", { uri: "plumblog://guides/nope" });
    assert.equal(missing.json.error.code, -32602);
    assert.deepEqual(missing.json.error.data, { error: "store.not_found", field: "uri" });
  });
});
