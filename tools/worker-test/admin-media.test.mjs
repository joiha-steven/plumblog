// The media library screen (web/admin/src/media/) in the system Chrome against the real Worker,
// a real local D1 and R2: the grid, the filters, editing a description, the viewer, delete (free,
// in use, not yours) and the empty and failing states. Pictures are uploaded through the API as
// the users, so the rows, R2 objects and `uploaded_by` are the real ones. UI text comes from
// locales/en.toml, never hard-coded here.
import assert from "node:assert/strict";
import { createHash, randomBytes } from "node:crypto";
import { readFileSync } from "node:fs";
import { deflateSync, crc32 } from "node:zlib";
import { after, before, test } from "node:test";
import { build } from "esbuild";
import { chromium } from "playwright-core";
import { startWorker } from "./harness.mjs";

const COOKIE = "__Host-plumblog_session";
const DAY = 86_400_000;
const T = Date.now();
const sq = (s) => `'${String(s).replaceAll("'", "''")}'`;
const sha = (s) => createHash("sha256").update(s).digest("hex");
const token = () => randomBytes(32).toString("base64url");
const S = { owner: token(), will: token(), wren: token() };

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
  assert.ok(en[key], `locale key ${key}`);
  return en[key];
};

/** A plain-colour PNG the browser can decode. */
function png(width, height, [r, g, b]) {
  const chunk = (type, data) => {
    const head = Buffer.alloc(8);
    head.writeUInt32BE(data.length);
    head.write(type, 4, "latin1");
    const crc = Buffer.alloc(4);
    crc.writeUInt32BE(crc32(Buffer.concat([head.subarray(4), data])));
    return Buffer.concat([head, data, crc]);
  };
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(width, 0);
  ihdr.writeUInt32BE(height, 4);
  ihdr.set([8, 2, 0, 0, 0], 8);
  const row = Buffer.concat([Buffer.from([0]), Buffer.from(Array.from({ length: width }, () => [r, g, b]).flat())]);
  const raw = Buffer.concat(Array.from({ length: height }, () => row));
  return Buffer.concat([Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), chunk("IHDR", ihdr), chunk("IDAT", deflateSync(raw)), chunk("IEND", Buffer.alloc(0))]);
}

const seedFor = (url) => `
  INSERT INTO users (id, login, slug, display_name, role, password_hash, disabled_at, created_at, updated_at) VALUES
    (1, 'olga', 'olga', 'Olga', 'owner', NULL, NULL, ${T}, ${T}),
    (2, 'will', 'will', 'Will', 'writer', NULL, NULL, ${T}, ${T}),
    (3, 'wren', 'wren', 'Wren', 'writer', NULL, NULL, ${T}, ${T});
  INSERT INTO sessions (token_hash, user_id, created_at, expires_at) VALUES
    (${sq(sha(S.owner))}, 1, ${T}, ${T + 29 * DAY}),
    (${sq(sha(S.will))}, 2, ${T}, ${T + 29 * DAY}),
    (${sq(sha(S.wren))}, 3, ${T}, ${T + 29 * DAY});
  INSERT INTO settings (key, value_json, updated_at) VALUES
    ('site_id', ${sq(JSON.stringify("site-media-1"))}, ${T}),
    ('origin', ${sq(JSON.stringify(url.replace("127.0.0.1", "localhost")))}, ${T});`;

let w, w2, w3, browser;
const at = (worker, path = "") => `${worker.url.replace("127.0.0.1", "localhost")}${path}`;

async function api(worker, who, method, path, { body, raw } = {}) {
  const headers = { origin: at(worker), cookie: `${COOKIE}=${S[who]}` };
  if (body !== undefined) headers["content-type"] = "application/json";
  const res = await fetch(worker.url + path, { method, headers, body: body === undefined ? raw : JSON.stringify(body) });
  const text = await res.text();
  return { status: res.status, json: text ? JSON.parse(text) : null };
}

const PICTURES = [
  // who, name, alt, [w, h, rgb]
  ["owner", "harbour.png", "Harbour at dawn", [64, 48, [196, 86, 31]]],
  ["will", "quiet.png", "", [48, 64, [95, 122, 98]]],
  ["will", "lamp.png", "A lamp", [64, 64, [154, 166, 173]]],
  ["wren", "wren.png", "", [80, 40, [138, 90, 60]]],
];

async function upload(worker, who, name, alt, [width, height, rgb]) {
  const q = new URLSearchParams({ name, mime: "image/png", width, height, ...(alt ? { alt } : {}) });
  const r = await api(worker, who, "POST", `/api/v1/media?${q}`, { raw: png(width, height, rgb) });
  assert.equal(r.status, 201, JSON.stringify(r.json));
  return r.json;
}

before(async () => {
  w = await startWorker({ seedSql: seedFor });
  for (const [who, name, alt, size] of PICTURES) await upload(w, who, name, alt, size);
  // harbour.png (id 1) is in use: a post whose body shows it.
  const post = await api(w, "owner", "POST", "/api/v1/posts", { body: { lang: "en", slug: "harbour", title: "Harbour", markdown: "![Harbour](/media/1/harbour.png)\n" } });
  assert.equal(post.status, 201, JSON.stringify(post.json));
  browser = await chromium.launch({ channel: "chrome", headless: true });
});
after(async () => {
  await browser?.close();
  await w?.stop();
  await w2?.stop();
  await w3?.stop();
});

/** A page signed in as `who`; `problems` collects page errors and CSP violations (expected 4xx/5xx logs are not). */
async function open(worker, who, path = "/admin/media", { width = 1440, height = 900, allow = null, setup = null } = {}) {
  const context = await browser.newContext({ viewport: { width, height }, locale: "en-US", timezoneId: "UTC" });
  await context.addCookies([{ name: COOKIE, value: S[who], domain: "localhost", path: "/", secure: true, httpOnly: true, sameSite: "Lax" }]);
  await context.addInitScript(() => {
    document.addEventListener("securitypolicyviolation", (e) => console.error(`CSPVIOLATION ${e.violatedDirective} ${e.blockedURI}`));
  });
  const page = await context.newPage();
  const problems = [];
  const logged = [];
  page.on("pageerror", (e) => problems.push(`page error: ${e}`));
  page.on("console", (m) => {
    if (m.type() !== "error" || /^Failed to load resource: the server responded with a status of (403|409|500)/.test(m.text())) return;
    // Failures a test causes on purpose are logged by the client (never silent); the test checks the log.
    (allow?.test(m.text()) ? logged : problems).push(`console: ${m.text()} ${m.location().url}`);
  });
  await setup?.(page);
  await page.goto(at(worker, path));
  return { context, page, problems, logged };
}

const names = (page) => page.locator(".tile .fname").allInnerTexts();
const tileOf = (page, name) => page.locator(".tile", { hasText: name });
const row = (id) => w.query(`SELECT id, alt, uploaded_by FROM media WHERE id = ${id}`)[0];

test("the grid shows every picture, newest first, with a lamp on those without a description", async () => {
  const { context, page, problems } = await open(w, "owner");
  try {
    await page.locator(".tile").first().waitFor();
    assert.deepEqual(await names(page), ["wren.png", "lamp.png", "quiet.png", "harbour.png"]);
    assert.deepEqual(await page.locator(".tile .badge-na").allInnerTexts(), [tx("admin.media.no_alt"), tx("admin.media.no_alt")]);
    assert.equal(await tileOf(page, "wren.png").locator(".badge-na").count(), 1);
    assert.equal(await tileOf(page, "lamp.png").locator(".badge-na").count(), 0);
    assert.match(await tileOf(page, "harbour.png").locator(".fmeta").innerText(), /byte|kB/);
    assert.match(await tileOf(page, "harbour.png").locator(".fmeta").innerText(), /64\u00d748$/);
    // Every <img> reserves its box and loads late; the pictures really come from /media/ for a signed-in user.
    const imgs = await page.locator(".tile img").evaluateAll((all) => all.map((i) => ({ w: i.getAttribute("width"), h: i.getAttribute("height"), lazy: i.loading, src: i.getAttribute("src"), ok: i.complete })));
    assert.equal(imgs.length, 4);
    for (const i of imgs) assert.ok(i.w && i.h && i.lazy === "lazy" && /^\/media\/\d+\/[a-z]+\.png$/.test(i.src), JSON.stringify(i));
    await page.waitForFunction(() => [...document.querySelectorAll(".tile img")].every((i) => i.complete && i.naturalWidth > 0));
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

test("the filters narrow the grid, ask the API for it and live in the address", async () => {
  const { context, page, problems } = await open(w, "owner");
  try {
    await page.locator(".tile").first().waitFor();
    const asked = [];
    page.on("request", (r) => /\/api\/v1\/media\?/.test(r.url()) && asked.push(new URL(r.url()).searchParams.toString()));
    const filter = (name) => page.locator(".seg button", { hasText: new RegExp(`^${name}$`) });
    await filter(tx("admin.media.no_alt")).click();
    await page.waitForFunction(() => document.querySelectorAll(".tile").length === 2);
    assert.deepEqual(await names(page), ["wren.png", "quiet.png"]);
    assert.equal(new URL(page.url()).searchParams.get("show"), "missing_alt");
    await filter(tx("admin.media.unused")).click();
    await page.waitForFunction(() => document.querySelectorAll(".tile").length === 3);
    assert.deepEqual(await names(page), ["wren.png", "lamp.png", "quiet.png"]);
    await filter(tx("admin.media.all")).click();
    await page.waitForFunction(() => document.querySelectorAll(".tile").length === 4);
    assert.equal(new URL(page.url()).searchParams.get("show"), null);
    assert.deepEqual(asked, ["limit=24&missing_alt=true", "limit=24&unused=true", "limit=24"]);
    // A reload on a filtered address opens that filter.
    await page.goto(at(w, "/admin/media?show=unused"));
    await page.waitForFunction(() => document.querySelectorAll(".tile").length === 3);
    assert.equal(await page.locator(".seg button[aria-pressed=true]").innerText(), tx("admin.media.unused"));
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

test("the details card shows size, type, dimensions and where a picture is used", async () => {
  const { context, page, problems } = await open(w, "owner");
  try {
    await tileOf(page, "harbour.png").click();
    const card = page.locator(".insp-card");
    await card.locator(".kv").waitFor();
    const facts = await card.locator(".kv").innerText();
    assert.match(facts, /PNG/);
    assert.match(facts, /64\u00d748/);
    const postLink = card.locator(".kv a.chip");
    await postLink.waitFor();
    assert.equal(await postLink.innerText(), "Post #1");
    assert.equal(await postLink.getAttribute("href"), "/admin/posts/1");
    assert.equal(await card.locator("#alt-1").inputValue(), "Harbour at dawn");
    assert.equal(await card.locator(".stage img").getAttribute("width"), "64");
    // An unused picture says so.
    await tileOf(page, "wren.png").click();
    await page.locator(".kv", { hasText: tx("admin.media.unused_note") }).waitFor();
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

test("a description is saved through the API, shows in the grid and survives a reload", async () => {
  const { context, page, problems } = await open(w, "owner");
  try {
    await tileOf(page, "quiet.png").click();
    const save = page.getByRole("button", { name: tx("admin.media.save") });
    await save.waitFor();
    assert.equal(await save.isDisabled(), true, "nothing to save yet");
    await page.getByLabel(tx("admin.media.alt"), { exact: true }).fill("A quiet corner of the harbour");
    assert.equal(await save.isDisabled(), false);
    await save.click();
    await page.locator(".toast", { hasText: tx("admin.media.saved") }).waitFor();
    assert.equal(row(2).alt, "A quiet corner of the harbour");
    assert.equal(await tileOf(page, "quiet.png").locator(".badge-na").count(), 0, "the lamp is gone from the grid");
    assert.equal(await save.isDisabled(), true);
    await page.reload();
    await page.locator(".tile").first().waitFor();
    assert.equal(await tileOf(page, "quiet.png").locator(".badge-na").count(), 0);
    // Emptying it again puts the lamp back.
    await tileOf(page, "quiet.png").click();
    await page.getByLabel(tx("admin.media.alt"), { exact: true }).fill("");
    await page.getByRole("button", { name: tx("admin.media.save") }).click();
    await page.locator(".toast", { hasText: tx("admin.media.saved") }).waitFor();
    assert.equal(row(2).alt, "");
    assert.equal(await tileOf(page, "quiet.png").locator(".badge-na").count(), 1);
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

test("Esc closes the details and the viewer, and focus goes back to the picture", async () => {
  const { context, page, problems } = await open(w, "owner");
  try {
    const focused = () => page.evaluate(() => ({ cls: document.activeElement?.className ?? "", text: document.activeElement?.textContent ?? "", pressed: document.activeElement?.getAttribute("aria-pressed") }));
    await tileOf(page, "lamp.png").click();
    await page.locator(".insp-card").waitFor();
    assert.equal(await tileOf(page, "lamp.png").getAttribute("aria-pressed"), "true");
    await page.keyboard.press("Escape");
    await page.locator(".insp-card").waitFor({ state: "detached" });
    const f = await focused();
    assert.ok(f.cls.includes("tile") && f.text.includes("lamp.png") && f.pressed === "false", JSON.stringify(f));
    // The viewer opens from the eye button, steps with the arrows and closes with Esc.
    const eye = page.getByRole("button", { name: tx("admin.media.view").replace("{name}", "lamp.png") });
    await eye.focus();
    await eye.press("Enter");
    const lb = page.locator(".lb");
    await lb.waitFor();
    assert.equal(await lb.locator(".lb-name b").innerText(), "lamp.png");
    await page.keyboard.press("ArrowRight");
    assert.equal(await lb.locator(".lb-name b").innerText(), "quiet.png");
    assert.equal(await lb.locator(".lb-fig img").getAttribute("width"), "48");
    await lb.locator(".lb-na").waitFor();
    await page.keyboard.press("ArrowLeft");
    await page.keyboard.press("ArrowLeft");
    assert.equal(await lb.locator(".lb-name b").innerText(), "wren.png");
    assert.equal(await lb.locator(".lb-prev").isDisabled(), true, "nothing before the first picture");
    await page.keyboard.press("Escape");
    await lb.waitFor({ state: "detached" });
    assert.equal(await page.evaluate(() => document.activeElement?.getAttribute("aria-label")), tx("admin.media.view").replace("{name}", "lamp.png"));
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

test("deleting a free picture removes it from the grid, D1 and R2", async () => {
  const { context, page, problems } = await open(w, "owner");
  try {
    const key = w.query("SELECT r2_key FROM media WHERE id = 4")[0].r2_key;
    assert.ok(w.r2Size(key) > 0, "the object is there");
    await tileOf(page, "wren.png").click();
    await page.getByRole("button", { name: tx("admin.media.delete") }).click();
    // Asked first; Cancel leaves it be.
    await page.locator(".confirm", { hasText: tx("admin.media.delete_q") }).waitFor();
    await page.getByRole("button", { name: tx("admin.media.cancel") }).click();
    assert.equal(w.query("SELECT COUNT(*) AS n FROM media WHERE id = 4")[0].n, 1);
    await page.getByRole("button", { name: tx("admin.media.delete") }).click();
    await page.locator(".confirm .btn.danger").click();
    await page.locator(".toast", { hasText: tx("admin.media.deleted") }).waitFor();
    assert.deepEqual(await names(page), ["lamp.png", "quiet.png", "harbour.png"]);
    assert.equal(await page.locator(".insp-card").count(), 0, "the details close");
    assert.equal(w.query("SELECT COUNT(*) AS n FROM media WHERE id = 4")[0].n, 0);
    assert.equal(w.r2Size(key), null, "the object is gone from R2");
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

test("deleting a picture that a post uses is refused and names the posts", async () => {
  const { context, page, problems } = await open(w, "owner");
  try {
    await tileOf(page, "harbour.png").click();
    await page.getByRole("button", { name: tx("admin.media.delete") }).click();
    await page.locator(".confirm .btn.danger").click();
    const note = page.locator(".del-note");
    await note.getByText(tx("admin.media.err_used")).waitFor();
    assert.equal(await note.locator("a.chip").getAttribute("href"), "/admin/posts/1");
    assert.equal(await note.locator("a.chip").innerText(), "Post #1");
    assert.equal(w.query("SELECT COUNT(*) AS n FROM media WHERE id = 1")[0].n, 1);
    assert.equal((await names(page)).includes("harbour.png"), true);
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

test("a writer cannot delete another's picture (no button); the server's 403 is shown when it still refuses", async () => {
  // Wren did not upload quiet.png (Will did): the button is off and says why.
  const wren = await open(w, "wren");
  try {
    await tileOf(wren.page, "quiet.png").click();
    const del = wren.page.getByRole("button", { name: tx("admin.media.delete") });
    await del.waitFor();
    assert.equal(await del.isDisabled(), true);
    await wren.page.getByText(tx("admin.media.err_forbidden")).waitFor();
    // The server agrees.
    assert.equal((await api(w, "wren", "DELETE", "/api/v1/media/2")).status, 403);
    assert.deepEqual(wren.problems, []);
  } finally {
    await wren.context.close();
  }
  // Will opens his own lamp.png and the picture is handed to someone else before he confirms
  // (the page still believes it is his): the server says no and the page shows it.
  const will = await open(w, "will");
  try {
    await tileOf(will.page, "lamp.png").click();
    await will.page.getByRole("button", { name: tx("admin.media.delete") }).click();
    w.exec("UPDATE media SET uploaded_by = 1 WHERE id = 3");
    await will.page.locator(".confirm .btn.danger").click();
    await will.page.locator(".del-note .err", { hasText: tx("admin.media.err_forbidden") }).waitFor();
    assert.equal(row(3).uploaded_by, 1, "still there");
    assert.equal((await names(will.page)).includes("lamp.png"), true);
    // His own picture goes.
    await tileOf(will.page, "quiet.png").click();
    await will.page.getByRole("button", { name: tx("admin.media.delete") }).click();
    await will.page.locator(".confirm .btn.danger").click();
    await will.page.locator(".toast", { hasText: tx("admin.media.deleted") }).waitFor();
    assert.equal(w.query("SELECT COUNT(*) AS n FROM media WHERE id = 2")[0].n, 0);
    assert.deepEqual(will.problems, []);
  } finally {
    await will.context.close();
  }
});

test("a failing API shows an error with Try again, not a blank screen; a slow one shows placeholders", async () => {
  const { context, page, problems, logged } = await open(w, "owner", "/admin/", { allow: /loading the library failed|api: GET \/api\/v1\/media|net::ERR_FAILED/ });
  try {
    await page.locator("h1").waitFor();
    await page.route("**/api/v1/media?**", (r) => r.fulfill({ status: 500, contentType: "application/json", body: '{"error":"internal"}' }));
    await page.locator("nav.side a", { hasText: tx("admin.nav.media") }).click();
    const alert = page.locator(".media [role=alert]");
    await alert.getByText(tx("admin.state.error_title")).waitFor();
    assert.equal(await page.locator(".media .tile").count(), 0);
    // Offline: the network message.
    await page.unroute("**/api/v1/media?**");
    await page.route("**/api/v1/media?**", (r) => r.abort());
    await alert.getByRole("button", { name: tx("admin.state.retry") }).click();
    await alert.getByText(tx("admin.state.network_title")).waitFor();
    // Back up, but slow: placeholders first, then the pictures.
    await page.unroute("**/api/v1/media?**");
    let release;
    const held = new Promise((r) => (release = r));
    await page.route("**/api/v1/media?**", async (r) => {
      await held;
      await r.continue();
    });
    await alert.getByRole("button", { name: tx("admin.state.retry") }).click();
    await page.locator(".grid[aria-busy] .tile-ph").first().waitFor();
    release();
    await page.locator(".tile").first().waitFor();
    assert.equal(await page.locator(".grid[aria-busy]").count(), 0);
    // Not silent: the next agent finds the cause in the console.
    assert.ok(logged.some((l) => /loading the library failed: internal \(500\)/.test(l)), logged.join("\n"));
    assert.ok(logged.some((l) => /loading the library failed: network \(0\)/.test(l)), logged.join("\n"));
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

test("an empty library shows the empty state, and so does a filter with nothing in it", async () => {
  w2 = await startWorker({ seedSql: seedFor });
  const { context, page, problems } = await open(w2, "owner");
  try {
    await page.locator(".media .empty").getByText(tx("admin.media.empty"), { exact: true }).waitFor();
    await page.locator(".media .empty").getByText(tx("admin.media.empty_hint")).waitFor();
    assert.equal(await page.locator(".tile").count(), 0);
    await page.locator(".seg button", { hasText: tx("admin.media.no_alt") }).click();
    await page.locator(".media .empty").getByText(tx("admin.media.empty_alt")).waitFor();
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

test("Load more asks for the next page and adds to the grid", async () => {
  // 26 pictures exist after this: a first page of 24 and "Load more" for the other two.
  for (let i = 0; i < 26; i++) await upload(w2, "owner", `p${String(i).padStart(2, "0")}.png`, "x", [8, 8, [i * 9, 90, 120]]);
  const { context, page, problems } = await open(w2, "owner");
  try {
    await page.waitForFunction(() => document.querySelectorAll(".tile").length === 24);
    await page.getByRole("button", { name: tx("admin.list.more") }).click();
    await page.waitForFunction(() => document.querySelectorAll(".tile").length === 26);
    assert.equal(await page.getByRole("button", { name: tx("admin.list.more") }).count(), 0, "no more pages");
    assert.equal((await names(page))[0], "p25.png", "newest first");
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

// ---- the rest run on the second Worker (26 pictures, all the owner's) ----

test("closing the viewer returns focus to what opened it, also from the card at 375 px, and a route change removes it", async () => {
  const { context, page, problems } = await open(w2, "owner", "/admin/media", { width: 375, height: 812 });
  try {
    await page.locator(".tile").first().waitFor();
    await page.locator(".tile").first().click();
    const stage = page.locator(".stage");
    await stage.waitFor();
    await stage.click();
    await page.locator(".lb").waitFor();
    await page.keyboard.press("ArrowRight");
    await page.keyboard.press("Escape");
    await page.locator(".lb").waitFor({ state: "detached" });
    assert.equal(await page.evaluate(() => document.activeElement?.className), "stage", "back on the card's picture");
    // Esc again closes the card; focus is on the tile.
    await page.keyboard.press("Escape");
    await page.locator(".insp-card").waitFor({ state: "detached" });
    assert.equal(await page.evaluate(() => document.activeElement?.className), "tile");
    // The viewer does not outlive its screen.
    await page.locator(".peek").first().click();
    await page.locator(".lb").waitFor();
    await page.evaluate(() => {
      history.pushState(null, "", "/admin/pages");
      dispatchEvent(new PopStateEvent("popstate"));
    });
    await page.locator("h1", { hasText: tx("admin.nav.pages") }).waitFor();
    await page.locator(".lb").waitFor({ state: "detached" });
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

/** A request the test can hold until it says go. */
function gate() {
  let go;
  const open = new Promise((r) => (go = r));
  return { open, go };
}

test("a delete in flight belongs to its picture: another open card stays; a refusal on a closed card is a toast", async () => {
  const free = await upload(w2, "owner", "free.png", "x", [8, 8, [9, 9, 9]]);
  const busy = await upload(w2, "owner", "busy.png", "x", [8, 8, [7, 7, 7]]);
  const post = await api(w2, "owner", "POST", "/api/v1/posts", { body: { lang: "en", slug: "busy", title: "Busy", markdown: `![b](/media/${busy.id}/busy.png)\n` } });
  assert.equal(post.status, 201, JSON.stringify(post.json));
  const { context, page, problems } = await open(w2, "owner");
  try {
    await page.locator(".tile").first().waitFor();
    const held = gate();
    await page.route("**/api/v1/media/*", async (r) => {
      if (r.request().method() === "DELETE") await held.open;
      await r.continue();
    });
    const del = async () => {
      await page.getByRole("button", { name: tx("admin.media.delete") }).click();
      await page.locator(".confirm .btn.danger").click();
    };
    // 1. Delete free.png, open busy.png's card while the request is out: free.png goes, busy.png's card stays.
    await tileOf(page, "free.png").click();
    await del();
    await tileOf(page, "busy.png").click();
    held.go();
    await page.locator(".toast", { hasText: tx("admin.media.deleted") }).waitFor();
    assert.equal(await tileOf(page, "free.png").count(), 0);
    assert.equal(await page.locator(".insp-title b").innerText(), "busy.png", "the other card is still open");
    assert.equal(w2.query(`SELECT COUNT(*) AS n FROM media WHERE id = ${free.id}`)[0].n, 0);
    // 2. Delete busy.png (in use), close its card before the answer: the refusal is a toast, not silence.
    await page.unroute("**/api/v1/media/*");
    const held2 = gate();
    await page.route("**/api/v1/media/*", async (r) => {
      if (r.request().method() === "DELETE") await held2.open;
      await r.continue();
    });
    await del();
    await page.keyboard.press("Escape");
    await page.locator(".insp-card").waitFor({ state: "detached" });
    held2.go();
    await page.locator(".toast", { hasText: tx("admin.media.err_used") }).waitFor();
    assert.match(await page.locator(".toast").innerText(), /Post #\d+/);
    assert.equal(await tileOf(page, "busy.png").count(), 1);
    // The deleted picture's own tile may refetch it (no-store) in the moment between the
    // server's delete and the tile's removal: a 404 for that one URL is expected.
    const gone = `/media/${free.id}/free.png`;
    assert.deepEqual(problems.filter((p) => !(p.includes("status of 404") && p.endsWith(gone))), []);
  } finally {
    await context.close();
  }
});

test("Save stays off while a save is out, and the latest text follows", async () => {
  const { context, page, problems } = await open(w2, "owner");
  try {
    await page.locator(".tile").first().waitFor();
    const bodies = [];
    const held = gate();
    await page.route("**/api/v1/media/*", async (r) => {
      if (r.request().method() === "PATCH") {
        bodies.push(r.request().postDataJSON().alt);
        if (bodies.length === 1) await held.open;
      }
      await r.continue();
    });
    await tileOf(page, "busy.png").click();
    const alt = page.getByLabel(tx("admin.media.alt"), { exact: true });
    const save = page.getByRole("button", { name: tx("admin.media.save") });
    await alt.fill("first");
    await save.click();
    await page.waitForFunction(() => document.querySelector(".insp-card .btn[aria-busy]"));
    await alt.fill("second");
    assert.equal(await save.isDisabled(), true, "no second save while one is out");
    held.go();
    await page.locator(".toast", { hasText: tx("admin.media.saved") }).waitFor();
    assert.deepEqual(bodies, ["first", "second"]);
    const id = w2.query("SELECT id FROM media WHERE r2_key LIKE '%/busy.png'")[0].id;
    assert.equal(w2.query(`SELECT alt FROM media WHERE id = ${id}`)[0].alt, "second");
    assert.equal(await save.isDisabled(), true);
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

test("the admin client's upload() sends the raw file, maps src to url, and stops on abort or an unreadable picture", async () => {
  const clientJs = (await build({ entryPoints: [new URL("../../web/admin/src/api/client.ts", import.meta.url).pathname], bundle: true, write: false, format: "esm", logLevel: "silent" })).outputFiles[0].text;
  const { context, page, problems, logged } = await open(w2, "owner", "/admin/media", { allow: /not a picture this browser can read/ });
  try {
    await page.route(at(w2, "/admin/test-client.js"), (r) => r.fulfill({ contentType: "text/javascript", body: clientJs }));
    const rows = () => w2.query("SELECT COUNT(*) AS n FROM media")[0].n;
    const bytes = png(10, 6, [200, 30, 30]);
    const run = (kind) =>
      page.evaluate(
        async ({ kind, data }) => {
          const { httpApi } = await import("/admin/test-client.js");
          const api = httpApi();
          const ok = new File([new Uint8Array(data)], "Client Up.PNG", { type: "image/png" });
          const progress = [];
          if (kind === "ok") return { r: await api.upload(ok, (s) => progress.push(s)), progress };
          const ctl = new AbortController();
          if (kind === "abort-before") {
            ctl.abort();
            return { r: await api.upload(ok, () => undefined, ctl.signal) };
          }
          if (kind === "abort-while-reading") {
            const p = api.upload(ok, () => undefined, ctl.signal);
            ctl.abort();
            return { r: await p };
          }
          const junk = new File([new TextEncoder().encode("not a picture")], "junk.png", { type: "image/png" });
          return { r: await api.upload(junk, () => undefined) };
        },
        { kind, data: [...bytes] },
      );
    const before = rows();
    const up = await run("ok");
    assert.equal(up.r.ok, true, JSON.stringify(up.r));
    assert.match(up.r.value.url, /^\/media\/\d+\/client-up\.png$/, "src comes back as url");
    assert.deepEqual([up.r.value.width, up.r.value.height], [10, 6]);
    assert.equal(rows(), before + 1);
    const row = w2.query(`SELECT r2_key, mime, width, height, bytes, uploaded_by FROM media WHERE id = ${up.r.value.id}`)[0];
    assert.match(row.r2_key, /^media\/[A-Za-z0-9_-]{22}\/client-up\.png$/);
    assert.deepEqual([row.mime, row.width, row.height, row.bytes, row.uploaded_by], ["image/png", 10, 6, bytes.length, 1]);
    // The body was the file itself: the stored original is byte for byte the same.
    const orig = await fetch(`${w2.url}/api/v1/media/${up.r.value.id}/original`, { headers: { cookie: `${COOKIE}=${S.owner}` } });
    assert.equal(orig.status, 200);
    assert.ok(Buffer.from(await orig.arrayBuffer()).equals(bytes), "stored bytes equal the file");
    for (const kind of ["abort-before", "abort-while-reading"]) {
      const r = (await run(kind)).r;
      assert.deepEqual([r.ok, r.error?.code], [false, "cancelled"], kind);
    }
    const junk = (await run("junk")).r;
    assert.deepEqual([junk.ok, junk.error.code], [false, "input.invalid"]);
    assert.equal(rows(), before + 1, "no row for an aborted or unreadable upload");
    assert.ok(logged.some((l) => /not a picture this browser can read/.test(l)), "the unreadable file is logged");
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

// ---- upload: a third, empty Worker, so the counts above stay as they are --------------------
// The tests build on each other in order: two pictures by the picker, the same one again, a
// drop, a paste; then the refusals, cancel, failures, several files and a user without rights.

const pngFile = (name, w, h, rgb) => ({ name, mimeType: "image/png", buffer: png(w, h, rgb) });
const hex = (buf) => createHash("sha256").update(buf).digest("hex");
const media3 = () => w3.query("SELECT id, mime, width, height, bytes, uploaded_by, sha256 FROM media ORDER BY id");
const posts3 = (page) => {
  const seen = [];
  page.on("request", (r) => r.method() === "POST" && /\/api\/v1\/media\?/.test(r.url()) && seen.push(new URL(r.url())));
  return seen;
};
const tilesAre = (page, n) => page.waitForFunction((n) => document.querySelectorAll(".media .tile").length === n, n);
const fill = (s, vars) => s.replace(/\{(\w+)\}/g, (_, k) => vars[k]);
const UP = { allow: /status of 400|status of 413|admin media: uploading|api: POST|net::ERR_FAILED/ };

test("the picker uploads a PNG with its sha256: a row, an R2 object, and the new tile at the top", async () => {
  w3 = await startWorker({ seedSql: seedFor });
  const { context, page, problems } = await open(w3, "will", "/admin/media", UP);
  try {
    await page.locator(".media .empty").waitFor();
    const sent = posts3(page);
    const input = page.locator(".up-add input[type=file]");
    assert.equal(await input.getAttribute("accept"), "image/avif,image/webp,image/jpeg,image/png,image/gif");
    assert.equal(await input.getAttribute("multiple"), "");
    const a = pngFile("pick-a.png", 12, 8, [200, 40, 40]);
    await input.setInputFiles(a);
    await tilesAre(page, 1);
    await page.locator(".toast").waitFor();
    const b = pngFile("pick-b.png", 10, 10, [40, 200, 40]);
    await input.setInputFiles(b);
    await tilesAre(page, 2);
    assert.deepEqual(await names(page), ["pick-b.png", "pick-a.png"], "the newest is first");
    const rows = media3();
    assert.equal(rows.length, 2);
    assert.deepEqual([rows[0].mime, rows[0].width, rows[0].height, rows[0].uploaded_by], ["image/png", 12, 8, 2]);
    assert.equal(sent[0].searchParams.get("sha256"), hex(a.buffer), "the browser sent the hash");
    assert.equal(rows[0].sha256, hex(a.buffer), "the stored hash is the file's");
    const orig = await fetch(`${w3.url}/api/v1/media/${rows[0].id}/original`, { headers: { cookie: `${COOKIE}=${S.will}` } });
    assert.ok(Buffer.from(await orig.arrayBuffer()).equals(a.buffer), "the R2 object is the file");
    assert.equal(await page.locator(".ups .up-row").count(), 0, "a finished file leaves no row");
    assert.equal(await page.locator(".toast").innerText(), fill(tx("admin.media.up_added"), { name: "pick-b.png" }));
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

test("the same file again is 200 with the first item: no second row, no second tile, and it says so", async () => {
  const { context, page, problems } = await open(w3, "will", "/admin/media", UP);
  try {
    await tilesAre(page, 2);
    const before = media3();
    const answer = page.waitForResponse((r) => r.request().method() === "POST" && r.url().includes("/api/v1/media?"));
    await page.locator(".up-add input[type=file]").setInputFiles(pngFile("pick-a.png", 12, 8, [200, 40, 40]));
    assert.equal((await answer).status(), 200);
    const note = page.locator(".up-row.is-dup");
    await note.waitFor();
    assert.equal(await note.locator(".up-msg").innerText(), fill(tx("admin.media.up_exists"), { name: "pick-a.png" }));
    assert.deepEqual(media3(), before, "nothing was written");
    assert.equal(await page.locator(".media .tile").count(), 2);
    // The existing picture is open in the details.
    await page.locator(".insp-card").waitFor();
    assert.equal(await page.locator(".insp-title b").innerText(), "pick-a.png");
    await note.getByRole("button", { name: tx("admin.media.up_dismiss") }).click();
    assert.equal(await page.locator(".ups .up-row").count(), 0);
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

test("dropping files on the library uploads them, and the drop target lights up while dragging", async () => {
  const { context, page, problems } = await open(w3, "will", "/admin/media", UP);
  try {
    await tilesAre(page, 2);
    const data = [...png(9, 9, [40, 40, 200])];
    const state = await page.evaluate((data) => {
      const dt = new DataTransfer();
      dt.items.add(new File([new Uint8Array(data)], "dropped.png", { type: "image/png" }));
      const main = document.querySelector(".media-main");
      const fire = (type) => main.dispatchEvent(new DragEvent(type, { dataTransfer: dt, bubbles: true, cancelable: true }));
      fire("dragenter");
      const over = main.classList.contains("over");
      fire("drop");
      return { over, after: main.classList.contains("over") };
    }, data);
    assert.deepEqual(state, { over: true, after: false });
    await tilesAre(page, 3);
    assert.equal((await names(page))[0], "dropped.png");
    assert.equal(media3().length, 3);
    // Dragging something that is not a file (text, a link) does not light it up.
    const plain = await page.evaluate(() => {
      const dt = new DataTransfer();
      dt.setData("text/plain", "words");
      const main = document.querySelector(".media-main");
      main.dispatchEvent(new DragEvent("dragenter", { dataTransfer: dt, bubbles: true, cancelable: true }));
      return main.classList.contains("over");
    });
    assert.equal(plain, false);
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

test("pasting a picture uploads it, but a paste into the description field stays the field's", async () => {
  const { context, page, problems } = await open(w3, "will", "/admin/media", UP);
  try {
    await tilesAre(page, 3);
    const paste = (selector, data) =>
      page.evaluate(
        ({ selector, data }) => {
          const dt = new DataTransfer();
          dt.items.add(new File([new Uint8Array(data)], "image.png", { type: "image/png" }));
          const e = new ClipboardEvent("paste", { clipboardData: dt, bubbles: true, cancelable: true });
          document.querySelector(selector).dispatchEvent(e);
          return e.defaultPrevented;
        },
        { selector, data },
      );
    await tileOf(page, "pick-a.png").click();
    const alt = page.locator(".insp-card textarea");
    await alt.waitFor();
    assert.equal(await paste(".insp-card textarea", [...png(7, 7, [90, 90, 90])]), false);
    await page.waitForTimeout(300);
    assert.equal(media3().length, 3);
    assert.equal(await paste("body", [...png(7, 7, [90, 90, 90])]), true);
    await tilesAre(page, 4);
    assert.equal((await names(page))[0], "image.png");
    assert.equal(media3().length, 4);
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

test("a type the library does not take and a file over 20 MiB are refused in the browser: no request", async () => {
  const { context, page, problems } = await open(w3, "will", "/admin/media", UP);
  try {
    await tilesAre(page, 4);
    const sent = posts3(page);
    await page.locator(".up-add input[type=file]").setInputFiles([
      { name: "notes.txt", mimeType: "text/plain", buffer: Buffer.from("hello") },
      { name: "icon.svg", mimeType: "image/svg+xml", buffer: Buffer.from("<svg xmlns='http://www.w3.org/2000/svg'/>") },
      { name: "huge.png", mimeType: "image/png", buffer: Buffer.alloc(20 * 1024 * 1024 + 1) },
    ]);
    await page.locator(".ups .up-row.is-err").nth(2).waitFor();
    assert.deepEqual(await page.locator(".up-row.is-err .up-msg").allInnerTexts(), [
      fill(tx("admin.media.up_type"), { name: "notes.txt" }),
      fill(tx("admin.media.up_type"), { name: "icon.svg" }),
      fill(tx("admin.media.up_size"), { name: "huge.png" }),
    ]);
    assert.equal(await page.locator(".up-row.is-err .up-msg[role=alert]").count(), 3, "each is announced");
    await page.waitForTimeout(300);
    assert.equal(sent.length, 0, "nothing was sent");
    assert.equal(media3().length, 4);
    await page.locator(".up-row.is-err").first().getByRole("button", { name: tx("admin.media.up_dismiss") }).click();
    assert.equal(await page.locator(".up-row.is-err").count(), 2);
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

test("a file the browser cannot read as a picture says so and sends nothing", async () => {
  const { context, page, problems, logged } = await open(w3, "will", "/admin/media", { allow: /not a picture this browser can read|admin media: uploading/ });
  try {
    await tilesAre(page, 4);
    const sent = posts3(page);
    await page.locator(".up-add input[type=file]").setInputFiles({ name: "broken.png", mimeType: "image/png", buffer: Buffer.from("not a picture") });
    const msg = page.locator(".up-row.is-err .up-msg");
    await msg.waitFor();
    assert.equal(await msg.innerText(), fill(tx("admin.media.up_unreadable"), { name: "broken.png" }));
    assert.equal(sent.length, 0);
    assert.ok(logged.some((l) => /not a picture this browser can read/.test(l)), "logged for the next agent");
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

test("Cancel while a file is going out stops it: no row, no tile, nothing stored", async () => {
  const { context, page, problems } = await open(w3, "will", "/admin/media", UP);
  try {
    await tilesAre(page, 4);
    const held = [];
    // The request is held on its way out, as a slow upload would be.
    await page.route("**/api/v1/media?**", (route) => (route.request().method() === "POST" ? held.push(route) : route.continue()));
    await page.locator(".up-add input[type=file]").setInputFiles(pngFile("cancel-me.png", 6, 6, [10, 120, 200]));
    const row = page.locator(".up-row");
    await row.getByText("cancel-me.png").waitFor();
    // Hashed and out: the row reads 0% and the request is held.
    await row.getByText(fill(tx("admin.media.up_sending"), { percent: "0" })).waitFor();
    for (let i = 0; i < 100 && !held.length; i++) await page.waitForTimeout(50);
    assert.equal(held.length, 1, "the request is out");
    await row.getByRole("button", { name: tx("admin.media.up_cancel") }).click();
    assert.equal(await page.locator(".up-row").count(), 0, "the row is gone");
    await held[0].abort();
    await page.waitForTimeout(300);
    assert.equal(await page.locator(".media .tile").count(), 4);
    assert.equal(await page.locator(".up-row.is-err").count(), 0, "a cancel is not an error");
    assert.equal(media3().length, 4, "nothing stored");
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

test("a server 400 names the field, a 413 and a lost connection each have their words, and Try again sends the file again", async () => {
  const { context, page, problems, logged } = await open(w3, "will", "/admin/media", UP);
  try {
    await tilesAre(page, 4);
    const send = (f) => page.locator(".up-add input[type=file]").setInputFiles(f);
    const msg = () => page.locator(".up-row.is-err .up-msg");
    const dismiss = () => page.locator(".up-row.is-err").getByRole("button", { name: tx("admin.media.up_dismiss") }).click();
    // A real 400: the request goes out with a width the file does not have.
    await page.route("**/api/v1/media?**", (route) => {
      if (route.request().method() !== "POST") return route.continue();
      const u = new URL(route.request().url());
      u.searchParams.set("width", "999");
      return route.continue({ url: u.toString() });
    });
    await send(pngFile("lies.png", 6, 6, [1, 2, 3]));
    await msg().waitFor();
    // Plain words, never the field's name; the log keeps the field.
    assert.equal(await msg().innerText(), fill(tx("admin.media.up_bad_size"), { name: "lies.png" }));
    assert.ok(!/width/.test(await msg().innerText()));
    assert.ok(logged.some((l) => /uploading lies\.png failed: input\.invalid \(400\) on width/.test(l)), logged.join("\n"));
    assert.equal(media3().length, 4);
    // 413.
    await page.unroute("**/api/v1/media?**");
    await page.route("**/api/v1/media?**", (route) => (route.request().method() === "POST" ? route.fulfill({ status: 413, contentType: "application/json", body: '{"error":"api.body_too_large"}' }) : route.continue()));
    await dismiss();
    await send(pngFile("big.png", 6, 6, [3, 2, 1]));
    await msg().waitFor();
    assert.equal(await msg().innerText(), fill(tx("admin.media.up_size"), { name: "big.png" }));
    // No network.
    await page.unroute("**/api/v1/media?**");
    await page.route("**/api/v1/media?**", (route) => (route.request().method() === "POST" ? route.abort() : route.continue()));
    await dismiss();
    await send(pngFile("offline.png", 6, 6, [5, 5, 5]));
    await msg().waitFor();
    assert.equal(await msg().innerText(), fill(tx("admin.media.up_offline"), { name: "offline.png" }));
    // Back online: Try again sends the same file and it lands.
    await page.unroute("**/api/v1/media?**");
    await page.locator(".up-row.is-err").getByRole("button", { name: tx("admin.state.retry") }).click();
    await tilesAre(page, 5);
    assert.equal((await names(page))[0], "offline.png");
    assert.equal(media3().length, 5);
    assert.equal(await page.locator(".up-row").count(), 0);
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

test("several files all upload, never more than two at once, and each gets its own tile", async () => {
  const { context, page, problems } = await open(w3, "will", "/admin/media", UP);
  try {
    await tilesAre(page, 5);
    let inflight = 0;
    let peak = 0;
    await page.route("**/api/v1/media?**", async (route) => {
      if (route.request().method() !== "POST") return route.continue();
      peak = Math.max(peak, ++inflight);
      await new Promise((r) => setTimeout(r, 400));
      await route.continue();
      inflight--;
    });
    const files = Array.from({ length: 6 }, (_, i) => pngFile(`many-${i}.png`, 8 + i, 8, [i * 40, 100, 50]));
    await page.locator(".up-add input[type=file]").setInputFiles(files);
    // Rows wait their turn: some say Waiting while two are out.
    await page.waitForFunction((w) => [...document.querySelectorAll(".up-state")].some((s) => s.textContent === w), tx("admin.media.up_waiting"));
    await tilesAre(page, 11);
    assert.equal(peak, 2, "two at a time");
    assert.deepEqual((await names(page)).slice(0, 6).sort(), files.map((f) => f.name).sort());
    assert.equal(media3().length, 11);
    assert.equal(await page.locator(".up-row").count(), 0);
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

test("a user without media.upload sees no upload controls, and a drop or paste does nothing", async () => {
  // The API has two roles, both allowed to upload, and a read-only token cannot sign in to the
  // admin; so the permission gate is exercised by answering /api/auth/me with a role the table lacks.
  const setup = (page) =>
    page.route("**/api/auth/me", async (route) => {
      const res = await route.fetch();
      const body = await res.json();
      body.user.role = "reader";
      await route.fulfill({ response: res, json: body });
    });
  const { context, page, problems } = await open(w3, "will", "/admin/media", { setup });
  try {
    await tilesAre(page, 11);
    assert.equal(await page.locator(".up-add").count(), 0);
    assert.equal(await page.locator("input[type=file]").count(), 0);
    assert.equal(await page.locator(".drop-hint").count(), 0);
    assert.equal(await page.locator(".media-main.drop").count(), 0);
    const sent = posts3(page);
    const data = [...png(5, 5, [1, 1, 1])];
    await page.evaluate((data) => {
      const dt = new DataTransfer();
      dt.items.add(new File([new Uint8Array(data)], "nope.png", { type: "image/png" }));
      document.querySelector(".media-main").dispatchEvent(new DragEvent("drop", { dataTransfer: dt, bubbles: true, cancelable: true }));
      document.body.dispatchEvent(new ClipboardEvent("paste", { clipboardData: dt, bubbles: true, cancelable: true }));
    }, data);
    await page.waitForTimeout(300);
    assert.equal(sent.length, 0);
    assert.equal(media3().length, 11);
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

test("on a phone the picture that was already there opens too (its own row does not count as other work)", async () => {
  const { context, page, problems } = await open(w3, "will", "/admin/media", { ...UP, width: 375, height: 812 });
  try {
    await tilesAre(page, 11);
    await page.locator(".up-add input[type=file]").setInputFiles(pngFile("many-0.png", 8, 8, [0, 100, 50]));
    // The row sits in the grid's column, which the details replace on a phone.
    await page.locator(".up-row.is-dup").waitFor({ state: "attached" });
    await page.locator(".insp-card").waitFor();
    assert.equal(await page.locator(".insp-title b").innerText(), "many-0.png");
    assert.equal(await page.locator(".toast").innerText(), fill(tx("admin.media.up_exists"), { name: "many-0.png" }), "said where the row is hidden");
    assert.equal(media3().length, 11);
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

const DUP_A = () => pngFile("pick-a.png", 12, 8, [200, 40, 40]);
const DUP_B = () => pngFile("pick-b.png", 10, 10, [40, 200, 40]);

test("a file dropped on the details panel is not opened by the browser and not uploaded", async () => {
  const { context, page, problems } = await open(w3, "will", "/admin/media", UP);
  try {
    await tilesAre(page, 11);
    await tileOf(page, "pick-a.png").click();
    await page.locator(".insp-card").waitFor();
    const sent = posts3(page);
    const url = page.url();
    const r = await page.evaluate(() => {
      const dt = new DataTransfer();
      dt.items.add(new File([new Uint8Array(4)], "x.png", { type: "image/png" }));
      const e = new DragEvent("drop", { dataTransfer: dt, bubbles: true, cancelable: true });
      document.querySelector(".inspector").dispatchEvent(e);
      const o = new DragEvent("dragover", { dataTransfer: dt, bubbles: true, cancelable: true });
      document.querySelector(".inspector").dispatchEvent(o);
      return { drop: e.defaultPrevented, over: o.defaultPrevented, effect: dt.dropEffect };
    });
    assert.deepEqual(r, { drop: true, over: true, effect: "none" }, "claimed, so no navigation, and shown as not accepted");
    await page.waitForTimeout(300);
    assert.equal(page.url(), url);
    assert.equal(sent.length, 0);
    assert.equal(media3().length, 11);
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

test("on a phone, cancelling the queued files lets the duplicate open", async () => {
  const { context, page, problems } = await open(w3, "will", "/admin/media", { ...UP, width: 375, height: 812 });
  try {
    await tilesAre(page, 11);
    const held = [];
    const sent = [];
    await page.route("**/api/v1/media?**", (route) => {
      if (route.request().method() !== "POST") return route.continue();
      sent.push(new URL(route.request().url()).searchParams.get("name"));
      held.push(route);
    });
    await page.locator(".up-add input[type=file]").setInputFiles([DUP_B(), DUP_A(), pngFile("q-1.png", 5, 5, [1, 9, 9]), pngFile("q-2.png", 5, 6, [2, 9, 9])]);
    for (let i = 0; i < 100 && held.length < 2; i++) await page.waitForTimeout(50);
    assert.equal(held.length, 2, "two are out, two wait");
    const waiting = page.locator(".up-row", { hasText: tx("admin.media.up_waiting") });
    assert.equal(await waiting.count(), 2);
    await waiting.first().getByRole("button", { name: tx("admin.media.up_cancel") }).click();
    await waiting.first().getByRole("button", { name: tx("admin.media.up_cancel") }).click();
    await page.waitForTimeout(200);
    assert.equal(held.length, 2, "a cancelled waiting file is never sent");
    // The first to answer sees the other still sending; the last one opens.
    const [first, second] = held;
    await first.continue();
    await page.locator(".toast").waitFor();
    assert.equal(await page.locator(".insp-card").count(), 0, "not while another is still out");
    await second.continue();
    await page.locator(".insp-card").waitFor();
    assert.equal(await page.locator(".insp-title b").innerText(), "pick-a.png");
    assert.deepEqual(sent.sort(), ["pick-a.png", "pick-b.png"], "q-1 and q-2 never went out");
    assert.equal(media3().length, 11);
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

test("a picture uploaded while the first page is loading is not lost", async () => {
  const gate = (() => {
    let go;
    const p = new Promise((r) => (go = r));
    return { p, go };
  })();
  const setup = (page) =>
    page.route("**/api/v1/media?limit*", async (route) => {
      // The page's answer is taken now and handed over only after the upload: it cannot know the picture.
      const res = await route.fetch();
      await gate.p;
      await route.fulfill({ response: res });
    });
  const { context, page, problems } = await open(w3, "will", "/admin/media", { ...UP, setup });
  try {
    await page.locator(".grid[aria-busy]").waitFor();
    await page.locator(".up-add input[type=file]").setInputFiles(pngFile("late.png", 7, 9, [77, 7, 7]));
    await page.locator(".toast").waitFor();
    assert.equal(media3().length, 12);
    gate.go();
    await tilesAre(page, 12);
    assert.equal((await names(page))[0], "late.png", "on top, once");
    assert.deepEqual(problems, []);
  } finally {
    gate.go();
    await context.close();
  }
});

test("an upload does not replace the first page's Try again; the picture shows once a page loads", async () => {
  let fail = true;
  const setup = (page) =>
    page.route("**/api/v1/media?limit*", (route) => (fail ? route.fulfill({ status: 500, contentType: "application/json", body: '{"error":"internal"}' }) : route.continue()));
  const { context, page, problems } = await open(w3, "will", "/admin/media", { allow: /loading the library failed|status of 500|admin media: uploading/, setup });
  try {
    const alert = page.locator(".media [role=alert]").first();
    await alert.getByText(tx("admin.state.error_title")).waitFor();
    await page.locator(".up-add input[type=file]").setInputFiles(pngFile("after-fail.png", 7, 8, [7, 77, 7]));
    await page.locator(".toast").waitFor();
    assert.equal(await page.locator(".media .tile").count(), 0);
    await alert.getByText(tx("admin.state.error_title")).waitFor();
    fail = false;
    await alert.getByRole("button", { name: tx("admin.state.retry") }).click();
    await tilesAre(page, 13);
    assert.equal((await names(page))[0], "after-fail.png");
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

test("a failure after the screen was left is a toast, not lost", async () => {
  const { context, page, problems } = await open(w3, "will", "/admin/media", UP);
  try {
    await tilesAre(page, 13);
    let hold;
    await page.route("**/api/v1/media?**", (route) => (route.request().method() === "POST" ? (hold = route) : route.continue()));
    await page.locator(".up-add input[type=file]").setInputFiles(pngFile("gone.png", 5, 7, [8, 8, 80]));
    for (let i = 0; i < 100 && !hold; i++) await page.waitForTimeout(50);
    await page.locator("nav.side a", { hasText: tx("admin.nav.posts") }).click();
    await page.locator(".media").waitFor({ state: "detached" });
    await hold.fulfill({ status: 500, contentType: "application/json", body: '{"error":"internal"}' });
    await page.locator(".toast").waitFor();
    assert.equal(await page.locator(".toast").innerText(), fill(tx("admin.media.up_failed"), { name: "gone.png" }));
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

test("a file waiting in the queue can be cancelled and is never sent", async () => {
  const { context, page, problems } = await open(w3, "will", "/admin/media", UP);
  try {
    await tilesAre(page, 13);
    const held = [];
    const names_ = [];
    await page.route("**/api/v1/media?**", (route) => {
      if (route.request().method() !== "POST") return route.continue();
      names_.push(new URL(route.request().url()).searchParams.get("name"));
      // The first two stay out until released; later ones (started when a slot frees) go through.
      if (held.length < 2) held.push(route);
      else route.continue();
    });
    await page.locator(".up-add input[type=file]").setInputFiles([1, 2, 3, 4].map((i) => pngFile(`cq-${i}.png`, 5 + i, 5, [i * 20, 3, 3])));
    for (let i = 0; i < 100 && held.length < 2; i++) await page.waitForTimeout(50);
    const waiting = page.locator(".up-row", { hasText: tx("admin.media.up_waiting") });
    assert.equal(await waiting.count(), 2);
    const cancelled = await waiting.first().locator(".up-name").innerText();
    await waiting.first().getByRole("button", { name: tx("admin.media.up_cancel") }).click();
    assert.equal(await page.locator(".up-row", { hasText: cancelled }).count(), 0);
    for (const r of held) await r.continue();
    await tilesAre(page, 16);
    assert.ok(!names_.includes(cancelled), `${cancelled} was never requested (${names_})`);
    assert.equal(names_.length, 3);
    assert.equal(media3().length, 16);
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});

test("when this browser gives no hash the file still goes, without sha256, and it is logged", async () => {
  const { context, page, problems, logged } = await open(w3, "will", "/admin/media", { allow: /sent without a sha256/, setup: (p) => p.addInitScript(() => Object.defineProperty(crypto, "subtle", { value: undefined })) });
  try {
    await tilesAre(page, 16);
    const sent = posts3(page);
    await page.locator(".up-add input[type=file]").setInputFiles(pngFile("nohash.png", 6, 9, [9, 99, 9]));
    await tilesAre(page, 17);
    assert.equal(sent.length, 1);
    assert.equal(sent[0].searchParams.has("sha256"), false);
    assert.equal(media3().at(-1).sha256, null);
    assert.ok(logged.some((l) => /sent without a sha256/.test(l)), logged.join("\n"));
    assert.deepEqual(problems, []);
  } finally {
    await context.close();
  }
});
