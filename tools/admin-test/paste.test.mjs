// Pasted HTML to Markdown (web/admin/src/editor/paste-md.ts) on the shapes Word, Google Docs
// and web pages put on the clipboard. Runs the real module (Node strips its types) in a
// stand-in DOM.
import assert from "node:assert/strict";
import { test } from "node:test";
import { Window } from "happy-dom";
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
