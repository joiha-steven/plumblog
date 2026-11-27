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
// Each preview refresh loads into a new frame (pb-preview-N) that replaces the shown one.
const previewIn = async (page, click) => {
  const nav = page.waitForEvent("framenavigated", (f) => f.name().startsWith("pb-preview") && f.url() !== "about:blank");
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
// Signed-out pages: type into the form, press the key, wait for the answer in the alert region.
const typed = async (page, values) => {
  for (const [id, v] of Object.entries(values)) await page.fill(`#${id}`, v);
};
const answered = (page) => page.waitForFunction(() => document.querySelector(".auth-alert")?.textContent);
const SIGN = { login: "lan", password: "correct horse battery" };
const ACCOUNT = { login: "lan", slug: "lan", "display-name": "Lan Nguyen", password: "correct horse battery", "password-repeat": "correct horse battery" };
const submitWith = (values, click = "button[type=submit]") => async (page) => {
  await typed(page, values);
  await page.click(click);
  await answered(page);
};
// Account screen: open the add form, or the remove confirm of the first passkey; `send` types the
// password and waits for the answer in the form's alert region.
const PASSKEYS = ".pk-list .pk";
const addForm = async (page) => {
  await page.click(".acct-add button");
  await page.waitForSelector(".acct-panel");
};
const removeConfirm = async (page) => {
  await page.click(".pk .btn");
  await page.waitForSelector(".confirm");
  await settle(page, ".pop");
};
const send = (form) => async (page) => {
  await page.fill(`${form} input[type=password]`, "correct horse battery");
  await page.click(`${form} button[type=submit]`);
  await page.waitForFunction((f) => document.querySelector(`${f} .auth-alert`)?.textContent, form);
};
// Two drafts for user 1 (the mock's user), under keys no editor state opens (editor/drafts.ts).
const seedDrafts = (page) =>
  page.evaluate(
    () =>
      new Promise((resolve, reject) => {
        const req = indexedDB.open("plumblog-admin", 2);
        req.onupgradeneeded = () => {
          if (req.result.objectStoreNames.contains("drafts")) req.result.deleteObjectStore("drafts");
          req.result.createObjectStore("drafts");
        };
        req.onerror = () => reject(req.error);
        req.onsuccess = () => {
          const db = req.result;
          const tx = db.transaction("drafts", "readwrite");
          const draft = { title: "A draft", body: "Words kept on this device.", base: null, rev: 1, savedAt: Date.now() };
          tx.objectStore("drafts").put(draft, "1:new-shot-a");
          tx.objectStore("drafts").put(draft, "1:new-shot-b");
          tx.oncomplete = () => (db.close(), resolve());
          tx.onerror = () => reject(tx.error);
        };
      }),
  );

// [name, path, element to wait for, action before the shot, widths it applies to]
// Pastes into the writing column what a clipboard would hold: a picture made on a canvas, or
// HTML as Google Docs puts it there.
const paste = (page, what) =>
  page.evaluate(async (w) => {
    const cm = document.querySelector(".cm-content");
    cm.focus();
    const dt = new DataTransfer();
    if (w === "picture") {
      const c = document.createElement("canvas");
      c.width = 1200;
      c.height = 800;
      const g = c.getContext("2d");
      const grad = g.createLinearGradient(0, 0, 1200, 800);
      grad.addColorStop(0, "#c9b79c");
      grad.addColorStop(1, "#4f6b5a");
      g.fillStyle = grad;
      g.fillRect(0, 0, 1200, 800);
      const blob = await new Promise((r) => c.toBlob(r, "image/png"));
      dt.items.add(new File([blob], "doi-thong-buoi-sang.png", { type: "image/png" }));
    } else {
      dt.setData("text/plain", "x");
      dt.setData("text/html", w);
    }
    cm.dispatchEvent(new ClipboardEvent("paste", { clipboardData: dt, bubbles: true, cancelable: true }));
  }, what);
const DOCS_HTML =
  '<b style="font-weight:normal" id="docs-internal-guid-1"><h1><span style="font-weight:400">K\u1ebf ho\u1ea1ch chuy\u1ebfn \u0111i</span></h1>' +
  '<p><span>Ch\u00fang t\u00f4i \u0111i l\u00fac </span><span style="font-weight:700">s\u00e1u gi\u1edd</span><span>, </span><span style="font-style:italic">tr\u01b0\u1edbc c\u01a1n m\u01b0a</span><span>. Xem </span>' +
  '<a href="https://www.google.com/url?q=https://example.com/ban-do&amp;sa=D"><span>b\u1ea3n \u0111\u1ed3</span></a><span>.</span></p>' +
  "<ul><li><p><span>Tr\u00e0 n\u00f3ng</span></p></li><li><p><span>\u00c1o m\u01b0a</span></p></li></ul>" +
  "<table><tr><td><p>Ng\u00e0y</p></td><td><p>N\u01a1i</p></td></tr><tr><td><p>1</p></td><td><p>Ch\u1ee3 s\u1edbm</p></td></tr></table></b>";

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
    // Touch screens have no bubble: the bottom bar acts on the selection.
    (w) => w >= 768,
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
  ["signin", "/admin/signin"],
  ["signin-wrong", "/admin/signin?auth=wrong", null, submitWith(SIGN)],
  ["signin-locked", "/admin/signin?auth=locked", null, submitWith(SIGN)],
  [
    "signin-turnstile",
    "/admin/signin?auth=turnstile_hold",
    null,
    async (page) => {
      await typed(page, SIGN);
      await page.click("button[type=submit]");
      // The always-pass widget solves itself, then the page sends the token by itself: busy again.
      await page.waitForSelector("button[type=submit]:disabled", { timeout: 15000 });
      await page.waitForTimeout(600);
    },
  ],
  ["signin-passkey-origin", "/admin/signin?auth=origin", null, submitWith({}, "button[type=button]")],
  ["setup", "/admin/setup?code=7f3a-91c2"],
  ["setup-short", "/admin/setup?code=7f3a-91c2", null, submitWith({ ...ACCOUNT, password: "short", "password-repeat": "short" })],
  ["setup-invalid", "/admin/setup?auth=setup_invalid&code=0000-0000", null, submitWith(ACCOUNT)],
  ["invite", "/admin/invite?code=b2d8-44e0"],
  ["invite-invalid", "/admin/invite?auth=invite_invalid&code=0000-0000", null, submitWith(ACCOUNT)],
  ["account", "/admin/account", PASSKEYS],
  ["account-empty", "/admin/account?passkeys=0", ".acct .empty"],
  ["account-add", "/admin/account", PASSKEYS, addForm],
  ["account-remove-confirm", "/admin/account", PASSKEYS, removeConfirm],
  [
    "account-remove-conflict",
    "/admin/account?auth=conflict",
    PASSKEYS,
    async (page) => {
      await removeConfirm(page);
      await send(".confirm")(page);
    },
  ],
  [
    "account-wrong-password",
    "/admin/account?auth=reauth_wrong",
    PASSKEYS,
    async (page) => {
      await addForm(page);
      await send(".acct-panel")(page);
    },
  ],
  [
    "signout-confirm",
    "/admin/account",
    PASSKEYS,
    async (page) => {
      await seedDrafts(page);
      await page.click("button.me");
      await page.waitForSelector(".pop");
      await page.click(".pop [data-act=signout]");
      await page.waitForSelector(".confirm");
      await settle(page, ".pop");
    },
  ],
  [
    "paste-docs",
    "/admin/posts/new",
    EDITOR,
    async (page) => {
      await paste(page, DOCS_HTML);
      await page.waitForFunction(() => document.querySelector(".cm-content")?.textContent?.includes("Ch\u1ee3 s\u1edbm"));
    },
  ],
  [
    "upload",
    "/admin/posts/3?upload=slow",
    EDITOR,
    async (page) => {
      await paste(page, "picture");
      await page.waitForFunction(() => document.querySelector(".up .bar i")?.style.transform.includes("0.1"));
    },
  ],
  [
    "upload-failed",
    "/admin/posts/3?upload=fail",
    EDITOR,
    async (page) => {
      await paste(page, "picture");
      await page.waitForSelector(".up.fail");
    },
  ],
  [
    "upload-offline",
    "/admin/posts/3",
    EDITOR,
    async (page) => {
      await page.evaluate(() => Object.defineProperty(navigator, "onLine", { get: () => false }));
      await paste(page, "picture");
      await page.waitForSelector(".toast");
      await settle(page, ".toast");
    },
  ],
  [
    "picture-alt",
    "/admin/posts/3",
    EDITOR,
    async (page) => {
      await paste(page, "picture");
      await page.waitForSelector(".altpop");
      await pictures(page);
      await settle(page, ".pop");
    },
  ],
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
        const english = ["posts", "no-permission", "editor", "slash", "signin-wrong", "setup-short", "account", "account-wrong-password", "signout-confirm"];
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
