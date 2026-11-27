// The admin's sign-in core (web/admin/src/auth, api/client.ts call) without a browser or a
// Worker. Files without imports load straight into Node (it strips their types); flows.ts and
// client.ts use extensionless imports, so esbuild bundles them here first.
import assert from "node:assert/strict";
import { test } from "node:test";
import { pbkdf2Sync } from "node:crypto";
import { readFileSync } from "node:fs";
import { build } from "esbuild";
import { safeNext } from "../../web/admin/src/auth/next.ts";
import { assertion, attestation, b64u, createOptions, fromB64u, getOptions } from "../../web/admin/src/auth/webauthn.ts";
import { countDrafts } from "../../web/admin/src/editor/drafts.ts";

const root = new URL("../../", import.meta.url);

async function bundle(file) {
  const out = await build({
    entryPoints: [new URL(file, root).pathname],
    bundle: true,
    write: false,
    format: "esm",
    logLevel: "silent",
  });
  return import(`data:text/javascript;base64,${Buffer.from(out.outputFiles[0].text).toString("base64")}`);
}

// --- safeNext ---

const GOOD = [
  "/admin/",
  "/admin/posts",
  "/admin/posts/1",
  "/admin/posts/1?x=1#y",
  "/admin/?tab=drafts",
  "/admin/#top",
  "/admin/posts/new-abc_def",
  "/admin/posts/caf\u00e9",
];
const BAD = [
  [undefined, "missing"],
  [null, "missing"],
  ["", "empty"],
  ["//evil.example/admin/", "protocol-relative"],
  ["/\\evil.example/admin/", "slash backslash"],
  ["\\\\evil.example", "backslashes"],
  ["/admin/\\evil", "backslash inside"],
  ["/admin/..\\..\\x", "backslash dot dot"],
  ["/admin/%2fevil", "encoded slash"],
  ["/admin/%2F%2Fevil.example", "encoded slashes, upper case"],
  ["/admin/%5cevil", "encoded backslash"],
  ["/admin/%2e%2e/%2e%2e/api/auth/logout", "encoded dot dot"],
  ["/admin/%252e%252e/x", "double encoded dot dot"],
  ["/admin/%25252f/x", "triple encoded slash"],
  ["/admin/%zz", "broken escape"],
  ["/admin/../api/auth/logout", "dot dot segment"],
  ["/admin/posts/..", "trailing dot dot"],
  ["/admin/./posts", "dot segment"],
  ["/admin//evil.example", "empty segment"],
  ["https://evil.example/admin/", "absolute URL"],
  ["javascript:alert(1)", "javascript scheme"],
  ["JaVaScRiPt:alert(1)//admin/", "mixed case scheme"],
  ["data:text/html,x", "data scheme"],
  ["/admin/\u0000x", "NUL"],
  ["/admin/\nx", "newline"],
  ["/admin/\tx", "tab"],
  ["/admin/ x", "space"],
  ["/admin/\u200bx", "zero width space"],
  ["/admin/%0ax", "encoded newline"],
  ["/admin/%09x", "encoded tab"],
  [" /admin/", "leading space"],
  ["/admin", "no trailing slash"],
  ["/administrator", "longer name"],
  ["/administrator/", "longer name with slash"],
  ["/Admin/", "other case"],
  ["admin/", "relative"],
  ["/posts", "outside the admin"],
  ["/admin/" + "a".repeat(3000), "too long"],
];

test("safeNext keeps admin paths", () => {
  for (const p of GOOD) assert.equal(safeNext(p), p, JSON.stringify(p));
});

test("safeNext sends everything else to /admin/", () => {
  assert.ok(GOOD.length + BAD.length >= 25);
  for (const [p, why] of BAD) assert.equal(safeNext(p), "/admin/", `${why}: ${JSON.stringify(p)}`);
});

// --- base64url ---

test("b64u and fromB64u round-trip every length and byte", () => {
  for (let n = 0; n < 40; n++) {
    const bytes = Uint8Array.from({ length: n }, (_, i) => (i * 97 + n * 31) & 0xff);
    const s = b64u(bytes);
    assert.match(s, /^[A-Za-z0-9_-]*$/);
    assert.equal(s, Buffer.from(bytes).toString("base64url"));
    assert.deepEqual([...fromB64u(s)], [...bytes]);
    assert.equal(b64u(bytes.buffer), s);
  }
  assert.equal(b64u(new Uint8Array([0xfb, 0xff])), "-_8");
});

test("fromB64u is strict like crates/auth base64url::decode", () => {
  for (const bad of ["QQ==", "QQ=", "Q", "QUJD\n", "QU JD", "QU+D", "QU/D", "QU.D", "QR", "QUJ", "\u00e9\u00e9"]) {
    assert.throws(() => fromB64u(bad), Error, JSON.stringify(bad));
  }
  assert.deepEqual([...fromB64u("")], []);
  assert.deepEqual([...fromB64u("QQ")], [65]);
});

// --- options ---

const enc = (s) => Buffer.from(s).toString("base64url");
const text = (u) => Buffer.from(u).toString();

test("createOptions decodes challenge, user id and excluded ids, keeps the rest", () => {
  const o = createOptions({
    challenge: enc("chal"),
    rp: { id: "blog.example", name: "blog.example" },
    user: { id: enc("\u0000\u0000\u0000\u0000\u0000\u0000\u0000\u0007"), name: "olga", displayName: "Olga" },
    pubKeyCredParams: [{ type: "public-key", alg: -7 }],
    excludeCredentials: [{ type: "public-key", id: enc("k1") }, { type: "public-key", id: enc("k2") }],
    timeout: 300000,
  });
  assert.equal(text(o.challenge), "chal");
  assert.deepEqual([...o.user.id], [0, 0, 0, 0, 0, 0, 0, 7]);
  assert.equal(o.user.name, "olga");
  assert.deepEqual(o.excludeCredentials.map((c) => text(c.id)), ["k1", "k2"]);
  assert.equal(o.excludeCredentials[0].type, "public-key");
  assert.equal(o.rp.id, "blog.example");
  assert.equal(o.timeout, 300000);
  assert.throws(() => createOptions({ challenge: "a=", user: { id: "AA" } }));
  assert.throws(() => createOptions({ challenge: "AA" }));
  assert.throws(() => createOptions(null));
});

test("getOptions decodes challenge and allowed ids", () => {
  const o = getOptions({ challenge: enc("c2"), rpId: "blog.example", allowCredentials: [{ type: "public-key", id: enc("k") }] });
  assert.equal(text(o.challenge), "c2");
  assert.equal(text(o.allowCredentials[0].id), "k");
  assert.equal(o.rpId, "blog.example");
  assert.equal(getOptions({ challenge: "AA" }).allowCredentials, undefined);
  assert.throws(() => getOptions({ challenge: "AA", allowCredentials: [{ id: 5 }] }));
});

// --- assertion field names ---

// crates/worker/src/api/passkey.rs:31 ASSERTION_FIELDS:
//   "credential_id", "client_data_json", "authenticator_data", "signature", "user_handle"
// crates/worker/src/api/passkey.rs:41 REAUTH_FIELDS:
//   "current_password", "reauth_credential_id", "reauth_client_data_json",
//   "reauth_authenticator_data", "reauth_signature", "reauth_user_handle", "turnstile_token"
// Read again from the source below, so a rename on either side fails here.
function rustList(name) {
  const src = readFileSync(new URL("crates/worker/src/api/passkey.rs", root), "utf8");
  const m = src.match(new RegExp(`const ${name}: &\\[&str\\] = &\\[([^\\]]*)\\]`));
  assert.ok(m, `${name} not found in passkey.rs`);
  return [...m[1].matchAll(/"([a-z_]+)"/g)].map((x) => x[1]);
}

const buf = (s) => new Uint8Array(Buffer.from(s)).buffer;
const fakeAssertion = (handle) => ({
  id: enc("cred-1"),
  response: {
    clientDataJSON: buf("{cd}"),
    authenticatorData: buf("ad"),
    signature: buf("sig"),
    userHandle: handle === undefined ? null : buf(handle),
  },
});

test("assertion field names are the server's, with and without reauth_", () => {
  const sign = rustList("ASSERTION_FIELDS");
  assert.deepEqual(sign, ["credential_id", "client_data_json", "authenticator_data", "signature", "user_handle"]);
  const full = assertion(fakeAssertion("u7"));
  assert.deepEqual(Object.keys(full).sort(), [...sign].sort());
  assert.equal(full.credential_id, enc("cred-1"));
  assert.equal(full.client_data_json, enc("{cd}"));
  assert.equal(full.user_handle, enc("u7"));
  assert.deepEqual(Object.keys(assertion(fakeAssertion())).sort(), sign.filter((f) => f !== "user_handle").sort());

  const reauth = rustList("REAUTH_FIELDS").filter((f) => f.startsWith("reauth_"));
  assert.deepEqual(Object.keys(assertion(fakeAssertion("u7"), "reauth_")).sort(), reauth.sort());
  assert.deepEqual(reauth.sort(), sign.map((f) => `reauth_${f}`).sort());
});

test("attestation fields are register/verify's", () => {
  const a = attestation({ response: { clientDataJSON: buf("c"), attestationObject: buf("o") } });
  assert.deepEqual(a, { client_data_json: enc("c"), attestation_object: enc("o") });
});

// --- drafts ---

// A Map behind the slice of IndexedDB drafts.ts uses: open, transaction, get/put/count/delete.
function fakeIndexedDB(rows) {
  const inRange = (k, r) => (typeof r === "string" ? k === r : k >= r.lower && k <= r.upper);
  const request = (fn) => {
    const r = {};
    queueMicrotask(() => {
      r.result = fn();
      r.onsuccess?.();
    });
    return r;
  };
  const store = {
    get: (k) => request(() => rows.get(k)),
    put: (v, k) => request(() => rows.set(k, v) && k),
    count: (r) => request(() => [...rows.keys()].filter((k) => inRange(k, r)).length),
    delete: (r) => request(() => [...rows.keys()].filter((k) => inRange(k, r)).forEach((k) => rows.delete(k))),
  };
  const db = {
    objectStoreNames: { contains: () => true },
    close() {},
    transaction() {
      const tx = { objectStore: () => store };
      setTimeout(() => tx.oncomplete?.(), 0);
      return tx;
    },
  };
  globalThis.IDBKeyRange = { bound: (lower, upper) => ({ lower, upper }) };
  globalThis.indexedDB = { open: () => request(() => db) };
}

const draftRows = () =>
  new Map([
    ["1:post-3", {}],
    ["1:new-ab", {}],
    ["10:post-3", {}],
    ["2:post-1", {}],
  ]);

test("countDrafts counts one user's drafts only (user 1 is not user 10)", async () => {
  fakeIndexedDB(draftRows());
  assert.equal(await countDrafts(1), 2);
  assert.equal(await countDrafts(10), 1);
  assert.equal(await countDrafts(3), 0);
});

// --- flows ---

const flows = await bundle("web/admin/src/auth/flows.ts");
const SITE = "site-test";
const ITER = 600000;
const expected = (login, pw) =>
  pbkdf2Sync(pw, `plumblog:${SITE}:${login.toLowerCase()}`, ITER, 32, "sha256").toString("base64url");

/** An Api whose `call` records requests and answers from `routes` ("POST /path" -> Result). */
function fakeApi(routes = {}) {
  const calls = [];
  return {
    calls,
    async call(method, path, body) {
      calls.push({ method, path, body });
      const r = routes[`${method} ${path}`];
      if (typeof r === "function") return r(body);
      return r ?? { ok: true, value: undefined };
    },
  };
}
const PARAMS = { ok: true, value: { site_id: SITE, stretch_iterations: ITER, turnstile_site_key: null } };
const ME = { id: 1, login: "olga", slug: "olga", display_name: "Olga", bio: "", role: "owner" };
const LOGIN_OPTS = { ok: true, value: { publicKey: { challenge: enc("login-chal"), rpId: "blog.example", allowCredentials: [] } } };
const REG_OPTS = {
  ok: true,
  value: { publicKey: { challenge: enc("reg-chal"), rp: { id: "b" }, user: { id: enc("u"), name: "olga" }, excludeCredentials: [] } },
};

/** navigator.credentials whose get/create answer with `get` / `create` (a value or a throw). */
function fakeCredentials({ get, create } = {}) {
  const seen = [];
  const answer = (a, opts, kind) => {
    seen.push({ kind, opts });
    if (a instanceof Error) return Promise.reject(a);
    return Promise.resolve(a);
  };
  globalThis.PublicKeyCredential = function PublicKeyCredential() {};
  Object.defineProperty(globalThis, "navigator", {
    configurable: true,
    writable: true,
    value: { credentials: { get: (o) => answer(get, o, "get"), create: (o) => answer(create, o, "create") } },
  });
  return seen;
}

test("signIn sends the stretch of the password, salted with the login, never the password", async () => {
  const api = fakeApi({ "GET /api/auth/params": PARAMS, "POST /api/auth/login": { ok: true, value: { user: ME } } });
  const r = await flows.signIn(api, "Olga", "correct horse \u00e9", "tok");
  assert.deepEqual(r, { ok: true, value: { user: ME } });
  const login = api.calls.find((c) => c.path === "/api/auth/login");
  assert.deepEqual(login.body, { login: "Olga", password: expected("olga", "correct horse \u00e9"), turnstile_token: "tok" });
  assert.ok(!JSON.stringify(api.calls).includes("correct horse"));
});

test("params are fetched once; a failure is retried, not cached", async () => {
  let n = 0;
  const api = fakeApi({ "GET /api/auth/params": () => (++n === 1 ? { ok: false, error: { code: "network", status: 0 } } : PARAMS) });
  assert.equal((await flows.params(api)).ok, false);
  assert.equal((await flows.params(api)).ok, true);
  assert.equal((await flows.params(api)).ok, true);
  assert.equal(n, 2);
});

test("a server stretching differently fails before any password leaves", async () => {
  const api = fakeApi({ "GET /api/auth/params": { ok: true, value: { ...PARAMS.value, stretch_iterations: 1000 } } });
  const r = await flows.signIn(api, "olga", "pw");
  assert.equal(r.ok, false);
  assert.equal(r.error.code, "bad_response");
  assert.equal(api.calls.length, 1);
});

test("setup and acceptInvite stretch with the new login", async () => {
  for (const [fn, path] of [[flows.setup, "/api/auth/setup"], [flows.acceptInvite, "/api/auth/invite/accept"]]) {
    const api = fakeApi({ "GET /api/auth/params": PARAMS, [`POST ${path}`]: { ok: true, value: { user: ME } } });
    const f = { code: "c0de", login: "NewOne", slug: "new-one", display_name: "New", password: "secret pw" };
    assert.equal((await fn(api, f)).ok, true);
    const call = api.calls.find((c) => c.path === path);
    assert.deepEqual(call.body, { code: "c0de", login: "NewOne", slug: "new-one", display_name: "New", password: expected("newone", "secret pw") });
  }
});

test("signInPasskey: options, browser prompt, verify with the assertion fields", async () => {
  const seen = fakeCredentials({ get: fakeAssertion("u1") });
  const api = fakeApi({ "POST /api/auth/passkey/login/options": LOGIN_OPTS, "POST /api/auth/passkey/login/verify": { ok: true, value: { user: ME } } });
  const r = await flows.signInPasskey(api);
  assert.deepEqual(r, { ok: true, value: { user: ME } });
  assert.equal(text(seen[0].opts.publicKey.challenge), "login-chal");
  assert.deepEqual(api.calls[1].body, assertion(fakeAssertion("u1")));
});

test("signInPasskey sends a Turnstile token to options only (#63: tokens are single use)", async () => {
  fakeCredentials({ get: fakeAssertion("u1") });
  const api = fakeApi({ "POST /api/auth/passkey/login/options": LOGIN_OPTS, "POST /api/auth/passkey/login/verify": { ok: true, value: { user: ME } } });
  await flows.signInPasskey(api, "tok");
  assert.deepEqual(api.calls[0].body, { turnstile_token: "tok" });
  assert.equal(api.calls[1].body.turnstile_token, undefined);
});

test("a dismissed or timed-out passkey prompt is cancelled, other failures bad_response", async () => {
  for (const [name, code] of [["NotAllowedError", "cancelled"], ["AbortError", "cancelled"], ["SecurityError", "bad_response"]]) {
    fakeCredentials({ get: new DOMException("x", name) });
    const api = fakeApi({ "POST /api/auth/passkey/login/options": LOGIN_OPTS });
    const r = await flows.signInPasskey(api);
    assert.equal(r.ok, false);
    assert.equal(r.error.code, code, name);
    assert.equal(api.calls.length, 1, "nothing verified after a failed prompt");
  }
});

test("no WebAuthn is unsupported, before any request", async () => {
  delete globalThis.PublicKeyCredential;
  const api = fakeApi();
  for (const r of [await flows.signInPasskey(api), await flows.addPasskey(api, "k", "passkey")]) {
    assert.equal(r.error.code, "unsupported");
  }
  assert.equal(api.calls.length, 0);
});

test("addPasskey with the password: current_password is the stretch, plus attestation and name", async () => {
  const seen = fakeCredentials({ create: { response: { clientDataJSON: buf("c"), attestationObject: buf("o") } } });
  const pk = { id: 4, name: "Phone", created_at: 1, last_used_at: null };
  const api = fakeApi({
    "GET /api/auth/params": PARAMS,
    "POST /api/auth/passkey/register/options": REG_OPTS,
    "POST /api/auth/passkey/register/verify": { ok: true, value: { passkey: pk } },
  });
  const r = await flows.addPasskey(api, "Phone", { login: "Olga", password: "pw 1" });
  assert.deepEqual(r, { ok: true, value: { passkey: pk } });
  assert.equal(text(seen[0].opts.publicKey.challenge), "reg-chal");
  const verify = api.calls.at(-1);
  assert.deepEqual(verify.body, { name: "Phone", client_data_json: enc("c"), attestation_object: enc("o"), current_password: expected("olga", "pw 1") });
  assert.ok(!JSON.stringify(api.calls).includes("pw 1"));
});

test("addPasskey with a passkey: reauth_ assertion from a login challenge, then create", async () => {
  const seen = fakeCredentials({ get: fakeAssertion("u1"), create: { response: { clientDataJSON: buf("c"), attestationObject: buf("o") } } });
  const api = fakeApi({
    "POST /api/auth/passkey/login/options": LOGIN_OPTS,
    "POST /api/auth/passkey/register/options": REG_OPTS,
    "POST /api/auth/passkey/register/verify": { ok: true, value: { passkey: {} } },
  });
  assert.equal((await flows.addPasskey(api, "Key", "passkey")).ok, true);
  assert.deepEqual(seen.map((s) => s.kind), ["get", "create"]);
  assert.deepEqual(api.calls.map((c) => c.path), [
    "/api/auth/passkey/login/options",
    "/api/auth/passkey/register/options",
    "/api/auth/passkey/register/verify",
  ]);
  const body = api.calls.at(-1).body;
  assert.deepEqual(body, { name: "Key", client_data_json: enc("c"), attestation_object: enc("o"), ...assertion(fakeAssertion("u1"), "reauth_") });
  assert.ok(!("current_password" in body));
});

test("addPasskey cancelled at the proof makes no key and sends nothing more", async () => {
  const seen = fakeCredentials({ get: new DOMException("x", "NotAllowedError") });
  const api = fakeApi({ "POST /api/auth/passkey/login/options": LOGIN_OPTS });
  const r = await flows.addPasskey(api, "Key", "passkey");
  assert.equal(r.error.code, "cancelled");
  assert.deepEqual(seen.map((s) => s.kind), ["get"]);
  assert.equal(api.calls.length, 1);
});

test("removePasskey sends DELETE with the re-auth fields of either kind", async () => {
  const api = fakeApi({ "GET /api/auth/params": PARAMS });
  assert.equal((await flows.removePasskey(api, 9, { login: "olga", password: "pw" })).ok, true);
  assert.deepEqual(api.calls.at(-1), { method: "DELETE", path: "/api/auth/passkey/9", body: { current_password: expected("olga", "pw") } });

  fakeCredentials({ get: fakeAssertion() });
  const api2 = fakeApi({ "POST /api/auth/passkey/login/options": LOGIN_OPTS });
  assert.equal((await flows.removePasskey(api2, 9, "passkey")).ok, true);
  assert.deepEqual(api2.calls.at(-1), { method: "DELETE", path: "/api/auth/passkey/9", body: assertion(fakeAssertion(), "reauth_") });
});

test("a Turnstile token rides with the re-auth proof (REAUTH_FIELDS has turnstile_token)", async () => {
  const api = fakeApi({ "GET /api/auth/params": PARAMS });
  assert.equal((await flows.removePasskey(api, 9, { login: "olga", password: "pw" }, "tok")).ok, true);
  assert.deepEqual(api.calls.at(-1).body, { current_password: expected("olga", "pw"), turnstile_token: "tok" });
});

test("listPasskeys unwraps the list", async () => {
  const api = fakeApi({ "GET /api/auth/passkey": { ok: true, value: { passkeys: [{ id: 1 }] } } });
  assert.deepEqual(await flows.listPasskeys(api), { ok: true, value: [{ id: 1 }] });
});

test("signOut clears this user's drafts only after the server signed out", async () => {
  const rows = draftRows();
  fakeIndexedDB(rows);
  const down = fakeApi({ "POST /api/auth/logout": { ok: false, error: { code: "network", status: 0 } } });
  assert.equal((await flows.signOut(down, 1)).ok, false);
  assert.equal(rows.size, 4, "a failed sign-out keeps every draft");

  const api = fakeApi();
  assert.deepEqual(await flows.signOut(api, 1), { ok: true, value: undefined });
  assert.deepEqual(api.calls, [{ method: "POST", path: "/api/auth/logout", body: undefined }]);
  assert.deepEqual([...rows.keys()].sort(), ["10:post-3", "2:post-1"]);
});

test("flows never throw: a throwing api is a bad_response", async () => {
  const api = { call: () => Promise.reject(new Error("boom")) };
  const r = await flows.listPasskeys(api);
  assert.equal(r.error.code, "bad_response");
});

// --- client.ts call ---

const client = await bundle("web/admin/src/api/client.ts");

function fakeFetch(status, body) {
  const seen = [];
  globalThis.fetch = async (path, init) => {
    seen.push({ path, init });
    return new Response(body, { status });
  };
  return seen;
}

test("call sends JSON same-origin; 204 and empty bodies resolve to undefined", async () => {
  const api = client.httpApi();
  const seen = fakeFetch(204, null);
  assert.deepEqual(await api.call("POST", "/api/auth/logout"), { ok: true, value: undefined });
  assert.equal(seen[0].init.method, "POST");
  assert.equal(seen[0].init.credentials, "same-origin");
  assert.equal(seen[0].init.headers.accept, "application/json");
  assert.equal(seen[0].init.body, undefined);

  const seen2 = fakeFetch(200, '{"a":1}');
  assert.deepEqual(await api.call("PATCH", "/api/auth/me", { bio: "x", flag: true }), { ok: true, value: { a: 1 } });
  assert.equal(seen2[0].init.body, '{"bio":"x","flag":true}');
  assert.equal(seen2[0].init.headers["content-type"], "application/json");
});

test("call maps errors like getJson", async () => {
  const api = client.httpApi();
  fakeFetch(429, '{"error":"auth.locked"}');
  assert.deepEqual(await api.call("POST", "/api/auth/login", {}), { ok: false, error: { code: "auth.locked", status: 429 } });
  fakeFetch(500, "<html>");
  assert.deepEqual(await api.call("GET", "/x"), { ok: false, error: { code: "bad_response", status: 500 } });
  fakeFetch(404, "");
  assert.deepEqual(await api.call("DELETE", "/x"), { ok: false, error: { code: "bad_response", status: 404 } });
  globalThis.fetch = async () => {
    throw new TypeError("offline");
  };
  assert.deepEqual(await api.call("GET", "/x"), { ok: false, error: { code: "network", status: 0 } });
});

test("session.expired runs on a 401 outside /api/auth/ only", async () => {
  const api = client.httpApi();
  let n = 0;
  client.session.expired = () => n++;
  fakeFetch(401, '{"error":"auth.unauthenticated"}');
  await api.call("GET", "/api/auth/me");
  await api.me();
  assert.equal(n, 0);
  await api.call("GET", "/api/posts");
  await api.posts({});
  assert.equal(n, 2);
  fakeFetch(401, '{"error":"auth.invalid_credentials"}');
  await api.call("GET", "/api/posts");
  assert.equal(n, 2);
  client.session.expired = () => {
    throw new Error("hook broke");
  };
  fakeFetch(401, '{"error":"auth.unauthenticated"}');
  assert.equal((await api.posts({})).error.code, "auth.unauthenticated");
});
