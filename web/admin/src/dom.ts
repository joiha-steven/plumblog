/** A small DOM builder: the admin has no framework (docs/ARCHITECTURE.md, Admin and editor). */

export type Child = Node | string | null | undefined | false | readonly Child[];
type Value = string | number | boolean | null | undefined;
type Handler = (e: Event) => void;
export type Attrs = Record<string, Value | Handler>;

export function h<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  attrs: Attrs = {},
  ...children: Child[]
): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (typeof v === "function") {
      el.addEventListener(k.replace(/^on/, ""), v);
    } else if (k === "style") {
      // The CSP blocks style attributes; layout belongs to web/admin/css.
      throw new Error("h(): no style attribute; use a class from web/admin/css");
    } else if (v !== null && v !== undefined && v !== false) {
      el.setAttribute(k, v === true ? "" : String(v));
    }
  }
  append(el, children);
  return el;
}

export function append(el: Node, children: readonly Child[]): void {
  for (const c of children) {
    if (c === null || c === undefined || c === false) continue;
    if (typeof c === "string") el.appendChild(document.createTextNode(c));
    else if (c instanceof Node) el.appendChild(c);
    else append(el, c);
  }
}
