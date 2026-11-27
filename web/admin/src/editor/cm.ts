/**
 * CodeMirror set up for writing: Markdown marks stay visible but dimmed, and the text takes
 * the style it will have when published (headings, quotes, code, callouts, pictures).
 */

import { history, historyKeymap, indentWithTab, standardKeymap } from "@codemirror/commands";
import { HighlightStyle, ensureSyntaxTree, syntaxHighlighting, syntaxTree, LanguageSupport } from "@codemirror/language";
import { deleteMarkupBackward, insertNewlineContinueMarkup, markdownLanguage } from "@codemirror/lang-markdown";
import { EditorState, type Extension, Prec, RangeSetBuilder, StateField, type Transaction } from "@codemirror/state";
import { Decoration, type DecorationSet, EditorView, type KeyBinding, ViewPlugin, type ViewUpdate, WidgetType, keymap, placeholder } from "@codemirror/view";
import { tags } from "@lezer/highlight";
import { wrap } from "./commands";

// Only the Markdown language, not `markdown()`: that one also loads HTML, CSS and JS parsers
// for embedded code, which the bundle budget cannot carry and posts cannot contain.
const language = new LanguageSupport(markdownLanguage);

const style = HighlightStyle.define([
  { tag: tags.processingInstruction, class: "cm-mk" },
  { tag: tags.contentSeparator, class: "cm-mk" },
  { tag: tags.emphasis, class: "cm-em" },
  { tag: tags.strong, class: "cm-strong" },
  { tag: tags.strikethrough, class: "cm-strike" },
  { tag: tags.link, class: "cm-link" },
  { tag: tags.url, class: "cm-url" },
  { tag: tags.monospace, class: "cm-ic" },
]);

/** A picture written alone on its line shows under it, with its description as caption. */
class Picture extends WidgetType {
  constructor(
    readonly src: string,
    readonly alt: string,
    readonly current: boolean,
    readonly missingAlt: string,
  ) {
    super();
  }
  override eq(o: Picture): boolean {
    return o.src === this.src && o.alt === this.alt && o.current === this.current;
  }
  override toDOM(): HTMLElement {
    const box = document.createElement("div");
    box.className = this.current ? "cm-img cm-cur" : "cm-img";
    // The CSP allows pictures from this site only; others keep just their caption here.
    if (this.src.startsWith("/")) {
      const img = document.createElement("img");
      img.src = this.src;
      img.alt = this.alt;
      img.loading = "lazy";
      box.append(img);
    }
    const cap = document.createElement("div");
    cap.className = this.alt ? "cm-cap" : "cm-cap miss";
    cap.textContent = this.alt || this.missingAlt;
    box.append(cap);
    return box;
  }
  override ignoreEvent(): boolean {
    return false;
  }
}

/** The paragraph holding the cursor (blank-line delimited), lit in focus mode. */
function paragraph(state: EditorState): [number, number] {
  const doc = state.doc;
  const n = doc.lineAt(state.selection.main.head).number;
  if (!doc.line(n).text.trim()) return [n, n];
  let a = n;
  let z = n;
  while (a > 1 && doc.line(a - 1).text.trim()) a--;
  while (z < doc.lines && doc.line(z + 1).text.trim()) z++;
  return [a, z];
}

function decorate(state: EditorState, missingAlt: string): DecorationSet {
  const [curFrom, curTo] = paragraph(state);
  const tree = ensureSyntaxTree(state, state.doc.length, 50) ?? syntaxTree(state);
  const lines = new Map<number, string[]>();
  const widgets: [number, Decoration][] = [];
  const add = (n: number, c: string) => lines.set(n, [...(lines.get(n) ?? []), c]);
  tree.iterate({
    enter(node) {
      const l1 = state.doc.lineAt(node.from).number;
      const l2 = state.doc.lineAt(node.to).number;
      const h = /^ATXHeading([1-6])$/.exec(node.name);
      if (h) add(l1, `cm-h${Math.min(3, Number(h[1]))}`);
      else if (node.name === "Blockquote") {
        const callout = /^>\s*\[!(NOTE|TIP|IMPORTANT|WARNING|CAUTION)\]/i.test(state.doc.line(l1).text);
        for (let i = l1; i <= l2; i++) add(i, callout ? "cm-callout" : "cm-quote");
      } else if (node.name === "FencedCode") {
        for (let i = l1; i <= l2; i++) add(i, i === l1 ? "cm-code cm-code-first" : i === l2 ? "cm-code cm-code-last" : "cm-code");
        return false;
      } else if (node.name === "HorizontalRule") add(l1, "cm-hr");
      else if (node.name === "Image") {
        const text = state.doc.sliceString(node.from, node.to);
        const m = /^!\[([^\]]*)\]\(([^)\s]*)(?:\s+"[^"]*")?\)$/.exec(text);
        const line = state.doc.lineAt(node.to);
        if (m && line.text.trim() === text) {
          const cur = line.number >= curFrom && line.number <= curTo;
          widgets.push([line.to, Decoration.widget({ widget: new Picture(m[2] ?? "", m[1] ?? "", cur, missingAlt), block: true, side: 1 })]);
          add(line.number, "cm-imgsrc");
        }
      }
      return undefined;
    },
  });
  // Display math blocks: `$$` lines and what is between them.
  let inMath = false;
  for (let i = 1; i <= state.doc.lines; i++) {
    const text = state.doc.line(i).text;
    const fence = text.trim() === "$$";
    if (fence || inMath) add(i, "cm-math");
    if (fence) inMath = !inMath;
    if (/^\[\^[^\]]+\]:/.test(text)) add(i, "cm-fn");
    if (i >= curFrom && i <= curTo) add(i, "cm-cur");
  }
  const all: [number, Decoration, number][] = [];
  for (const [n, c] of lines) all.push([state.doc.line(n).from, Decoration.line({ class: c.join(" ") }), 0]);
  for (const [p, d] of widgets) all.push([p, d, 1]);
  all.sort((a, b) => a[0] - b[0] || a[2] - b[2]);
  const b = new RangeSetBuilder<Decoration>();
  for (const [p, d] of all) b.add(p, p, d);
  return b.finish();
}

function decorations(missingAlt: string): Extension {
  return StateField.define<DecorationSet>({
    create: (s) => decorate(s, missingAlt),
    update(v, tr: Transaction) {
      return tr.docChanged || tr.selection || syntaxTree(tr.startState) !== syntaxTree(tr.state) ? decorate(tr.state, missingAlt) : v;
    },
    provide: (f) => EditorView.decorations.from(f),
  });
}

/** Footnote references `[^1]` look like the superscript they become. */
const footnoteRefs = ViewPlugin.fromClass(
  class {
    decorations: DecorationSet;
    constructor(v: EditorView) {
      this.decorations = this.build(v);
    }
    update(u: ViewUpdate) {
      if (u.docChanged || u.viewportChanged) this.decorations = this.build(u.view);
    }
    build(v: EditorView): DecorationSet {
      const b = new RangeSetBuilder<Decoration>();
      for (const { from, to } of v.visibleRanges) {
        const text = v.state.doc.sliceString(from, to);
        for (const m of text.matchAll(/\[\^[^\]]+\](?!:)/g)) {
          b.add(from + (m.index ?? 0), from + (m.index ?? 0) + m[0].length, Decoration.mark({ class: "cm-fnref" }));
        }
      }
      return b.finish();
    }
  },
  { decorations: (p) => p.decorations },
);

export interface Hooks {
  lang: string;
  nonce: string;
  label: string;
  placeholder: string;
  missingAlt: string;
  /** Checked first: the "/" menu's arrows, Enter and Escape. */
  keys: KeyBinding[];
  link: (v: EditorView) => void;
  update: (u: ViewUpdate) => void;
  events: Parameters<typeof EditorView.domEventHandlers>[0];
}

export function createEditor(parent: HTMLElement, doc: string, hooks: Hooks): EditorView {
  const state = EditorState.create({
    doc,
    extensions: [
      // CodeMirror puts its base theme in a <style> element; the admin's CSP admits it only
      // with the nonce the Worker sent for this page.
      EditorView.cspNonce.of(hooks.nonce),
      history(),
      language,
      syntaxHighlighting(style),
      decorations(hooks.missingAlt),
      footnoteRefs,
      EditorView.lineWrapping,
      placeholder(hooks.placeholder),
      EditorView.contentAttributes.of({
        "aria-label": hooks.label,
        spellcheck: "true",
        autocorrect: "on",
        autocapitalize: "sentences",
        lang: hooks.lang,
      }),
      Prec.highest(keymap.of(hooks.keys)),
      keymap.of([
        { key: "Mod-b", run: (v) => (wrap(v, "**"), true) },
        { key: "Mod-i", run: (v) => (wrap(v, "*"), true) },
        { key: "Mod-k", run: (v) => (hooks.link(v), true) },
        // Enter continues a list or quote; Backspace at its start removes the mark.
        { key: "Enter", run: insertNewlineContinueMarkup },
        { key: "Backspace", run: deleteMarkupBackward },
        // The standard keys, not defaultKeymap: its code-editor extras (move or copy lines,
        // toggle comments, bracket selection) cost bundle size and mean nothing in prose.
        ...standardKeymap,
        ...historyKeymap,
        indentWithTab,
      ]),
      EditorView.updateListener.of(hooks.update),
      EditorView.domEventHandlers(hooks.events),
    ],
  });
  return new EditorView({ state, parent });
}
