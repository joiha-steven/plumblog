/**
 * Blocks the "/" menu offers. Themes and plugins never add code here: a later declarative
 * hook can add snippets the same shape (docs/ARCHITECTURE.md, Plugins and options).
 */

import type { EditorView } from "@codemirror/view";
import type { IconName } from "../icons";
import { t } from "../i18n";
import { insertBlock } from "./commands";

export interface Block {
  id: string;
  icon: IconName;
  run: (view: EditorView, from: number, to: number) => void;
}

const snippet =
  (text: () => string, cursor: number) =>
  (v: EditorView, from: number, to: number): void =>
    insertBlock(v, text(), cursor, from, to);

const lineStart =
  (prefix: string) =>
  (v: EditorView, from: number, to: number): void => {
    v.dispatch({ changes: { from, to, insert: prefix }, selection: { anchor: from + prefix.length } });
    v.focus();
  };

/** Opens the picture picker at a position; the editor screen sets it. */
export const picker = { open: (_at: number): void => undefined };

export const BLOCKS: readonly Block[] = [
  { id: "h2", icon: "heading", run: lineStart("## ") },
  { id: "h3", icon: "heading", run: lineStart("### ") },
  { id: "list", icon: "ul", run: snippet(() => "- ", 2) },
  { id: "numbered", icon: "ol", run: snippet(() => "1. ", 3) },
  { id: "task", icon: "task", run: snippet(() => "- [ ] ", 6) },
  { id: "quote", icon: "quote", run: snippet(() => "> ", 2) },
  {
    id: "picture",
    icon: "image",
    run: (v, from, to) => {
      v.dispatch({ changes: { from, to } });
      picker.open(from);
    },
  },
  { id: "callout", icon: "note", run: snippet(() => "> [!NOTE]\n> ", 12) },
  { id: "code", icon: "code", run: snippet(() => "```\n\n```\n", 4) },
  { id: "math", icon: "math", run: snippet(() => "$$\n\n$$\n", 3) },
  {
    id: "table",
    icon: "table",
    run: snippet(() => `| ${t("admin.editor.table_column", { n: 1 })} | ${t("admin.editor.table_column", { n: 2 })} |\n| --- | --- |\n|  |  |\n`, 2),
  },
  {
    id: "footnote",
    icon: "footnote",
    run: (v, from, to) => {
      const doc = v.state.doc.toString();
      const n = (doc.match(/^\[\^\d+\]:/gm) ?? []).length + 1;
      const ref = `[^${n}]`;
      const tail = `${doc.endsWith("\n") ? "" : "\n"}\n${ref}: `;
      v.dispatch({ changes: [{ from, to, insert: ref }, { from: doc.length, insert: tail }] });
      v.dispatch({ selection: { anchor: v.state.doc.length }, scrollIntoView: true });
      v.focus();
    },
  },
  { id: "divider", icon: "hr", run: snippet(() => "---\n\n", 5) },
];

/** The block's name in the UI language. */
export const blockName = (b: Block): string =>
  // i18n: admin.editor.block.h2 admin.editor.block.h3 admin.editor.block.list admin.editor.block.numbered admin.editor.block.task admin.editor.block.quote admin.editor.block.picture admin.editor.block.callout admin.editor.block.code admin.editor.block.math admin.editor.block.table admin.editor.block.footnote admin.editor.block.divider
  t(`admin.editor.block.${b.id}`);

/** Lowercase without marks, so "/bang" finds the Vietnamese "table" item and "/table" finds it too. */
export const fold = (s: string): string =>
  s
    .normalize("NFD")
    .replace(/[\u0300-\u036f]/g, "")
    .replace(/\u0111/g, "d")
    .replace(/\u0110/g, "d")
    .toLowerCase();
