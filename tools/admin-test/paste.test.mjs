// Pasted HTML to Markdown (web/admin/src/editor/paste-md.ts) on the shapes Word, Google Docs
// and web pages put on the clipboard. Runs the real module (Node strips its types) in a
// stand-in DOM.
import assert from "node:assert/strict";
import { test } from "node:test";
import { Window } from "happy-dom";
import { readFileSync } from "node:fs";
import { mdAlt, mdUrl } from "../../web/admin/src/editor/md-text.ts";
import { htmlToMarkdown, isRich } from "../../web/admin/src/editor/paste-md.ts";

const win = new Window();
const md = (html) => htmlToMarkdown(new win.DOMParser().parseFromString(html, "text/html").body);

// Google Docs: everything inside <b style="font-weight:normal" id="docs-internal-guid-...">,
// emphasis as span styles, headings as h1/h2, links through google.com/url redirects.
const DOCS = `<meta charset="utf-8"><b style="font-weight:normal;" id="docs-internal-guid-1a2b"><h1 dir="ltr"><span style="font-size:20pt;font-weight:400;">M\u1ed9t tu\u1ea7n \u1edf \u0110\u00e0 L\u1ea1t</span></h1><p dir="ltr"><span style="font-weight:400;">S\u00e1ng \u0111\u1ea7u ti\u00ean, </span><span style="font-weight:700;">s\u01b0\u01a1ng d\u00e0y</span><span style="font-weight:400;"> v\u00e0 </span><span style="font-style:italic;font-weight:400;">r\u1ea5t l\u1ea1nh</span><span style="font-weight:400;">. Xem </span><a href="https://www.google.com/url?q=https://example.com/da-lat&amp;sa=D&amp;source=editors" style="text-decoration:none;"><span style="color:#1155cc;text-decoration:underline;">b\u1ea3n \u0111\u1ed3</span></a><span>.</span></p><ul><li dir="ltr" style="list-style-type:disc;"><p dir="ltr"><span>Ch\u1ee3 s\u1edbm</span></p></li><li dir="ltr"><p dir="ltr"><span>\u0110\u1ed3i th\u00f4ng</span></p><ul><li><p><span>Sau m\u01b0a</span></p></li></ul></li></ul><ol><li><p><span>M\u1ed9t</span></p></li><li><p><span>Hai</span></p></li></ol></b>`;

// Word: MsoNormal paragraphs, lists as paragraphs with mso-list and a hidden marker span,
// <o:p> fillers, bold as <b>, italic as <i>.
const WORD = `<html xmlns:o="urn:schemas-microsoft-com:office:office"><head><style>p.MsoNormal{margin:0}</style></head><body lang=EN-US><!--StartFragment--><h2>Plan<o:p></o:p></h2><p class=MsoNormal>We leave at <b>six</b>, <i>before</i> the rain.<o:p></o:p></p><p class=MsoListParagraphCxSpFirst style='text-indent:-.25in;mso-list:l0 level1 lfo1'><![if !supportLists]><span style='font-family:Symbol;mso-list:Ignore'>\u00b7<span style='font:7.0pt "Times New Roman"'>&nbsp;&nbsp;</span></span><![endif]>Tea<o:p></o:p></p><p class=MsoListParagraphCxSpLast style='margin-left:1.0in;mso-list:l0 level2 lfo1'><span style='mso-list:Ignore'>o<span>&nbsp;</span></span>Green<o:p></o:p></p><p class=MsoListParagraph style='mso-list:l1 level1 lfo2'><span style='mso-list:Ignore'>1.<span>&nbsp;</span></span>Pack<o:p></o:p></p><p class=MsoListParagraph style='mso-list:l1 level1 lfo2'><span style='mso-list:Ignore'>2.<span>&nbsp;</span></span>Go<o:p></o:p></p><!--EndFragment--></body></html>`;

test("Google Docs: headings one level down, emphasis from styles, redirect links unwrapped, nested lists", () => {
  assert.equal(
    md(DOCS),
    [
      "## M\u1ed9t tu\u1ea7n \u1edf \u0110\u00e0 L\u1ea1t",
      "S\u00e1ng \u0111\u1ea7u ti\u00ean, **s\u01b0\u01a1ng d\u00e0y** v\u00e0 *r\u1ea5t l\u1ea1nh*. Xem [b\u1ea3n \u0111\u1ed3](https://example.com/da-lat).",
      "- Ch\u1ee3 s\u1edbm\n- \u0110\u1ed3i th\u00f4ng\n   - Sau m\u01b0a",
      "1. M\u1ed9t\n2. Hai",
    ].join("\n\n"),
  );
});

test("Word: mso lists become Markdown lists, one block per list, markers and <o:p> dropped", () => {
  assert.equal(md(WORD), ["### Plan", "We leave at **six**, *before* the rain.", "- Tea\n   - Green", "1. Pack\n2. Go"].join("\n\n"));
});

test("web pages: quotes, code, tables, dividers, pictures and breaks", () => {
  const html = `<blockquote><p>Quiet <em>mornings</em>.</p><p>Second</p></blockquote><pre><code>let x = 1;\nx *= 2;</code></pre><p>Use <code>npm ci</code> and a<br>break</p><table><tr><th>Day</th><th>Place</th></tr><tr><td>1</td><td>Ch\u1ee3 | s\u1edbm</td></tr></table><hr><p><img src="https://example.com/a b.jpg" alt="Hill"><img src="data:image/png;base64,AAA" alt="x"></p>`;
  assert.equal(
    md(html),
    [
      "> Quiet *mornings*.\n>\n> Second",
      "```\nlet x = 1;\nx *= 2;\n```",
      "Use `npm ci` and a\\\nbreak",
      "| Day | Place |\n| --- | --- |\n| 1 | Ch\u1ee3 \\| s\u1edbm |",
      "---",
      "![Hill](https://example.com/a%20b.jpg)",
    ].join("\n\n"),
  );
});

test("text that looks like Markdown stays text", () => {
  assert.equal(md("<p># not a heading</p><p>1. not a list</p><p>a *star* and [brackets] and snake_case _x_</p>"), "\\# not a heading\n\n1\\. not a list\n\na \\*star\\* and \\[brackets\\] and snake_case \\_x\\_");
});

test("unsafe or empty links keep only their text; spaces stay outside the marks", () => {
  assert.equal(md('<p><a href="javascript:alert(1)">click</a> <b> bold </b><i></i><a href="https://x.test/(a)">x</a></p>'), "click **bold** [x](https://x.test/%28a%29)");
});

test("only structured HTML is converted", () => {
  assert.equal(isRich("<span>plain</span>"), false);
  assert.equal(isRich("<meta charset='utf-8'><span style='font-weight:700'>x</span>"), true);
  assert.equal(isRich(WORD), true);
  assert.equal(isRich(DOCS), true);
});

test("a large paste converts fast", () => {
  const big = `<p>Line <b>bold</b> and <i>it</i> with a <a href="https://x.test">link</a>.</p>`.repeat(5000);
  const t = performance.now();
  const out = md(big);
  assert.ok(out.length > 100_000);
  assert.ok(performance.now() - t < 2000, "5,000 paragraphs under 2 s in happy-dom");
});

// ---- Hostile and awkward input ----

/** No link or picture target in `out` may use a scheme that runs or embeds code. */
const noBadTargets = (out) => {
  for (const m of out.matchAll(/\]\(([^)]*)\)/g)) assert.doesNotMatch(m[1], /^\s*(javascript|data|vbscript|file):/i, `target ${m[1]} in ${out}`);
};

test("data:, vbscript: and file: links keep only their text", () => {
  for (const href of ["data:text/html,<script>alert(1)</script>", "vbscript:msgbox(1)", "file:///etc/passwd"]) {
    const out = md(`<p><a href="${href.replace(/"/g, "&quot;")}">go</a></p>`);
    assert.equal(out, "go");
    noBadTargets(out);
  }
});

test("entity- and whitespace-obfuscated javascript: links keep only their text", () => {
  for (const href of ["jav&#x09;ascript:alert(1)", "&#106;avascript:alert(1)", "java&#10;script:alert(1)", "&#x01;javascript:alert(1)", "  JaVaScRiPt:alert(1)", "jav&Tab;ascript:alert(1)"]) {
    const out = md(`<p><a href="${href}">go</a></p>`);
    assert.equal(out, "go", href);
    noBadTargets(out);
  }
});

test("redirects to javascript: are dropped, redirects to pages are unwrapped", () => {
  assert.equal(md('<a href="https://www.google.com/url?q=javascript:alert(1)">g</a>'), "g");
  assert.equal(md('<a href="https://www.google.com/url?q=javascript%3Aalert(1)">g</a>'), "g");
  assert.equal(md('<a href="https://x.safelinks.protection.outlook.com/?url=%0Ajavascript%3Aalert(1)">o</a>'), "o");
  assert.equal(md('<a href="https://x.safelinks.protection.outlook.com/?url=https%3A%2F%2Fa.test%2Fp">o</a>'), "[o](https://a.test/p)");
  assert.equal(md('<a href="https://www.google.com/url?q=https%3A%2F%2Fa.test%2Fp&sa=D">o</a>'), "[o](https://a.test/p)");
});

test("malformed percent-encoding in a redirect does not lose the rest of the paste", () => {
  const out = md('<p><a href="https://www.google.com/url?q=%E0%A4%A">bad</a></p><p>rest of paste</p>');
  assert.equal(out, "[bad](https://www.google.com/url?q=%E0%A4%A)\n\nrest of paste");
  noBadTargets(out);
  assert.equal(md('<a href="https://x.test/?x=%E0%A4%A">ok</a>'), "[ok](https://x.test/?x=%E0%A4%A)");
});

test("javascript:, data: and file: pictures are dropped", () => {
  for (const src of ["javascript:alert(1)", " javascript:alert(1)", "data:image/svg+xml,<svg onload=alert(1)>", "file:///a.png", "vbscript:x"]) {
    assert.equal(md(`<p>a<img src="${src}" alt="x">b</p>`), "ab", src);
  }
});

test("pasted text that looks like Markdown or HTML stays inert text", () => {
  const out = md("<p>&lt;script&gt;alert(1)&lt;/script&gt; and ](javascript:alert(1)) and [x](javascript:alert(1))</p>");
  assert.equal(out, "<script>alert(1)</script> and \\](javascript:alert(1)) and \\[x\\](javascript:alert(1))");
  assert.doesNotMatch(out, /(^|[^\\])[[\]]/);
});

test("link targets cannot end early", () => {
  assert.equal(mdUrl("https://x.test/a)b"), "https://x.test/a%29b");
  assert.equal(mdUrl("https://x.test/a b\\c<d>e(f)\t\n\u00a0g"), "https://x.test/a%20b%5Cc%3Cd%3Ee%28f%29%09%0A%C2%A0g");
  assert.equal(md('<a href="https://x.test/a]b>c\\d e(f)">br</a>'), "[br](https://x.test/a]b%3Ec%5Cd%20e%28f%29)");
  assert.equal(md('<img src="https://x.test/a)b.png">'), "![](https://x.test/a%29b.png)");
});

test("inline code fences are longer than any backtick run inside", () => {
  assert.equal(md("<p><code>a`b</code></p>"), "``a`b``");
  assert.equal(md("<p><code>a``b`c</code></p>"), "```a``b`c```");
  assert.equal(md("<p><code>`a</code></p>"), "`` `a ``");
  assert.equal(md("<p><code>a`</code></p>"), "`` a` ``");
  assert.equal(md("<p><code>plain</code></p>"), "`plain`");
  assert.equal(md("<pre>a\n````\nb</pre>"), "`````\na\n````\nb\n`````");
});

test("picture descriptions are one line with brackets and backslashes escaped", () => {
  assert.equal(mdAlt("a\n\n b\t[c]\\"), "a b \\[c\\]\\\\");
  assert.equal(md('<img src="https://x.test/a.png" alt="a\nb\n\n  c [d] e\\">'), "![a b c \\[d\\] e\\\\](https://x.test/a.png)");
});

// Wall time on a shared CI runner is a poor clock (CI measured 4.5x for 2x the input), so
// linearity is checked on a count instead: every read the converter makes of the tree, with
// the reads that cost in proportion to what they return (`childNodes`, `children`,
// `textContent`) weighted by its length. Re-walking a subtree, the usual way such code turns
// quadratic, shows in the count exactly, whatever the machine. A wall-time bound far above
// normal (about 80 ms on CI) backstops work the count cannot see, such as string building.
const nest = (tag, n, inner = "x") => `<${tag}>`.repeat(n) + inner + `</${tag}>`.repeat(n);
const SHAPES = {
  spans: (n) => nest("span", n),
  "spans around a block": (n) => nest("span", n, "<p>x</p>"),
  divs: (n) => nest("div", n),
  bold: (n) => nest("b", n),
  lists: (n) => "<ul><li>".repeat(n) + "x" + "</li></ul>".repeat(n),
  quotes: (n) => nest("blockquote", n),
};
const parsed = (html) => new win.DOMParser().parseFromString(html, "text/html").body;

/** Runs `fn` with every tree read the converter uses counted; returns [result, reads]. */
function counting(fn) {
  const sample = parsed("<p>x</p>").firstChild;
  const weigh = (v) => (v && typeof v.length === "number" ? v.length : 1);
  const undo = [];
  let reads = 0;
  for (const name of ["tagName", "nodeType", "childNodes", "children", "parentElement", "textContent", "getAttribute"]) {
    let proto = sample;
    while (proto && !Object.prototype.hasOwnProperty.call(proto, name)) proto = Object.getPrototypeOf(proto);
    assert.ok(proto, `happy-dom has ${name}`);
    const d = Object.getOwnPropertyDescriptor(proto, name);
    const counted = { ...d };
    if (d.get) {
      counted.get = function () {
        const v = d.get.call(this);
        reads += name === "childNodes" || name === "children" || name === "textContent" ? weigh(v) : 1;
        return v;
      };
    } else {
      counted.value = function (...a) {
        reads += 1;
        return d.value.apply(this, a);
      };
    }
    Object.defineProperty(proto, name, counted);
    undo.push(() => Object.defineProperty(proto, name, d));
  }
  try {
    return [fn(), reads];
  } finally {
    for (const u of undo) u();
  }
}

test("deep and wide nesting converts with work linear in its size, and never throws", () => {
  for (const [name, make] of Object.entries(SHAPES)) {
    const small = parsed(make(5_000));
    const large = parsed(make(20_000));
    const [outSmall, readsSmall] = counting(() => htmlToMarkdown(small));
    const t = performance.now();
    const [outLarge, readsLarge] = counting(() => htmlToMarkdown(large));
    const ms = performance.now() - t;
    assert.match(outSmall, /x/, name);
    assert.match(outLarge, /x/, name);
    assert.ok(readsSmall > 5_000, `${name}: the count sees the walk (${readsSmall} reads)`);
    // 4x the input: linear work reads 4x (plus a constant), quadratic 16x.
    const ratio = readsLarge / readsSmall;
    assert.ok(ratio < 4.5, `${name}: ${readsSmall} -> ${readsLarge} reads (x${ratio.toFixed(2)})`);
    assert.ok(ms < 3_000, `${name}: ${ms.toFixed(0)} ms`);
  }
});

test("50,000-deep nesting converts without overflowing the stack", () => {
  for (const [name, make] of Object.entries(SHAPES)) {
    assert.match(htmlToMarkdown(parsed(make(50_000))), /x/, name);
  }
});

test("the converter reads a parsed copy and never writes HTML into the page", () => {
  const dir = new URL("../../web/admin/src/editor/", import.meta.url);
  const risky = /innerHTML|outerHTML|insertAdjacentHTML|document\.write|createContextualFragment|eval\(|new Function/;
  const conv = readFileSync(new URL("paste-md.ts", dir), "utf8");
  const editor = readFileSync(new URL("index.ts", dir), "utf8");
  assert.doesNotMatch(conv, risky);
  assert.doesNotMatch(editor, risky);
  assert.match(editor, /new DOMParser\(\)\.parseFromString\(html, "text\/html"\)/);
  assert.match(editor, /htmlToMarkdown\(doc\.body\)/);
});
