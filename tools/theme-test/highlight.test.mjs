// The minimal theme's code highlighter on hostile input: every language, every block under
// 50 ms, text never changed. Runs the real script against a tiny stand-in DOM.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const SRC = readFileSync(new URL("../../themes/minimal/assets/highlight.js", import.meta.url), "utf8");
const LANGS = ["sh", "js", "ts", "python", "rust", "go", "c", "html", "css", "json", "yaml", "toml", "sql", "markdown", "diff"];
const BUDGET_MS = 50;
const big = (s) => s.repeat(Math.ceil(50_000 / s.length)).slice(0, 49_000);

// Shapes that make backtracking regexes slow: unclosed openers, runs of brackets, long lines
// with no terminator, escapes, and the review's regex-literal case.
const CORPUS = {
  "regex literal, [a] x30": "x = /" + "[a]".repeat(30) + "/ y",
  "[a] runs": big("[a]"),
  "quotes": big("\""),
  "escaped quotes": big("\"\\\""),
  "single quotes": big("'"),
  "backticks": big("`"),
  "template opener": big("`${"),
  "block comment openers": big("/* "),
  "html comment openers": big("<!-- "),
  "triple quotes": big("\"\"\" "),
  "raw string openers": big("r#\"r#\""),
  "brackets": big("[[[]"),
  "slashes": big("/"),
  "words without colon": big("a b "),
  "dotted run": big("a."),
  "dashed run": big("a-"),
  "stars": big("*a "),
  "spaces then word": " ".repeat(49_000) + "a:",
  "long word": "a".repeat(49_000),
  "dollar braces": big("${"),
  "lifetimes": big("'a "),
};

function stubDom(blocks) {
  const frag = () => {
    const parts = [];
    return { parts, append: (...n) => parts.push(...n) };
  };
  return {
    createDocumentFragment: frag,
    createElement: () => ({ className: "", textContent: "" }),
    querySelectorAll: () => blocks,
  };
}

function block(lang, text) {
  const code = {
    textContent: text,
    out: null,
    querySelector: () => null,
    replaceChildren(f) {
      this.out = f.parts.map((p) => (typeof p === "string" ? p : p.textContent)).join("");
      this.tokens = f.parts.filter((p) => typeof p !== "string").map((p) => [p.className.slice(4), p.textContent]);
    },
  };
  return { dataset: { lang }, querySelector: () => code, code };
}

function run(blocks) {
  const t = performance.now();
  new Function("document", "console", SRC)(stubDom(blocks), { warn() {}, error() {} });
  return performance.now() - t;
}

let slowest = { ms: 0 };
test.after?.(() => console.log(`slowest block: ${slowest.ms.toFixed(1)} ms (${slowest.at})`));

for (const lang of LANGS) {
  test(`${lang}: hostile input stays under ${BUDGET_MS} ms and keeps the text`, () => {
    for (const [name, text] of Object.entries(CORPUS)) {
      // Best of three: the budget is about growth with input size, not machine noise.
      let b, ms = Infinity;
      for (let k = 0; k < 3; k++) {
        b = block(lang, text);
        ms = Math.min(ms, run([b]));
      }
      if (ms > slowest.ms) slowest = { ms, at: `${lang} / ${name}` };
      assert.ok(ms < BUDGET_MS, `${lang} / ${name}: ${ms.toFixed(0)} ms`);
      if (b.code.out !== null) assert.equal(b.code.out, text, `${lang} / ${name}: text changed`);
    }
  });
}

test("a block named like an Object property does not stop later blocks", () => {
  const later = block("js", "const x = 1;");
  run([block("constructor", "x"), block("__proto__", "x"), block("toString", "x"), later]);
  assert.ok(later.code.out !== null, "the js block after them was highlighted");
});

test("a JS template literal holding a backtick in ${} is one string", () => {
  const b = block("js", "const s = `a ${f(`b`)} c`; x");
  run([b]);
  assert.deepEqual(b.code.tokens.find(([c]) => c === "string"), ["string", "`a ${f(`b`)} c`"]);
});

test("Rust block comments nest", () => {
  const b = block("rust", "/* a /* b */ still comment */ let x = 1;");
  run([b]);
  assert.deepEqual(b.code.tokens[0], ["comment", "/* a /* b */ still comment */"]);
  assert.deepEqual(b.code.tokens[1], ["keyword", "let"]);
});
