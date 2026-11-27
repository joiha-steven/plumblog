// The minimal theme's sidenotes in a DOM (happy-dom): a footnote cited 1, 2 or 3 times gets
// exactly one sidenote, ids stay unique and every aria reference resolves (issue #28).
// MINIMAL_JS=<path> runs another copy of the script (used to show the old bug).
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { Window } from "happy-dom";

const SRC = readFileSync(process.env.MINIMAL_JS || new URL("../../themes/minimal/assets/minimal.js", import.meta.url), "utf8");

function page(citesOfFirst) {
  const refs = Array.from({ length: citesOfFirst }, (_, k) =>
    `<sup class="fnref" id="fnref-1${k ? "-" + (k + 1) : ""}"><a href="#fn-1" role="doc-noteref">1</a></sup>`);
  const html = `<main><div class="prose" data-note-label="Show note {n}">
    <p>One${refs[0]} two.</p>${refs.slice(1).map((r) => `<p>Again${r}.</p>`).join("")}
    <p>Other<sup class="fnref" id="fnref-2"><a href="#fn-2" role="doc-noteref">2</a></sup>.</p>
    <section class="footnotes" role="doc-endnotes"><ol>
      <li id="fn-1"><p>First note. <a class="fnback" href="#fnref-1">back</a></p></li>
      <li id="fn-2"><p>Second note. <a class="fnback" href="#fnref-2">back</a></p></li>
    </ol></section></div></main>`;
  const window = new Window({ url: "https://blog.test/post" });
  window.document.body.innerHTML = html;
  // Run the theme script as the browser would, with the page's globals.
  new Function("document", "matchMedia", "localStorage", "sessionStorage", "navigator", "getSelection", "setTimeout", "location", "DOMParser", "URL", "console", SRC)(
    window.document, (q) => window.matchMedia(q), window.localStorage, window.sessionStorage, window.navigator,
    () => window.getSelection(), window.setTimeout.bind(window), window.location, window.DOMParser, window.URL, console,
  );
  return window;
}

for (const cites of [1, 2, 3]) {
  test(`footnote cited ${cites} time(s): one sidenote, unique ids, aria resolves`, () => {
    const window = page(cites);
    const d = window.document;
    const ids = [...d.querySelectorAll("[id]")].map((e) => e.id);
    assert.deepEqual(ids.filter((x, i) => ids.indexOf(x) !== i), [], "no duplicate ids");
    assert.equal(d.querySelectorAll(".sidenote").length, 2, "one sidenote per footnote");

    const buttons = [...d.querySelectorAll(".fnref button")].filter((b) => b.textContent === "1");
    assert.equal(buttons.length, cites);
    for (const b of buttons) assert.equal(b.getAttribute("aria-controls"), "sn-fn-1");
    assert.ok(d.getElementById("sn-fn-1"));
    // Only the first citation is a toggle; repeats reveal the note and carry no aria-expanded.
    assert.equal(buttons[0].getAttribute("aria-expanded"), "false");
    for (const b of buttons.slice(1)) assert.equal(b.getAttribute("aria-expanded"), null);

    const note = d.getElementById("sn-fn-1");
    buttons.at(-1).click();
    assert.ok(note.classList.contains("open"), "last citation opens the note");
    assert.equal(buttons[0].getAttribute("aria-expanded"), "true");
    window.happyDOM.close();
  });
}
