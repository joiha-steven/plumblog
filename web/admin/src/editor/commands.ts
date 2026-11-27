/** Markdown edits the toolbar, bubble, keys and "/" menu share. */

import { EditorSelection } from "@codemirror/state";
import type { EditorView } from "@codemirror/view";

/** Wraps each selection in `open`/`close`, or unwraps it when it already is. */
export function wrap(view: EditorView, open: string, close = open): void {
  const { state } = view;
  view.dispatch(
    state.changeByRange((r) => {
      const text = state.sliceDoc(r.from, r.to);
      if (text.startsWith(open) && text.endsWith(close) && text.length >= open.length + close.length) {
        return {
          changes: { from: r.from, to: r.to, insert: text.slice(open.length, text.length - close.length) },
          range: EditorSelection.range(r.from, r.to - open.length - close.length),
        };
      }
      const before = state.sliceDoc(r.from - open.length, r.from);
      const after = state.sliceDoc(r.to, r.to + close.length);
      if (before === open && after === close) {
        return {
          changes: [
            { from: r.from - open.length, to: r.from },
            { from: r.to, to: r.to + close.length },
          ],
          range: EditorSelection.range(r.from - open.length, r.to - open.length),
        };
      }
      return {
        changes: [
          { from: r.from, insert: open },
          { from: r.to, insert: close },
        ],
        range: EditorSelection.range(r.from + open.length, r.to + open.length),
      };
    }),
  );
  view.focus();
}

const PREFIX = /^(#{1,6} |> |- \[[ x]\] |- |\d+\. )/;

/** Sets the current line's block prefix (`> `, `- `, `## `...), or removes it if it is the same. */
export function linePrefix(view: EditorView, prefix: string): void {
  const line = view.state.doc.lineAt(view.state.selection.main.head);
  const m = line.text.match(PREFIX);
  const same = m?.[0] === prefix;
  view.dispatch({ changes: { from: line.from, to: line.from + (m ? m[0].length : 0), insert: same ? "" : prefix } });
  view.focus();
}

/** Heading level 1-3 for the current line; 0 makes it a paragraph. */
export function heading(view: EditorView, level: number): void {
  const line = view.state.doc.lineAt(view.state.selection.main.head);
  const m = line.text.match(/^#{1,6} /);
  view.dispatch({ changes: { from: line.from, to: line.from + (m ? m[0].length : 0), insert: level ? `${"#".repeat(level)} ` : "" } });
  view.focus();
}

/**
 * Inserts a block at `from..to` (the typed "/word" when it comes from the menu), on a line of
 * its own, with the cursor `cursor` characters into it.
 */
export function insertBlock(view: EditorView, text: string, cursor: number, from: number, to: number): void {
  const line = view.state.doc.lineAt(from);
  const sep = from > line.from && line.text.slice(0, from - line.from).trim() ? "\n\n" : "";
  view.dispatch({
    changes: { from, to, insert: sep + text },
    selection: { anchor: from + sep.length + cursor },
    scrollIntoView: true,
  });
  view.focus();
}

/** Replaces `from..to` with `[text](url)`, the selection's text or `fallback`. */
export function link(view: EditorView, from: number, to: number, url: string, fallback: string): void {
  const text = view.state.sliceDoc(from, to) || fallback;
  const insert = `[${text}](${url})`;
  view.dispatch({ changes: { from, to, insert }, selection: { anchor: from + insert.length } });
  view.focus();
}

/** Words for the counter: Markdown syntax and URLs do not count. */
export function countWords(text: string): number {
  return text
    .replace(/!\[[^\]]*\]\([^)]*\)|\]\([^)]*\)|[#>*_`~|[\]^:$-]+/g, " ")
    .split(/\s+/)
    .filter(Boolean).length;
}
