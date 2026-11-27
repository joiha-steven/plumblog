// The admin's sign-in, setup, invite and account screens (web/admin/src/auth/) in the system
// Chrome against the real Worker: the built client, the real CSP, a real local D1.
//   fresh:  setup code + origin http://localhost:<port>, so passkeys work (an IP is not a valid rp id).
//   invited: an owner and a writer invite, to reach the account page as a writer.
// UI text is read from locales/en.toml, never hard-coded here.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync } from "node:fs";
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

let fresh, invited, browser;
// Where the page is opened: the same port, under the name the configured origin uses.
const at = (w, path = "") => `${w.url.replace("127.0.0.1", "localhost")}${path}`;

before(async () => {
  [fresh, invited] = await Promise.all([
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
  ]);
  browser = await chromium.launch({ channel: "chrome", headless: true });
});
after(async () => {
  await browser?.close();
  await Promise.all([fresh?.stop(), invited?.stop()]);
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
  // The posts list is not served yet (the admin home asks for it); any other 404 is a fault.
  page.on("response", (r) => r.status() === 404 && !r.url().endsWith("/api/posts") && problems.push(`404 ${r.url()}`));
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
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});
