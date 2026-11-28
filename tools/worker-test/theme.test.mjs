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

function themeJson(name) {
  const files = pushedFiles(name);
  const hash = sha256hex(JSON.stringify(files));
  return JSON.stringify({ api: 1, name, version: "0.2.0", hash, files });
}

const seed = [
  `INSERT INTO users (id, login, slug, display_name, role, password_hash, disabled_at, created_at, updated_at) VALUES
    (1, 'olga', 'olga', 'Olga', 'owner', NULL, NULL, ${T}, ${T});`,
  `INSERT INTO api_tokens (id, user_id, name, token_hash, scope, created_at, expires_at, revoked_at) VALUES
    (13, 1, 'boss', ${sq(sha256hex(OWNER))}, 'owner', ${T}, NULL, NULL);`,
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
      broken: { vars: { PLUMBLOG_THEME: "paper" }, assets: assetsWith("paper", "{not json") },
      missing: { vars: { PLUMBLOG_THEME: "ghost" }, assets: assetsWith("other", null) },
      embedded: {},
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
