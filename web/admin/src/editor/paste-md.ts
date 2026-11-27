/**
 * Pasted HTML (Word, Google Docs, web pages) to the Markdown posts are written in: headings,
 * emphasis, links, lists, quotes, code, tables and dividers survive; colors, fonts, classes
 * and layout do not. No imports, so it loads as its own small chunk on the first rich paste
 * and runs in tests without a bundler (tools/admin-test).
 */
import { mdAlt, mdUrl } from "./md-text.ts";

/** Whether pasted HTML carries structure worth converting; plain text otherwise pastes as is. */
export const isRich = (html: string): boolean =>
  /<(b|strong|i|em|h[1-6]|ul|ol|table|a|blockquote|pre|code|del|s|hr)[\s>]|font-weight|font-style:\s*italic|mso-list/i.test(html);

const SKIP = new Set(["SCRIPT", "STYLE", "META", "TITLE", "HEAD", "LINK", "TEMPLATE", "NOSCRIPT", "SVG", "O:P"]);
const BLOCK = new Set(["P", "DIV", "H1", "H2", "H3", "H4", "H5", "H6", "UL", "OL", "LI", "BLOCKQUOTE", "PRE", "TABLE", "HR", "SECTION", "ARTICLE", "HEADER", "FOOTER", "MAIN", "ASIDE", "FIGURE", "FIGCAPTION", "DL", "DT", "DD", "ADDRESS"]);

/** Deeper nesting than any real document: the rest of that subtree is taken as plain text. */
const MAX_DEPTH = 200;
let level = 0;
/** Elements with a block somewhere below them, filled by one pass per paste. */
let withBlock = new WeakSet<Element>();
/** Text of a subtree without recursion (a hostile paste can nest deeper than the stack). */
function text(root: Node): string {
  let out = "";
  for (const todo = [root]; todo.length; ) {
    const n = todo.pop() as Node;
    if (n.nodeType === 3) out += n.nodeValue ?? "";
    else if (n.nodeType === 1 && !SKIP.has((n as Element).tagName)) todo.push(...Array.from(n.childNodes).reverse());
  }
  return out;
}
const flat = (n: Node): string => escapeText(text(n).replace(/[\s\u00a0]+/g, " "));
function guard<T>(over: () => T, run: () => T): T {
  if (level >= MAX_DEPTH) return over();
  level++;
  try {
    return run();
  } finally {
    level--;
  }
}
const style = (el: Element): string => (el.getAttribute("style") ?? "").toLowerCase().replace(/\s+/g, "");

/** Bold, italic, struck or code by tag or by inline style (Word and Docs use spans). */
function marks(el: Element): string[] {
  const tag = el.tagName;
  const s = style(el);
  const out: string[] = [];
  // Docs wraps the whole paste in <b style="font-weight:normal">: the style wins over the tag.
  const weight = /font-weight:(\d+|bold|bolder|normal)/.exec(s)?.[1];
  const bold = weight ? weight === "bold" || weight === "bolder" || Number(weight) >= 600 : tag === "B" || tag === "STRONG";
  if (bold) out.push("**");
  if (tag === "I" || tag === "EM" || /font-style:italic/.test(s)) out.push("*");
  if (tag === "S" || tag === "DEL" || tag === "STRIKE" || /text-decoration[^;]*line-through/.test(s)) out.push("~~");
  return out;
}

const isCode = (el: Element): boolean =>
  el.tagName === "CODE" || el.tagName === "KBD" || el.tagName === "SAMP" || /font-family:[^;]*(mono|courier|consolas)/.test(style(el));

/** Characters that would turn plain text into Markdown. */
const escapeText = (s: string): string =>
  s.replace(/([\\`*[\]])/g, "\\$1").replace(/(^|[^\p{L}\p{N}])_|_(?=[^\p{L}\p{N}]|$)/gu, (m) => m.replace("_", "\\_"));

/** Google and Outlook wrap outgoing links in redirects; keep where they lead. */
function href(raw: string): string | undefined {
  let u = raw.trim();
  const m = /^https?:\/\/(?:www\.)?google\.[a-z.]+\/url\?(?:.*&)?q=([^&]+)/.exec(u) ?? /safelinks\.protection\.outlook\.com\/\?url=([^&]+)/.exec(u);
  if (m?.[1]) {
    try {
      u = decodeURIComponent(m[1]);
    } catch {
      // Malformed %: keep the redirect address itself, which the check below still vets.
    }
  }
  return /^(https?:|mailto:|tel:|\/(?!\/)|#)/i.test(u) ? mdUrl(u) : undefined;
}

/** Puts markers around text, keeping its outer spaces outside them (`** x**` is not bold). */
function around(text: string, open: string, close = open): string {
  const m = /^(\s*)([\s\S]*?)(\s*)$/.exec(text);
  if (!m || !m[2]) return text;
  return `${m[1]}${open}${m[2]}${close}${m[3]}`;
}

/** One more than the longest run of `ch` in `s`, at least `min`. */
function fenceLen(s: string, ch: string, min: number): number {
  let top = 0;
  for (const r of s.match(new RegExp(`${ch}+`, "g")) ?? []) top = Math.max(top, r.length);
  return Math.max(min, top + 1);
}

function inline(node: Node, pre = false): string {
  if (node.nodeType === 3) {
    const txt = node.textContent ?? "";
    return pre ? txt : escapeText(txt.replace(/[\s\u00a0]+/g, " "));
  }
  if (node.nodeType !== 1) return "";
  const el = node as Element;
  if (SKIP.has(el.tagName) || /mso-list:ignore/.test(style(el))) return "";
  if (el.tagName === "BR") return "\\\n";
  if (el.tagName === "IMG") {
    const src = el.getAttribute("src") ?? "";
    // Only pictures on the web travel; data:, blob: and file: ones are the clipboard's own.
    return /^https?:\/\//i.test(src) ? `![${mdAlt(el.getAttribute("alt") ?? "")}](${mdUrl(src)})` : "";
  }
  if (isCode(el)) {
    const txt = text(el).replace(/\s+/g, " ");
    if (!txt.trim()) return txt;
    // CommonMark: a fence longer than any backtick run inside, padded when the code touches one.
    const body = txt.trim();
    const fence = "`".repeat(fenceLen(body, "`", 1));
    const pad = body.startsWith("`") || body.endsWith("`") ? " " : "";
    return around(txt, fence + pad, pad + fence);
  }
  return guard(
    () => flat(el),
    () => inlineChildren(el, pre),
  );
}

function inlineChildren(el: Element, pre: boolean): string {
  let text = "";
  for (const c of Array.from(el.childNodes)) text += BLOCK.has((c as Element).tagName) ? ` ${inline(c, pre)} ` : inline(c, pre);
  if (el.tagName === "A") {
    const to = href(el.getAttribute("href") ?? "");
    if (to && text.trim()) text = around(text, "[", `](${to})`);
  }
  for (const m of marks(el)) text = around(text, m);
  return text;
}

/** Inline content of a block, one line (tables) or with its own line breaks. */
const line = (el: Node): string => inline(el).replace(/ *\\\n */g, "\\\n").replace(/[^\S\n]+/g, " ").trim();

/** A Word list paragraph: `mso-list:l0 level2 lfo1`, its marker in a hidden span. */
function wordItem(el: Element): { list: string; level: number; ordered: boolean } | undefined {
  const m = /mso-list:(\w+)[^;]*level(\d+)/.exec(style(el));
  if (!m) return undefined;
  const marker = Array.from(el.querySelectorAll("span")).find((s) => /mso-list:ignore/.test(style(s)))?.textContent?.trim() ?? "";
  return { list: m[1] ?? "", level: Number(m[2]) - 1, ordered: /^(\d+|[a-z]{1,3})[.)]$/i.test(marker) };
}

function table(el: Element): string {
  const rows = Array.from(el.querySelectorAll("tr")).map((tr) =>
    Array.from(tr.children)
      .filter((c) => c.tagName === "TD" || c.tagName === "TH")
      .map((c) => line(c).replace(/\\\n/g, " ").replace(/\|/g, "\\|")),
  );
  const width = Math.max(0, ...rows.map((r) => r.length));
  // A one-cell table is a box around text (Docs code blocks, callouts): keep the text.
  if (width <= 1) return rows.map((r) => r[0] ?? "").filter(Boolean).join("\n\n");
  const row = (r: string[]) => `| ${Array.from({ length: width }, (_, i) => r[i] ?? "").join(" | ")} |`;
  const [head = [], ...body] = rows;
  return [row(head), row(Array(width).fill("---")), ...body.map(row)].join("\n");
}

const list = (el: Element, depth: number): string =>
  guard(
    () => flat(el),
    () => listItems(el, depth),
  );

function listItems(el: Element, depth: number): string {
  const ordered = el.tagName === "OL";
  let n = Number(el.getAttribute("start") ?? 1) || 1;
  const items: string[] = [];
  for (const li of Array.from(el.children)) {
    if (li.tagName !== "LI") {
      if (li.tagName === "UL" || li.tagName === "OL") items.push(list(li, depth + 1));
      continue;
    }
    const marker = ordered ? `${n++}. ` : "- ";
    const pad = "   ".repeat(depth);
    const parts = blocks(li, depth + 1);
    const [first = "", ...rest] = parts;
    items.push(`${pad}${marker}${first.trimStart()}`, ...rest);
  }
  return items.join("\n");
}

const blocks = (root: Element, depth = 0): string[] =>
  guard(
    () => [flat(root).trim()].filter(Boolean),
    () => blockList(root, depth),
  );

function blockList(root: Element, depth: number): string[] {
  const out: string[] = [];
  let run = "";
  // The Word list being continued (`l0`, `l1`...) and its counters per level.
  let word: { list: string; n: number[] } | undefined;
  const flush = () => {
    // Paragraph text that happens to begin like a block ("# ", "> ", "1. ", "- ") stays text.
    const txt = run.replace(/[^\S\n]+/g, " ").trim().replace(/^(#{1,6} |>|[-+] )/, "\\$1").replace(/^(\d+)([.)] )/, "$1\\$2");
    if (txt) out.push(depth ? `${"   ".repeat(depth)}${txt}` : txt);
    run = "";
  };
  for (const c of Array.from(root.childNodes)) {
    const el = c as Element;
    if (c.nodeType !== 1 || !BLOCK.has(el.tagName)) {
      // An inline element around blocks (Docs wraps a whole paste in one <b>) is only a box.
      if (c.nodeType === 1 && !SKIP.has(el.tagName) && withBlock.has(el)) {
        flush();
        out.push(...blocks(el, depth));
      } else run += inline(c);
      continue;
    }
    flush();
    const item = wordItem(el);
    if (item) {
      const same = word?.list === item.list;
      const n = same && word ? word.n : [];
      n.length = item.level + 1;
      n[item.level] = (n[item.level] ?? 0) + 1;
      word = { list: item.list, n };
      const text = `${"   ".repeat(item.level)}${item.ordered ? `${n[item.level]}. ` : "- "}${line(el)}`;
      // Items of one list are one block; the next list starts after a blank line.
      if (same && out.length) out[out.length - 1] += `\n${text}`;
      else out.push(text);
      continue;
    }
    word = undefined;
    const tag = el.tagName;
    const h = /^H([1-6])$/.exec(tag);
    let b = "";
    // The post's title is its only top heading: document headings start one level down.
    if (h) b = line(el) && `${"#".repeat(Math.min(6, Number(h[1]) + 1))} ${line(el)}`;
    else if (tag === "UL" || tag === "OL") b = list(el, depth);
    else if (tag === "BLOCKQUOTE") b = blocks(el).join("\n\n").replace(/^/gm, "> ").replace(/^> $/gm, ">");
    else if (tag === "PRE") {
      const txt = text(el).replace(/\n$/, "");
      const fence = "`".repeat(fenceLen(txt, "`", 3));
      b = `${fence}\n${txt}\n${fence}`;
    } else if (tag === "TABLE") b = table(el);
    else if (tag === "HR") b = "---";
    else {
      const inner = blocks(el, 0);
      b = inner.join("\n\n");
    }
    if (b) out.push(depth && !/^\s/.test(b) ? b.replace(/^/gm, "   ".repeat(depth)) : b);
  }
  flush();
  return out;
}

/** Markdown for pasted HTML, or "" when it holds no text. */
export function htmlToMarkdown(body: Element): string {
  level = 0;
  // Document order without recursion; walked backwards, every element follows its descendants.
  withBlock = new WeakSet();
  const all: Element[] = [];
  for (const todo = [body]; todo.length; ) {
    const e = todo.pop() as Element;
    all.push(e);
    todo.push(...Array.from(e.children).reverse());
  }
  for (let i = all.length - 1; i >= 0; i--) {
    const e = all[i] as Element;
    if (BLOCK.has(e.tagName) || withBlock.has(e)) if (e.parentElement) withBlock.add(e.parentElement);
  }
  return blocks(body)
    .join("\n\n")
    .replace(/\n{3,}/g, "\n\n")
    .trim();
}
