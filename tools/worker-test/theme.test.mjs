// A theme pushed as static assets (`PLUMBLOG_THEME` + `/_theme/<name>/theme.json`, the
// contract in crates/render pushed.rs): the Worker renders with it, falls back to the
// embedded default when theme.json is broken or missing (logging a stable code once), and
// checks option writes against the pushed theme. Also prints the cold first-render cost of
// a pushed theme next to the embedded one.
import assert from "node:assert/strict";
import { createHash, randomBytes } from "node:crypto";
import { cpSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, relative, resolve } from "node:path";
import { after, before, describe, test } from "node:test";
import { fileURLToPath } from "node:url";

import { ASSETS_DIR, startWorker } from "./harness.mjs";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const MINIMAL = join(ROOT, "themes/minimal");
const DEFAULT_ACCENT = "#8a2d6f";
const MARKER_ACCENT = "#0b6e4f";
const T = Date.now();
const sha256hex = (s) => createHash("sha256").update(s).digest("hex");
const sq = (s) => `'${String(s).replaceAll("'", "''")}'`;
const OWNER = `plb_${randomBytes(32).toString("base64url")}`;

function walk(dir) {
  return readdirSync(dir, { withFileTypes: true }).flatMap((e) =>
    e.isDirectory() ? walk(join(dir, e.name)) : [join(dir, e.name)],
  );
}

/** The minimal theme's text files (the set crates/worker build.rs embeds), renamed `name`, accent changed. */
function pushedFiles(name) {
  const files = {};
  for (const p of walk(MINIMAL).sort()) {
    const rel = relative(MINIMAL, p).replaceAll("\\", "/");
    const text = /\.(html|toml|css)$/.test(rel) && (!rel.startsWith("assets/") || rel === "assets/fonts/faces.css");
    if (text) files[rel] = readFileSync(p, "utf8");
  }
  files["theme.toml"] = files["theme.toml"]
    .replace('name = "minimal"', `name = "${name}"`)
    .replaceAll(DEFAULT_ACCENT, MARKER_ACCENT);
  return files;
}

/** The build's assets plus `/_theme/<name>/` holding `themeJson` (none when null) and the theme's assets. */
function assetsWith(name, themeJson) {
  const dir = mkdtempSync(join(tmpdir(), "plumblog-theme-assets-"));
  cpSync(ASSETS_DIR, dir, { recursive: true });
  const at = join(dir, "_theme", name);
  mkdirSync(at, { recursive: true });
  cpSync(join(MINIMAL, "assets"), join(at, "assets"), { recursive: true });
  if (themeJson !== null) writeFileSync(join(at, "theme.json"), themeJson);
  return dir;
}

function themeJson(name, edit = (f) => f) {
  const files = edit(pushedFiles(name));
  const hash = sha256hex(JSON.stringify(files));
  return JSON.stringify({ api: 1, name, version: "0.2.0", hash, files });
}

const tagOf = (json) => JSON.parse(json).hash.slice(0, 16);

// plumblog_render::pushed::MAX_DOCUMENT: the largest theme.json a Worker parses.
const MAX_DOCUMENT = 256 * 1024;

/** The pushed theme grown with CSS rules (minified at load, the costliest part) to `bytes` of theme.json. */
function grown(bytes) {
  let n = 0;
  let rules = "";
  const at = (r) => themeJson("paper", (f) => ({ ...f, "style.css": f["style.css"] + r })).length;
  const base = at("");
  // Each "\n" is two characters in the JSON.
  while (base + rules.length + n < bytes - 64) rules += `.pad-${n++} > a:hover { margin: 0 ${n}px; }\n`;
  return themeJson("paper", (f) => ({ ...f, "style.css": f["style.css"] + rules }));
}
const AT_CAP = grown(MAX_DOCUMENT - 1024);
const OVER_CAP = grown(MAX_DOCUMENT + 64 * 1024);

/** A post template that loads but fails on a real post: `nope` is not a post field. */
const FRAGILE = themeJson("paper", (f) => ({
  ...f,
  "templates/post.html": f["templates/post.html"].replace("<h1>{{ post.title }}</h1>", "<h1>{{ post.nope.deeper }}</h1>"),
}));

const seed = [
  `INSERT INTO users (id, login, slug, display_name, role, password_hash, disabled_at, created_at, updated_at) VALUES
    (1, 'olga', 'olga', 'Olga', 'owner', NULL, NULL, ${T}, ${T});`,
  `INSERT INTO api_tokens (id, user_id, name, token_hash, scope, created_at, expires_at, revoked_at) VALUES
    (13, 1, 'boss', ${sq(sha256hex(OWNER))}, 'owner', ${T}, NULL, NULL);`,
].join("\n");

const postSeed = [
  seed,
  `INSERT INTO posts (kind, translation_group, lang, slug, status, published_at, author_id, title, markdown, html, created_at, updated_at)
    VALUES ('post', 0, 'en', 'real-post', 'published', ${T - 1000}, 1, 'Real post', 'x', '<p>x</p>', ${T}, ${T});`,
  "UPDATE posts SET translation_group = id WHERE slug = 'real-post';",
].join("\n");

const put = (w, path, value) =>
  fetch(w.url + path, {
    method: "PUT",
    headers: { authorization: `Bearer ${OWNER}`, "content-type": "application/json", origin: w.url },
    body: JSON.stringify({ value }),
  });

const count = (text, needle) => text.split(needle).length - 1;

/** First request after start: the isolate loads its theme here. */
async function cold(w) {
  const t0 = performance.now();
  const res = await fetch(`${w.url}/`);
  const html = await res.text();
  return { res, html, wall: performance.now() - t0, render: res.headers.get("server-timing") };
}

describe("pushed theme", () => {
  const dirs = [];
  const workers = {};
  const first = {};
  before(async () => {
    const setups = {
      pushed: { vars: { PLUMBLOG_THEME: "paper" }, assets: assetsWith("paper", themeJson("paper")), seedSql: seed },
      broken: { vars: { PLUMBLOG_THEME: "paper" }, assets: assetsWith("paper", "{not json"), seedSql: seed },
      atcap: { vars: { PLUMBLOG_THEME: "paper" }, assets: assetsWith("paper", AT_CAP) },
      overcap: { vars: { PLUMBLOG_THEME: "paper" }, assets: assetsWith("paper", OVER_CAP) },
      missing: { vars: { PLUMBLOG_THEME: "ghost" }, assets: assetsWith("other", null) },
      embedded: {},
      fragile: { vars: { PLUMBLOG_THEME: "paper" }, assets: assetsWith("paper", FRAGILE), seedSql: postSeed },
    };
    for (const s of Object.values(setups)) if (s.assets) dirs.push(s.assets);
    await Promise.all(
      Object.entries(setups).map(async ([k, opts]) => {
        workers[k] = await startWorker(opts);
      }),
    );
    for (const k of Object.keys(setups)) first[k] = await cold(workers[k]);
  });
  after(async () => {
    await Promise.all(Object.values(workers).map((w) => w.stop()));
    for (const d of dirs) rmSync(d, { recursive: true, force: true });
  });

  test("the page renders with the pushed theme and its assets", async () => {
    const { res, html } = first.pushed;
    assert.equal(res.status, 200, html);
    assert.match(html, new RegExp(`--c-accent:light-dark\\(${MARKER_ACCENT},`));
    assert.ok(!html.includes(DEFAULT_ACCENT), "no default accent");
    const font = html.match(/\/_theme\/paper\/assets\/fonts\/[a-z-]+\.woff2/)?.[0];
    assert.ok(font, "fonts load from the pushed theme's assets");
    assert.equal((await fetch(workers.pushed.url + font)).status, 200, font);
    assert.match(res.headers.get("etag"), /-t[0-9a-f]{16}-/, "ETag names the theme");
    // blog deploy's smoke check reads this to confirm the pushed theme is the one serving.
    assert.equal(res.headers.get("x-plumblog-theme"), tagOf(themeJson("paper")));
    assert.equal((await fetch(`${workers.pushed.url}/`)).headers.get("x-plumblog-cache"), "hit");
    const log = workers.pushed.log();
    assert.equal(count(log, "theme.pushed_loaded name=paper"), 1, log);
    assert.ok(!log.includes("theme.pushed_invalid") && !log.includes("theme.pushed_missing"), log);
  });

  test("option writes are checked against the pushed theme", async () => {
    const w = workers.pushed;
    const r = await put(w, "/api/v1/themes/paper/options/excerpt_words", 41);
    assert.equal(r.status, 400);
    assert.deepEqual(await r.json(), { error: "input.invalid", field: "value" });
    assert.equal((await put(w, "/api/v1/themes/paper/options/excerpt_words", 40)).status, 204);
    // The embedded default is checked too, under its own name.
    assert.equal((await put(w, "/api/v1/themes/minimal/options/accent", "#336699")).status, 400);
    const html = await (await fetch(`${w.url}/`)).text();
    assert.match(html, new RegExp(`--c-accent:light-dark\\(${MARKER_ACCENT},`), "still the pushed theme");
  });

  test("a broken theme.json falls back to the default and is logged once", async () => {
    const w = workers.broken;
    assert.equal(first.broken.res.status, 200);
    assert.match(first.broken.html, new RegExp(`--c-accent:light-dark\\(${DEFAULT_ACCENT},`));
    for (const path of ["/", "/archive/", "/missing-page"]) await (await fetch(w.url + path)).text();
    const log = w.log();
    assert.equal(count(log, "theme.pushed_invalid name=paper"), 1, log);
    assert.doesNotMatch(first.broken.res.headers.get("etag"), /-t[0-9a-f]{16}-/);
    assert.equal(first.broken.res.headers.get("x-plumblog-theme"), null, "no theme tag on the default");
    assert.equal(first.embedded.res.headers.get("x-plumblog-theme"), null);
  });

  test("theme.json at the cap loads; over it the default renders and it is logged (cold wall time printed)", async () => {
    assert.ok(AT_CAP.length <= MAX_DOCUMENT && OVER_CAP.length > MAX_DOCUMENT);
    assert.equal(first.atcap.res.headers.get("x-plumblog-theme"), tagOf(AT_CAP));
    assert.equal(first.overcap.res.status, 200);
    assert.match(first.overcap.html, new RegExp(`--c-accent:light-dark\\(${DEFAULT_ACCENT},`));
    assert.equal(first.overcap.res.headers.get("x-plumblog-theme"), null);
    const log = workers.overcap.log();
    assert.match(log, /theme\.pushed_invalid name=paper detail=theme\.json is \d+ bytes, over 262144/, log);
    const loaded = workers.atcap.log().match(/theme\.pushed_loaded .*fetch_ms=(\d+) load_ms=(\d+)/);
    console.log(
      `cold first GET / with theme.json at the cap (${(AT_CAP.length / 1024).toFixed(0)} KB): wall ${first.atcap.wall.toFixed(1)} ms, ` +
        `${first.atcap.render}; in the Worker fetch ${loaded?.[1]} ms, parse+load ${loaded?.[2]} ms`,
    );
  });

  test("a store error with a pushed theme takes the plain error path, not the theme fallback", async () => {
    const w = workers.fragile;
    w.exec("ALTER TABLE tags RENAME TO tags_gone");
    try {
      const res = await fetch(`${w.url}/tag/anything`);
      await res.text();
      assert.equal(res.status, 500);
    } finally {
      w.exec("ALTER TABLE tags_gone RENAME TO tags");
    }
    assert.doesNotMatch(w.log(), /theme\.pushed_render_failed/, "a D1 error is not a theme failure");
  });

  test("option writes for a pushed theme that failed to load are refused", async () => {
    const w = workers.broken;
    const r = await put(w, "/api/v1/themes/paper/options/excerpt_words", 41);
    assert.equal(r.status, 409);
    assert.deepEqual(await r.json(), { error: "theme.unavailable" });
    // The embedded default is still checked under its own name.
    assert.equal((await put(w, "/api/v1/themes/minimal/options/accent", "#336699")).status, 400);
  });

  test("a page the pushed theme fails to render is served with the default, uncached, logged once", async () => {
    const w = workers.fragile;
    assert.equal(first.fragile.res.status, 200, first.fragile.html);
    for (let i = 0; i < 2; i++) {
      const res = await fetch(`${w.url}/real-post`);
      const html = await res.text();
      assert.equal(res.status, 200, html);
      assert.match(html, /Real post/);
      assert.match(html, new RegExp(`--c-accent:light-dark\\(${DEFAULT_ACCENT},`), "the default theme");
      assert.equal(res.headers.get("cache-control"), "no-store");
      assert.equal(res.headers.get("etag"), null);
      assert.equal(res.headers.get("x-plumblog-cache"), "bypass");
      assert.equal(res.headers.get("x-plumblog-theme"), null);
    }
    assert.equal(first.fragile.res.headers.get("x-plumblog-theme"), tagOf(FRAGILE), "home renders with the push");
    const log = w.log();
    assert.equal(count(log, "theme.pushed_render_failed"), 1, log);
    assert.match(log, /theme\.pushed_render_failed name=paper path=\/real-post/);
  });

  test("a theme.json that was never pushed falls back to the default", async () => {
    assert.equal(first.missing.res.status, 200);
    assert.match(first.missing.html, new RegExp(`--c-accent:light-dark\\(${DEFAULT_ACCENT},`));
    assert.equal(count(workers.missing.log(), "theme.pushed_missing name=ghost"), 1, workers.missing.log());
  });

  test("cold first render: pushed vs embedded (printed, not gated)", () => {
    const loaded = workers.pushed.log().match(/theme\.pushed_loaded .*fetch_ms=(\d+) load_ms=(\d+)/);
    for (const k of ["pushed", "embedded"]) {
      const f = first[k];
      console.log(`cold first GET / (${k}): wall ${f.wall.toFixed(1)} ms, ${f.render}`);
    }
    console.log(`pushed theme load in the Worker: fetch ${loaded?.[1]} ms, parse+load ${loaded?.[2]} ms`);
    assert.ok(loaded, "the load is logged with its timing");
  });
});
