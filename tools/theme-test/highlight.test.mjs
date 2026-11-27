// The minimal theme's code highlighter on hostile input: every language, text never changed,
// work linear in the block size. Runs the real script against a tiny stand-in DOM.
//
// Wall time on a shared CI runner is a poor clock, so no test gates on a number of
// milliseconds. Linearity is shown two ways: the regex calls and characters matched (counted,
// so exact) grow in proportion to the input, and the time of an 8x larger block stays well
// under the 64x a quadratic scan would take. The absolute bound only catches blowups.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const SRC = readFileSync(new URL("../../themes/minimal/assets/highlight.js", import.meta.url), "utf8");
const LANGS = ["sh", "js", "ts", "python", "rust", "go", "c", "html", "css", "json", "yaml", "toml", "sql", "markdown", "diff"];
const SMALL = 6_000;
const LARGE = 48_000; // under the script's 50,000 limit; 8x: linear work grows 8x, quadratic 64x
const BACKSTOP_MS = 2_000; // normal is 5-25 ms; only a catastrophic regex gets here
const big = (s, n) => s.repeat(Math.ceil(n / s.length)).slice(0, n);

// Shapes that make backtracking regexes slow: unclosed openers, runs of brackets, long lines
// with no terminator, escapes, and the review's regex-literal case.
const CORPUS = (n) => ({
  "regex literal, [a] repeated": "x = /" + "[a]".repeat(Math.round(n / 1_600)) + "/ y",
  "[a] runs": big("[a]", n),
  "quotes": big("\"", n),
  "escaped quotes": big("\"\\\"", n),
  "single quotes": big("'", n),
  "backticks": big("`", n),
  "template opener": big("`${", n),
  "block comment openers": big("/* ", n),
  "html comment openers": big("<!-- ", n),
  "triple quotes": big("\"\"\" ", n),
  "raw string openers": big("r#\"r#\"", n),
  "brackets": big("[[[]", n),
  "slashes": big("/", n),
  "words without colon": big("a b ", n),
  "dotted run": big("a.", n),
  "dashed run": big("a-", n),
  "stars": big("*a ", n),
  "spaces then word": " ".repeat(n) + "a:",
  "long word": "a".repeat(n),
  "dollar braces": big("${", n),
  "lifetimes": big("'a ", n),
});

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

// Regex work the script does: every `exec` call and the characters those calls matched.
// Counting is exact, so it does not depend on how busy the machine is.
const realExec = RegExp.prototype.exec;
function run(blocks, count) {
  const work = { calls: 0, matched: 0 };
  // Counting slows every regex call, so timed runs go without it.
  if (count) {
    RegExp.prototype.exec = function (s) {
      const m = realExec.call(this, s);
      work.calls++;
      if (m) work.matched += m[0].length;
      return m;
    };
  }
  const t = performance.now();
  try {
    new Function("document", "console", SRC)(stubDom(blocks), { warn() {}, error() {} });
  } finally {
    RegExp.prototype.exec = realExec;
  }
  return { ms: performance.now() - t, ...work };
}

// Counts come from one run (they are the same every run); time is the best of seven, the
// minimum being the least noisy estimate.
function measure(lang, text) {
  const b = block(lang, text);
  const { calls, matched } = run([b], true);
  let ms = Infinity;
  for (let k = 0; k < 7; k++) ms = Math.min(ms, run([block(lang, text)]).ms);
  return { ms, calls, matched, b };
}

let worst = { ratio: 0 };
test.after?.(() => console.log(`worst 8x time ratio: ${worst.ratio.toFixed(1)} (${worst.at}); linear is about 8, quadratic 64`));

for (const lang of LANGS) {
  test(`${lang}: hostile input costs work linear in its size and keeps the text`, () => {
    const small = CORPUS(SMALL);
    const large = CORPUS(LARGE);
    for (const name of Object.keys(large)) {
      const at = `${lang} / ${name}`;
      const s = measure(lang, small[name]);
      const l = measure(lang, large[name]);
      // The script tries every rule at a position, so calls are a fixed multiple of the
      // text; a rescan from each position would add a factor of n. 8x input is 8x work;
      // the bound of 12 leaves room for the fixed cost of compiling the rules.
      assert.ok(l.calls < 12 * s.calls, `${at}: regex calls ${s.calls} -> ${l.calls}`);
      assert.ok(l.matched < 12 * Math.max(s.matched, SMALL), `${at}: matched chars ${s.matched} -> ${l.matched}`);
      // Time inside one regex cannot be counted: compare the sizes with a wide bound (linear
      // 8x, quadratic 64x; up to 2.8x over linear seen under load); the floor keeps timer
      // noise on tiny runs out.
      const ratio = l.ms / Math.max(s.ms, 1);
      if (ratio > worst.ratio) worst = { ratio, at };
      assert.ok(ratio < 28, `${at}: ${s.ms.toFixed(1)} ms -> ${l.ms.toFixed(1)} ms (x${ratio.toFixed(1)})`);
      assert.ok(l.ms < BACKSTOP_MS, `${at}: ${l.ms.toFixed(0)} ms`);
      if (l.b.code.out !== null) assert.equal(l.b.code.out, large[name], `${at}: text changed`);
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
