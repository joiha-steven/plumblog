// The admin's sign-in, setup, invite and account screens (web/admin/src/auth/) in the system
// Chrome against the real Worker: the built client, the real CSP, a real local D1.
//   fresh:  setup code + origin http://localhost:<port>, so passkeys work (an IP is not a valid rp id).
//   invited: an owner and a writer invite, to reach the account page as a writer.
// UI text is read from locales/en.toml, never hard-coded here.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
import { createServer } from "node:http";
import { after, before, test } from "node:test";
import { chromium } from "playwright-core";
import { startWorker } from "./harness.mjs";

const sha256hex = (b) => createHash("sha256").update(b).digest("hex");
const sq = (s) => `'${String(s).replaceAll("'", "''")}'`;
const T = Date.now();
const DAY = 86_400_000;

// A flat reader for the string values of locales/en.toml ("section.key"); enough for these keys.
const en = (() => {
  const out = {};
  let section = "";
  for (const line of readFileSync(new URL("../../locales/en.toml", import.meta.url), "utf8").split("\n")) {
    const s = /^\[(.+)\]$/.exec(line);
    if (s) section = s[1];
    const kv = /^(\w+) = "((?:[^"\\]|\\.)*)"$/.exec(line);
    if (kv) out[`${section}.${kv[1]}`] = kv[2].replaceAll('\\"', '"');
    // Plural forms: key = { one = "...", other = "..." }.
    const pl = /^(\w+) = \{(.*)\}$/.exec(line);
    if (pl) for (const [, k, v] of pl[2].matchAll(/(\w+) = "((?:[^"\\]|\\.)*)"/g)) out[`${section}.${pl[1]}.${k}`] = v;
  }
  return out;
})();
const tx = (key) => {
  const v = en[key];
  assert.ok(v, `locale key ${key}`);
  return v;
};

const SITE = "site-ui-1";
const SETUP_CODE = "setup-code-ui-0123456789";
const OWNER = { login: "Hana", slug: "hana", name: "Hana Owner", password: "correct horse battery" };
const INVITE = "invite-writer-ui-code";
const WRITER = { login: "wren", slug: "wren", name: "Wren Writer", password: "writer pass phrase 1" };

// Cloudflare's always-pass test site key; siteverify is a local stub that passes its dummy token.
const TS_SITE_KEY = "1x00000000000000000000AA";
const TS_TOKEN = "XXXX.DUMMY.TOKEN.XXXX";

let fresh, invited, guarded, stub, browser;
// Where the page is opened: the same port, under the name the configured origin uses.
const at = (w, path = "") => `${w.url.replace("127.0.0.1", "localhost")}${path}`;

before(async () => {
  stub = createServer((req, res) => {
    let data = "";
    req.on("data", (d) => (data += d));
    req.on("end", () => {
      const ok = new URLSearchParams(data).get("response") === TS_TOKEN;
      res.setHeader("content-type", "application/json");
      res.end(JSON.stringify({ success: ok, hostname: "localhost", "error-codes": [] }));
    });
  });
  await new Promise((ok) => stub.listen(0, "127.0.0.1", ok));
  [fresh, invited, guarded] = await Promise.all([
    startWorker({
      seedSql: (url) => `INSERT INTO settings (key, value_json, updated_at) VALUES
        ('site_id', ${sq(JSON.stringify(SITE))}, ${T}),
        ('origin', ${sq(JSON.stringify(url.replace("127.0.0.1", "localhost")))}, ${T}),
        ('setup_code_hash', ${sq(JSON.stringify(sha256hex(SETUP_CODE)))}, ${T}),
        ('setup_code_expires_at', '${T + DAY}', ${T});`,
    }),
    startWorker({
      seedSql: `INSERT INTO settings (key, value_json, updated_at) VALUES ('site_id', ${sq(JSON.stringify("site-ui-2"))}, ${T});
        INSERT INTO users (id, login, slug, display_name, role, password_hash, disabled_at, created_at, updated_at)
          VALUES (1, 'boss', 'boss', 'Boss', 'owner', 'p1$1000$AAAAAAAAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA', NULL, ${T}, ${T});
        INSERT INTO invites (code_hash, role, created_by, created_at, expires_at)
          VALUES (${sq(sha256hex(INVITE))}, 'writer', 1, ${T}, ${T + DAY});`,
    }),
    // Turnstile on; the one user's stored hash matches no password, so every sign-in fails.
    startWorker({
      seedSql: (url) => `INSERT INTO settings (key, value_json, updated_at) VALUES
        ('site_id', ${sq(JSON.stringify("site-ui-3"))}, ${T}),
        ('origin', ${sq(JSON.stringify(url.replace("127.0.0.1", "localhost")))}, ${T}),
        ('turnstile_site_key', ${sq(JSON.stringify(TS_SITE_KEY))}, ${T});
        INSERT INTO users (id, login, slug, display_name, role, password_hash, disabled_at, created_at, updated_at)
          VALUES (1, 'boss', 'boss', 'Boss', 'owner', 'p1$1000$AAAAAAAAAAAAAAAAAAAAAA$AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA', NULL, ${T}, ${T});`,
      vars: { TURNSTILE_SECRET: "stub-secret", TURNSTILE_VERIFY_URL: `http://127.0.0.1:${stub.address().port}/verify` },
    }),
  ]);
  browser = await chromium.launch({ channel: "chrome", headless: true });
});
after(async () => {
  await browser?.close();
  await Promise.all([fresh?.stop(), invited?.stop(), guarded?.stop()]);
  stub?.close();
});

/** A page that records CSP violations, page errors and console errors (checked by done()). */
async function open(w) {
  const context = await browser.newContext({ viewport: { width: 1440, height: 900 }, locale: "en-US" });
  await context.addInitScript(() => {
    document.addEventListener("securitypolicyviolation", (e) =>
      console.error(`CSPVIOLATION ${e.violatedDirective} ${e.blockedURI}`),
    );
  });
  const page = await context.newPage();
  const problems = [];
  // Any 404 is a fault.
  page.on("response", (r) => r.status() === 404 && problems.push(`404 ${r.url()}`));
  page.on("pageerror", (e) => problems.push(`page error: ${e}`));
  page.on("console", (m) => {
    const text = m.text();
    // A refused API call (wrong password, signed out) logs by itself; that is not a client fault.
    if (m.type() === "error" && !/^Failed to load resource: the server responded with a status of 40[134]/.test(text)) {
      problems.push(`console: ${text}`);
    }
    if (/Content Security Policy|Refused to/.test(text)) problems.push(`csp: ${text}`);
  });
  return { context, page, problems };
}

const signInWith = async (page, login, password) => {
  await page.getByLabel(tx("admin.auth.login")).fill(login);
  await page.getByLabel(tx("admin.auth.password"), { exact: true }).fill(password);
  await page.getByRole("button", { name: tx("admin.auth.signin"), exact: true }).click();
};
const signOutViaMenu = async (page, name = OWNER.name) => {
  // The button is named after the signed-in user.
  await page.getByRole("button", { name: new RegExp(`^${name}`) }).click();
  await page.getByRole("button", { name: tx("admin.account.signout"), exact: true }).click();
  await page.waitForURL(/\/admin\/signin$/);
};

test("setup, sign out, sign in, passkeys, all through the UI", async () => {
  const { context, page, problems } = await open(fresh);
  try {
    // 1. Setup through the UI.
    await page.goto(at(fresh, `/admin/setup?code=${SETUP_CODE}`));
    await page.getByLabel(tx("admin.auth.login")).fill(OWNER.login);
    await page.getByLabel(tx("admin.auth.slug")).fill(OWNER.slug);
    await page.getByLabel(tx("admin.auth.display_name")).fill(OWNER.name);
    await page.getByLabel(tx("admin.auth.password"), { exact: true }).fill(OWNER.password);
    await page.getByLabel(tx("admin.auth.password_repeat")).fill(OWNER.password);
    await page.getByRole("button", { name: tx("admin.auth.create") }).click();
    await page.waitForURL(/\/admin\/$/);
    await page.locator("nav.side").waitFor();
    const users = fresh.query("SELECT login, role, password_hash FROM users");
    assert.equal(users.length, 1);
    assert.equal(users[0].login, OWNER.login);
    assert.equal(users[0].role, "owner");
    // The browser stretched the password: the server only ever saw (and stored) a derived value.
    assert.ok(!users[0].password_hash.includes(OWNER.password));
    assert.match(users[0].password_hash, /^p1\$/);

    // 2. Sign out from the account menu (no drafts here, so no question).
    await signOutViaMenu(page);
    assert.equal(await page.locator("h1").innerText(), tx("admin.auth.title_signin").replace("{site}", SITE));

    // 3. Wrong password: one plain message, no raw code.
    await signInWith(page, OWNER.login, "not the password at all");
    const alert = page.getByRole("alert");
    await alert.filter({ hasText: tx("admin.auth.err_invalid_credentials") }).waitFor();
    assert.equal((await alert.innerText()).trim(), tx("admin.auth.err_invalid_credentials"));
    assert.doesNotMatch(await page.locator("body").innerText(), /auth\.invalid_credentials/);
    assert.match(page.url(), /\/admin\/signin$/);

    // 4. A `next`: the signed-out writer is sent to sign-in and comes back.
    await page.goto(at(fresh, "/admin/posts/new"));
    await page.waitForURL(/\/admin\/signin\?next=/);
    assert.equal(new URL(page.url()).searchParams.get("next"), "/admin/posts/new");
    await signInWith(page, OWNER.login, OWNER.password);
    await page.waitForURL(/\/admin\/posts\/new$/);
    // The editor has no side nav or account menu; its toolbar proves the signed-in client booted.
    await page.getByRole("toolbar").waitFor();

    // 5. Bad `next` values fall back to the admin home.
    for (const next of ["https://evil.test/", "//evil.test"]) {
      await page.goto(at(fresh, "/admin/")); // the editor has no account menu
      await signOutViaMenu(page);
      await page.goto(at(fresh, `/admin/signin?next=${encodeURIComponent(next)}`));
      await signInWith(page, OWNER.login, OWNER.password);
      await page.waitForURL((u) => u.pathname === "/admin/" && u.hostname === "localhost", { timeout: 15_000 });
      await page.locator("nav.side").waitFor();
    }

    // 6. Passkey round trip with Chrome's virtual authenticator.
    const cdp = await context.newCDPSession(page);
    await cdp.send("WebAuthn.enable");
    await cdp.send("WebAuthn.addVirtualAuthenticator", {
      options: { protocol: "ctap2", transport: "internal", hasResidentKey: true, hasUserVerification: true, isUserVerified: true, automaticPresenceSimulation: true },
    });
    await page.goto(at(fresh, "/admin/account"));
    await page.getByText(tx("admin.account.passkeys_empty"), { exact: true }).waitFor();
    await page.getByRole("button", { name: tx("admin.account.add"), exact: true }).click();
    await page.locator("#pk-add-password").fill(OWNER.password);
    await page.getByRole("button", { name: tx("admin.account.add_submit"), exact: true }).click();
    await page.locator(".pk-name", { hasText: tx("admin.account.add_name_default") }).waitFor();
    assert.equal(await page.locator(".pk").count(), 1);
    assert.equal(fresh.query("SELECT count(*) AS n FROM passkeys")[0].n, 1);

    await signOutViaMenu(page);
    await page.getByRole("button", { name: tx("admin.auth.passkey") }).click();
    await page.waitForURL(/\/admin\/$/);
    await page.locator("nav.side").waitFor();

    await page.goto(at(fresh, "/admin/account"));
    await page.locator(".pk").waitFor();
    await page.getByRole("button", { name: tx("admin.account.remove"), exact: true }).click();
    await page.locator("#pk-remove-password").fill(OWNER.password);
    await page.getByRole("button", { name: tx("admin.account.remove_submit"), exact: true }).click();
    await page.getByText(tx("admin.account.passkeys_empty"), { exact: true }).waitFor();
    assert.equal(await page.locator(".pk").count(), 0);
    assert.equal(fresh.query("SELECT count(*) AS n FROM passkeys")[0].n, 0);

    // 8. No CSP violation and no console error on any page above.
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

test("a writer accepts an invite and the account page loads for them", async () => {
  const { context, page, problems } = await open(invited);
  try {
    await page.goto(at(invited, `/admin/invite?code=${INVITE}`));
    await page.getByLabel(tx("admin.auth.login")).fill(WRITER.login);
    await page.getByLabel(tx("admin.auth.slug")).fill(WRITER.slug);
    await page.getByLabel(tx("admin.auth.display_name")).fill(WRITER.name);
    await page.getByLabel(tx("admin.auth.password"), { exact: true }).fill(WRITER.password);
    await page.getByLabel(tx("admin.auth.password_repeat")).fill(WRITER.password);
    await page.getByRole("button", { name: tx("admin.auth.create") }).click();
    await page.waitForURL(/\/admin\/$/);
    await page.goto(at(invited, "/admin/account"));
    await page.locator("#main").getByText(WRITER.name, { exact: true }).waitFor();
    await page.locator("#main").getByText(tx("admin.role.writer"), { exact: true }).waitFor();
    await page.getByText(tx("admin.account.passkeys_empty"), { exact: true }).waitFor();

    // Signing out with drafts on this device asks first, then deletes this writer's only. The
    // count is one per post, as the posts list shows them: a side copy (another tab's) adds none.
    const me = invited.query(`SELECT id FROM users WHERE login = ${sq(WRITER.login)}`)[0].id;
    const keys = [`${me}:new-aaaaaaaaaaaa`, `${me}:post-5`, `${me}:post-5~abcdefgh12345678`, "1:new-bbbbbbbbbbbb"];
    await idb(page, "put", keys);
    await page.getByRole("button", { name: new RegExp(`^${WRITER.name}`) }).click();
    await page.getByRole("button", { name: tx("admin.account.signout"), exact: true }).click();
    const ask = tx("admin.account.signout_drafts.other").replace("{count}", "2");
    await page.getByText(ask, { exact: true }).waitFor();
    assert.deepEqual(await idb(page, "keys"), keys.toSorted());
    await page.getByRole("button", { name: tx("admin.account.signout_delete"), exact: true }).click();
    await page.waitForURL(/\/admin\/signin$/);
    assert.deepEqual(await idb(page, "keys"), ["1:new-bbbbbbbbbbbb"]);
    assert.equal(invited.query("SELECT count(*) AS n FROM sessions")[0].n, 0);
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

/** The admin's drafts store (web/admin/src/editor/drafts.ts): put drafts under `keys`, or list the keys. */
function idb(page, op, keys = []) {
  return page.evaluate(
    ([op, keys]) =>
      new Promise((resolve, reject) => {
        const req = indexedDB.open("plumblog-admin", 2);
        req.onupgradeneeded = () => req.result.createObjectStore("drafts");
        req.onerror = () => reject(req.error);
        req.onsuccess = () => {
          const db = req.result;
          const tx = db.transaction("drafts", op === "put" ? "readwrite" : "readonly");
          const store = tx.objectStore("drafts");
          let out;
          if (op === "put") for (const k of keys) store.put({ title: k, body: k, base: null, rev: 1, savedAt: 0 }, k);
          else store.getAllKeys().onsuccess = (e) => (out = e.target.result);
          tx.oncomplete = () => (db.close(), resolve(out));
          tx.onerror = () => reject(tx.error);
        };
      }),
    [op, keys],
  );
}

test("Turnstile: one sign-in request per submit, even when the widget solves itself again", async () => {
  const { context, page, problems } = await open(guarded);
  // Turnstile's script, replaced by one that acts like managed mode at its most eager: it
  // solves on render and on every reset, then again by itself (a refresh) shortly after.
  let solves = 0;
  await page.route("https://challenges.cloudflare.com/turnstile/v0/api.js*", (route) =>
    route.fulfill({
      contentType: "text/javascript",
      body: `window.turnstile = {
        render(el, o) { this.o = o; this.solve(); return "w1"; },
        reset() { this.solve(); },
        solve() { for (const ms of [50, 400]) setTimeout(() => { window.__solves = (window.__solves || 0) + 1; this.o.callback(${JSON.stringify(TS_TOKEN)}); }, ms); },
      };`,
    }),
  );
  const logins = [];
  page.on("request", (r) => r.url().endsWith("/api/auth/login") && logins.push(JSON.parse(r.postData() ?? "{}")));
  const submit = async () => {
    await page.getByRole("button", { name: tx("admin.auth.signin"), exact: true }).click();
    await page.getByRole("alert").filter({ hasText: /./ }).waitFor();
    await page.getByRole("button", { name: tx("admin.auth.signin"), exact: true }).and(page.locator(":enabled")).waitFor();
  };
  const quiet = () => page.waitForTimeout(1500);
  try {
    await page.goto(at(guarded, "/admin/signin"));
    await page.getByLabel(tx("admin.auth.login")).fill("boss");
    await page.getByLabel(tx("admin.auth.password"), { exact: true }).fill("a typo");
    // Five wrong passwords: one request each, no Turnstile yet.
    for (let i = 1; i <= 5; i++) {
      await submit();
      assert.equal(logins.length, i);
    }
    // The sixth is refused for want of a token: the widget shows and solves itself, and nothing
    // is sent until the writer submits again.
    await submit();
    await page.getByText(tx("admin.auth.err_turnstile"), { exact: true }).waitFor();
    await quiet();
    assert.equal(logins.length, 6);
    assert.ok(!("turnstile_token" in logins[5]));
    // Each later submit sends exactly one request with a token, and the widget's own re-solves
    // never send another.
    for (let i = 7; i <= 9; i++) {
      await submit();
      await page.getByText(tx("admin.auth.err_invalid_credentials"), { exact: true }).waitFor();
      await quiet();
      assert.equal(logins.length, i, `requests after submit ${i}`);
      assert.equal(logins[i - 1].turnstile_token, TS_TOKEN);
    }
    solves = await page.evaluate(() => window.__solves);
    assert.ok(solves >= 4, `the widget solved itself ${solves} times`);
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});
