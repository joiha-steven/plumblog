// Screenshots of every admin state at every checked width (docs/ARCHITECTURE.md, Design
// language), in light and dark, English and Vietnamese, plus a contact sheet (index.html).
// Runs the system Chrome through playwright-core: no browser download.
//
//   node tools/admin-shots.mjs http://localhost:8790 OUT_DIR [LAYOUT_AUDIT_JS]
//
// ONLY=state,state and WIDTHS=375,1440 narrow a run while working on one screen.
// LAYOUT_AUDIT_JS, when given, is evaluated on each page and must define
// globalThis.doGiaoDien() returning { tong: {kind: count}, ... }; the counts go into
// OUT_DIR/audit.json and the sheet. Serve the admin first: scripts/admin-dev.
import { chromium } from "playwright-core";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const [base, out, auditFile] = process.argv.slice(2);
if (!base || !out) {
  console.error("usage: node tools/admin-shots.mjs BASE_URL OUT_DIR [LAYOUT_AUDIT_JS]");
  process.exit(2);
}
mkdirSync(out, { recursive: true });
const audit = auditFile ? readFileSync(auditFile, "utf8") : null;

// Folded phone cover, phone, open foldable, tablet, open foldable landscape, desktop.
const SIZES = [
  [344, 882],
  [375, 812],
  [673, 841],
  [768, 1024],
  [884, 1104],
  [1440, 900],
];
const EDITOR = ".cm-content";
const settle = (page, sel) =>
  page.waitForFunction((q) => [...document.querySelectorAll(q)].every((el) => el.getAnimations().every((a) => a.playState === "finished")), sel);
// Pictures in the post must have loaded before the shot.
const pictures = (page) => page.waitForFunction(() => [...document.querySelectorAll(".cm-img img")].every((i) => i.complete));
// The preview frame is sandboxed (opaque origin): wait for its navigation, not its DOM.
const previewIn = async (page, click) => {
  const nav = page.waitForEvent("framenavigated", (f) => f.name() === "pb-preview" && f.url() !== "about:blank");
  await click();
  const frame = await nav;
  await frame.waitForSelector("h1");
};
// CodeMirror draws only the lines near the viewport: scroll the writing pane down until the
// line is drawn, then centre it.
const line = async (page, text) => {
  await page.evaluate(async (t) => {
    const pane = document.querySelector(".ed-write");
    const find = () => [...document.querySelectorAll(".cm-line")].find((l) => l.textContent.includes(t));
    for (let i = 0; i < 60 && !find(); i++) {
      pane.scrollTop += pane.clientHeight / 2;
      await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
    }
    find()?.scrollIntoView({ block: "center" });
    await new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
  }, text);
  return page.locator(".cm-line", { hasText: text }).first();
};
// [name, path, element to wait for, action before the shot, widths it applies to]
const STATES = [
  ["posts", "/admin/"],
  ["empty", "/admin/?mock=empty"],
  ["loading", "/admin/?mock=loading"],
  ["error", "/admin/?mock=error"],
  ["offline", "/admin/?mock=offline"],
  ["no-permission", "/admin/settings?role=writer"],
  ["not-found", "/admin/nope"],
  [
    "menu",
    "/admin/pages",
    null,
    async (page) => {
      await page.click("button.me");
      await page.waitForSelector(".pop");
      await settle(page, ".pop");
    },
  ],
  ["editor", "/admin/posts/1", EDITOR, pictures],
  [
    "editor-blocks",
    "/admin/posts/1",
    EDITOR,
    async (page) => {
      await line(page, "[!NOTE]");
    },
  ],
  [
    "bubble",
    "/admin/posts/1",
    EDITOR,
    async (page) => {
      await pictures(page);
      await (await line(page, "Ch\u1ee3 \u0110\u00e0 L\u1ea1t m\u1edf")).dblclick({ position: { x: 30, y: 10 } });
      await page.waitForSelector(".bubble");
      await settle(page, ".bubble");
    },
  ],
  [
    "slash",
    "/admin/posts/1",
    EDITOR,
    async (page) => {
      const l = await line(page, "T\u00f4i ghi l\u1ea1i s\u1ed1 ch\u1eef");
      await l.click({ position: { x: 5, y: 5 } });
      await page.keyboard.press("Home");
      await page.keyboard.press("Enter");
      await page.keyboard.press("ArrowUp");
      await page.keyboard.type("/");
      await page.waitForSelector(".slash");
      await settle(page, ".pop");
    },
  ],
  [
    "focus",
    "/admin/posts/1",
    EDITOR,
    async (page) => {
      const l = await line(page, "T\u00f4i ghi l\u1ea1i s\u1ed1 ch\u1eef");
      await l.click({ position: { x: 5, y: 5 } });
      await page.click("[data-tool=more]");
      await page.waitForSelector(".pop [data-tool=focus]");
      await page.click(".pop [data-tool=focus]");
      await page.evaluate(() => document.querySelector(".cm-line.cm-cur")?.scrollIntoView({ block: "center" }));
      // The toast says focus mode is on, then leaves; the shot is of the writing.
      await page.waitForFunction(() => !document.querySelector(".toast"), null, { timeout: 6000 });
      await page.waitForTimeout(400);
    },
  ],
  [
    "split",
    "/admin/posts/1",
    EDITOR,
    (page) => previewIn(page, () => page.click(".seg button:nth-child(2)")),
    (w) => w >= 900,
  ],
  ["preview", "/admin/posts/1", EDITOR, (page) => previewIn(page, () => page.click(".seg button:last-child"))],
  ["new", "/admin/posts/new", EDITOR],
];

const only = process.env.ONLY?.split(",");
const widths = process.env.WIDTHS?.split(",").map(Number);

const browser = await chromium.launch({ channel: "chrome", headless: true });
const shots = [];
try {
  for (const [w, hgt] of SIZES.filter(([w]) => !widths || widths.includes(w))) {
    for (const theme of ["light", "dark"]) {
      for (const lang of ["vi", "en"]) {
        // English once per width and theme, on the busiest screens, to catch longer text.
        const english = ["posts", "no-permission", "editor", "slash"];
        const states = STATES.filter(([s, , , , fits]) => (lang === "vi" || english.includes(s)) && (!fits || fits(w)) && (!only || only.includes(s)));
        const ctx = await browser.newContext({ viewport: { width: w, height: hgt }, deviceScaleFactor: 1, hasTouch: w < 768, isMobile: w < 768, colorScheme: theme });
        await ctx.addCookies([{ name: "pb_lang", value: lang, url: base }]);
        await ctx.addInitScript((t) => localStorage.setItem("pb.theme", t), theme);
        const page = await ctx.newPage();
        const errors = [];
        page.on("console", (m) => m.type() === "error" && errors.push(m.text()));
        page.on("pageerror", (e) => errors.push(String(e)));
        for (const [state, path, ready, act] of states) {
          await page.goto(base + path, { waitUntil: "networkidle" });
          if (state !== "loading") await page.waitForSelector(ready ?? "main h1, main [role=alert]", { timeout: 5000 });
          // Popovers and bubbles fade in: actions wait until their animations are over.
          if (act) await act(page);
          const name = `${state}-${w}-${theme}-${lang}.png`;
          await page.screenshot({ path: join(out, name) });
          let counts = null;
          if (audit) {
            await page.evaluate(audit);
            counts = await page.evaluate(() => globalThis.doGiaoDien().tong);
          }
          shots.push({ name, state, w, theme, lang, counts, errors: errors.splice(0) });
        }
        await ctx.close();
      }
    }
  }
} finally {
  await browser.close();
}

writeFileSync(join(out, "audit.json"), JSON.stringify(shots, null, 2));
const esc = (s) => String(s).replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
const groups = [...new Set(shots.map((s) => s.state))];
const sheet = `<!doctype html><meta charset="utf-8"><title>Admin screenshots</title>
<style>body{font:14px system-ui;margin:24px;background:#f3f2ef}h2{margin:32px 0 8px}
.r{display:flex;gap:16px;flex-wrap:wrap;align-items:flex-start}figure{margin:0}
img{display:block;max-height:420px;border:1px solid #ccc;background:#fff}figcaption{font-size:12px;margin-top:4px}
.bad{color:#a33}</style>
<h1>Admin screenshots</h1><p>${shots.length} shots. Layout audit counts and console errors under each.</p>
${groups
  .map(
    (g) => `<h2>${esc(g)}</h2><div class="r">${shots
      .filter((s) => s.state === g)
      .map((s) => {
        const issues = s.counts ? Object.entries(s.counts).filter(([, n]) => n > 0) : [];
        const note = [...issues.map(([k, n]) => `${k} ${n}`), ...s.errors.map((e) => `console: ${e}`)];
        return `<figure><a href="${esc(s.name)}"><img src="${esc(s.name)}" loading="lazy" alt=""></a><figcaption>${s.w} ${s.theme} ${s.lang}${
          note.length ? ` <span class="bad">${esc(note.join("; "))}</span>` : ""
        }</figcaption></figure>`;
      })
      .join("")}</div>`,
  )
  .join("")}`;
writeFileSync(join(out, "index.html"), sheet);
const flagged = shots.filter((s) => s.errors.length || (s.counts && Object.values(s.counts).some((n) => n > 0)));
console.log(`admin-shots: ${shots.length} shots in ${out}; ${flagged.length} with audit findings or console errors`);
for (const s of flagged) console.log(`  ${s.name}: ${JSON.stringify(s.counts)} ${s.errors.join(" | ")}`);
