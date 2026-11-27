// minimal theme: code highlighting on the reader page. Loaded (deferred) only on posts with
// code. It wraps tokens in tok-* spans that only set color, so nothing moves; if it fails or
// does not run, the code stays as plain readable text.
//
// Speed rules (tools/theme-test/highlight.test.mjs holds them): every pattern is linear.
// Alternatives inside a repetition never start with the same character; anything with an
// opener (strings, comments) also accepts the end of the line or text as its end, so an
// unclosed opener matches once instead of being rescanned from every later position; and
// plain text is skipped a word or a space run at a time, never one character inside a word.
(() => {
  const kw = (words, flags = "") => new RegExp("\\b(?:" + words.replace(/ /g, "|") + ")\\b", flags);
  // Unclosed strings run to the end of the line.
  const q = (c) => c + "(?:\\\\[\\s\\S]|[^" + c + "\\\\\\n])*(?:" + c + "|(?=\\n)|$)";
  const str = new RegExp(q('"') + "|" + q("'"));
  const num = /\b(?:0x[\da-f_]+|0b[01_]+|0o[0-7_]+|\d[\d_]*(?:\.\d[\d_]*)?(?:e[+-]?\d+)?)[a-z\d]*\b/i;
  const block = /\/\*[\s\S]*?(?:\*\/|$)/;
  const slash = new RegExp("//.*|" + block.source);
  const hash = /#.*/;
  const fn = /\b[a-z_$][\w$]*(?=\s*\()/i;
  const caps = /\b[A-Z][\w$]*\b/;
  const op = /[-+*/%=<>!&|^~?]+/;
  const punct = /[{}()[\];,.:@]/;
  const consts = kw("true false null undefined nil None True False NaN Infinity");

  // Rust block comments nest; a regex cannot count, so this one is a small scanner.
  const nested = (text, i) => {
    if (!text.startsWith("/*", i)) return 0;
    let depth = 0, j = i;
    while (j < text.length) {
      if (text.startsWith("/*", j)) { depth++; j += 2; }
      else if (text.startsWith("*/", j)) { j += 2; if (!--depth) break; }
      else j++;
    }
    return j - i;
  };

  // Each language: [token class, pattern or scanner] tried in order at every position.
  const C = (keywords, extra = [], comment = slash) => [
    ["comment", comment], ...extra, ["string", str], ["constant", consts], ["keyword", kw(keywords)],
    ["number", num], ["function", fn], ["type", caps], ["operator", op], ["punct", punct],
  ];
  const js = C("async await break case catch class const continue debugger default delete do else export extends finally for from function get if import in instanceof let new of return set static super switch this throw try typeof var void while with yield as type interface enum implements namespace declare readonly private protected public keyof satisfies", [
    // `${...}` may hold backticks; it is consumed whole (no nested braces).
    ["string", /`(?:\\[\s\S]|\$\{[^}]*(?:\}|$)|\$(?!\{)|[^`\\$])*(?:`|$)/],
    // Regex literal: a class `[...]` is consumed whole and the other branch never takes `[`.
    ["string", /\/(?![*/])(?:\\.|\[(?:\\.|[^\]\\\n])*\]|[^/\\\n[])+\/[dgimsuyv]*(?=\s*[.,;)\]}]|\s*$)/m],
  ]);
  const rust = C("as async await break const continue crate dyn else enum extern fn for if impl in let loop match mod move mut pub ref return self Self static struct super trait type unsafe use where while", [
    ["comment", nested], ["attr", /#!?\[[^\]\n]*\]?/], ["string", /b?r(#*)"[\s\S]*?(?:"\1|$)/], ["string", /b?'(?:\\.|[^'\\\n])'/],
    ["type", /'[a-z_]\w*\b/], ["builtin", /\b[a-z_]\w*!/], ["type", kw("u8 u16 u32 u64 u128 usize i8 i16 i32 i64 i128 isize f32 f64 bool char str")],
  ], /\/\/.*/);
  const go = C("break case chan const continue default defer else fallthrough for func go goto if import interface map package range return select struct switch type var", [
    ["string", /`[^`]*(?:`|$)/], ["type", kw("bool byte complex64 complex128 error float32 float64 int int8 int16 int32 int64 rune string uint uint8 uint16 uint32 uint64 uintptr any")],
    ["builtin", kw("append cap close copy delete len make new panic print println recover")],
  ]);
  const clike = C("auto break case catch char class const continue default delete do double else enum extern final finally float for goto if implements import inline int long namespace new package private protected public return short signed sizeof static struct switch template this throw try typedef union unsigned using virtual void volatile while", [
    ["attr", /^[ \t]*#[ \t]*\w+/m],
  ]);
  const python = [
    ["comment", hash], ["string", /[rbfu]{0,2}("""|''')[\s\S]*?(?:\1|$)/i], ["string", new RegExp("[rbfu]{0,2}(?:" + str.source + ")", "i")],
    ["attr", /@[\w.]+/], ["constant", consts],
    ["keyword", kw("and as assert async await break class continue def del elif else except finally for from global if import in is lambda match case nonlocal not or pass raise return try while with yield")],
    ["builtin", kw("print len range enumerate zip open dict list set tuple str int float bool type isinstance super self cls map filter sorted min max sum any all repr")],
    ["number", num], ["function", fn], ["type", caps], ["operator", op], ["punct", punct],
  ];
  const shell = [
    ["comment", /(?<!\S)#.*/], ["string", /'[^']*(?:'|$)/], ["string", /"(?:\\[\s\S]|[^"\\])*(?:"|$)/], ["property", /\$(?:\{[^}\n]*\}?|\w+|[@#?$!*-])/],
    ["keyword", kw("if then else elif fi for while until do done case esac in function return local export select")],
    ["builtin", kw("echo printf cd pwd ls cat grep sed awk cp mv rm mkdir chmod chown sudo source exit set unset read test eval exec curl git npm npx cargo docker make")],
    ["attr", /(?<!\S)--?[\w-]+/], ["number", /\b\d+\b/], ["operator", /[|&;<>]+|=/], ["punct", /[{}()[\]]/],
  ];
  const html = [
    ["comment", /<!--[\s\S]*?(?:-->|$)/], ["keyword", /<![a-z][^>\n]*>?/i], ["tag", /<\/?[\w:-]+|\/?>/], ["attr", /(?<![\w:@.-])[\w:@.-]+(?=\s*=)/],
    ["string", str], ["constant", /&#?\w+;/], ["operator", /=/],
  ];
  const css = [
    ["comment", block], ["string", str], ["keyword", /@[\w-]+|!important/],
    // The cheap lookahead first, so the lookbehind only runs at the start of a name.
    ["property", /--[\w-]+|(?=[\w-])(?<![\w-])(?<=[{;]\s*)[\w-]+(?=\s*:)/],
    ["number", /#[\da-f]{3,8}\b|-?\b\d*\.?\d+(?:%|[a-z]+)?/i], ["function", /(?<![\w-])[\w-]+(?=\()/], ["type", /[.#][\w-]+|::?[\w-]+/],
    ["tag", /\b[a-z][\w-]*\b/], ["operator", /[>+~*=]/], ["punct", /[{}()[\];,:]/],
  ];
  const json = [
    ["property", /"(?:\\.|[^"\\\n])*"(?=\s*:)/], ["string", new RegExp(q('"'))], ["constant", kw("true false null")],
    ["number", /-?\b\d+(?:\.\d+)?(?:e[+-]?\d+)?\b/i], ["punct", /[{}[\],:]/],
  ];
  const yaml = [
    // Keys only at the start of a line (after indent or a list dash): one scan per line.
    ["comment", /(?<!\S)#.*/], ["keyword", /^(?:---|\.\.\.)[ \t]*$/m], ["property", /(?<=^[ \t]*(?:-[ \t]+)?)[\w.\-/][^:\n]*?(?=:(?:\s|$))/m],
    ["string", str], ["attr", /[&*][\w-]+|![\w!-]*/], ["constant", kw("true false null yes no on off")], ["constant", /~/],
    ["number", /-?\b\d+(?:\.\d+)?\b/], ["punct", /[-:|>[\]{},?]/],
  ];
  const toml = [
    ["comment", hash], ["type", /^[ \t]*\[\[?[^\]\n]*\]?\]?/m], ["string", /("""|''')[\s\S]*?(?:\1|$)/], ["property", /(?<![\w.-])[\w.-]+(?=\s*=)/], ["string", str],
    ["constant", kw("true false inf nan")], ["number", /\b\d{4}-\d\d-\d\d(?:[T ][\d:.]+(?:Z|[+-]\d\d:\d\d)?)?|[+-]?\b\d[\d_]*(?:\.\d+)?(?:e[+-]?\d+)?\b/i],
    ["operator", /=/], ["punct", /[[\]{},.]/],
  ];
  const sql = [
    ["comment", new RegExp("--.*|" + block.source)], ["string", /'(?:''|[^'])*(?:'|$)/], ["property", /"[^"\n]*(?:"|$)|`[^`\n]*(?:`|$)/m],
    ["keyword", kw("select from where and or not insert into values update set delete create table index view drop alter add column primary key foreign references join left right inner outer full on as group by order having limit offset union all distinct case when then else end is null in exists like between returning with default unique check constraint if begin commit rollback transaction asc desc", "i")],
    ["type", kw("integer int text real blob varchar char boolean date timestamp numeric serial bigint", "i")],
    ["number", num], ["function", fn], ["operator", /[-+*/%=<>!|]+/], ["punct", /[(),;.]/],
  ];
  const markdown = [
    ["keyword", /^#{1,6}\s.*/m], ["comment", /^[ \t]*>.*/m], ["string", /`[^`\n]+`|^(?:```|~~~).*/m], ["function", /!?\[[^\]\n]*\]\([^)\n]*\)/],
    ["type", /\*\*[^*\n]+\*\*|__[^_\n]+__/], ["attr", /\*[^*\s][^*\n]*\*|_[^_\s][^_\n]*_/], ["punct", /^[ \t]*(?:[-*+]|\d+\.)[ \t]/m],
  ];
  const diff = [
    ["comment", /^(?:diff|index|---|\+\+\+) .*/m], ["function", /^@@.*/m], ["string", /^\+.*/m], ["number", /^-.*/m],
  ];
  const langs = new Map(Object.entries({ js, javascript: js, jsx: js, ts: js, typescript: js, tsx: js, mjs: js, rust, rs: rust, go, golang: go,
    c: clike, h: clike, cpp: clike, "c++": clike, java: clike, cs: clike, csharp: clike, kotlin: clike, swift: clike, php: clike,
    python, py: python, sh: shell, bash: shell, shell, zsh: shell, console: shell, html, xml: html, svg: html, vue: html,
    css, scss: css, json, jsonc: json, yaml, yml: yaml, toml, sql, sqlite: sql, markdown, md: markdown, diff, patch: diff }));

  // Sticky regexes become matchers with the scanner's shape: (text, i) -> length matched.
  const matcher = (p) => {
    if (typeof p === "function") return p;
    const re = new RegExp(p.source, p.flags.replace(/[gy]/g, "") + "y");
    return (text, i) => { re.lastIndex = i; const m = re.exec(text); return m ? m[0].length : 0; };
  };
  const compiled = new Map();
  // Plain text: a word, a run of spaces, or one other character. Newlines stand alone so
  // line-start rules (`^` with the m flag) are tried right after them.
  const skip = /[\p{L}\p{N}_$]+|[^\S\n]+|[\s\S]/uy;
  const MAX = 50000; // longer blocks stay plain: highlighting must never make reading slow

  const paint = (box) => {
    const code = box.querySelector("pre > code");
    const name = (box.dataset.lang || "").toLowerCase();
    if (!code || !langs.has(name) || code.querySelector("span")) return;
    const text = code.textContent;
    if (text.length > MAX) return;
    if (!compiled.has(name)) compiled.set(name, langs.get(name).map(([cls, p]) => [cls, matcher(p)]));
    const rules = compiled.get(name);
    const out = document.createDocumentFragment();
    let plain = "", i = 0;
    while (i < text.length) {
      let cls = "", len = 0;
      for (const [c, match] of rules) {
        len = match(text, i);
        if (len) { cls = c; break; }
      }
      if (!len) {
        skip.lastIndex = i;
        const n = skip.exec(text)[0].length;
        plain += text.slice(i, i + n);
        i += n;
        continue;
      }
      if (plain) { out.append(plain); plain = ""; }
      const span = document.createElement("span");
      span.className = "tok-" + cls;
      span.textContent = text.slice(i, i + len);
      out.append(span);
      i += len;
    }
    if (plain) out.append(plain);
    code.replaceChildren(out);
  };

  for (const box of document.querySelectorAll(".prose .code[data-lang]")) {
    // One bad block (or a browser without some regex feature) must not stop the others.
    try { paint(box); } catch (e) { console.warn("highlight: block left plain", e); }
  }
})();
