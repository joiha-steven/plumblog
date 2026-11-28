/** The admin's one line icon set (24 px grid, 1.5 px stroke; docs/ARCHITECTURE.md, Icons). */

const PATHS = {
  posts: '<path d="M14 3H7a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h10a2 2 0 0 0 2-2V8z"/><path d="M14 3v5h5M9 13h6M9 17h4"/>',
  pages: '<rect x="4" y="3" width="16" height="18" rx="2"/><path d="M8 7h8M8 11h8M8 15h5"/>',
  media: '<rect x="3" y="4" width="18" height="16" rx="2"/><circle cx="9" cy="10" r="2"/><path d="m21 16-5-5-9 9"/>',
  settings: '<path d="M4 6h10M18 6h2M4 12h4M12 12h8M4 18h12M20 18h0"/><circle cx="16" cy="6" r="2"/><circle cx="10" cy="12" r="2"/><circle cx="18" cy="18" r="2"/>',
  plus: '<path d="M12 5v14M5 12h14"/>',
  upload: '<path d="M12 16V4M7 9l5-5 5 5M4 20h16"/>',
  user: '<circle cx="12" cy="8" r="4"/><path d="M4 21a8 8 0 0 1 16 0"/>',
  offline: '<path d="M2 8.5a15 15 0 0 1 20 0M5 12a10 10 0 0 1 14 0M8.5 15.5a5 5 0 0 1 7 0"/><path d="M3 3l18 18"/>',
  lock: '<rect x="5" y="11" width="14" height="10" rx="2"/><path d="M8 11V7a4 4 0 0 1 8 0v4"/>',
  alert: '<circle cx="12" cy="12" r="9"/><path d="M12 7.5v5.5M12 16.5v.01"/>',
  compass: '<circle cx="12" cy="12" r="9"/><path d="m15.5 8.5-2 5-5 2 2-5z"/>',
  back: '<path d="M15 18 9 12l6-6"/>',
  pen: '<path d="M4 20h4L19 9l-4-4L4 16z"/><path d="m13.5 6.5 4 4"/>',
  split: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M12 4v16"/>',
  eye: '<path d="M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12z"/><circle cx="12" cy="12" r="3"/>',
  heading: '<path d="M6 5v14M18 5v14M6 12h12"/>',
  bold: '<path d="M7 5h5.5a3.5 3.5 0 0 1 0 7H7zM7 12h6.5a3.5 3.5 0 0 1 0 7H7z"/>',
  italic: '<path d="M10 5h8M6 19h8M14 5l-4 14"/>',
  strike: '<path d="M4 12h16M16 7.5C15.4 5.9 13.9 5 12 5c-2.4 0-4 1.3-4 3 0 1.1.7 2 2.2 2.6M8 16.5c.6 1.6 2.1 2.5 4 2.5 2.4 0 4-1.3 4-3 0-.6-.2-1.1-.5-1.6"/>',
  code: '<path d="m8 8-4 4 4 4M16 8l4 4-4 4M13.5 5l-3 14"/>',
  link: '<path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1"/><path d="M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1"/>',
  quote: '<path d="M7 7h4v4c0 3-1 5-4 6M14 7h4v4c0 3-1 5-4 6"/>',
  ul: '<path d="M9 6h11M9 12h11M9 18h11"/><circle cx="4.5" cy="6" r=".9"/><circle cx="4.5" cy="12" r=".9"/><circle cx="4.5" cy="18" r=".9"/>',
  ol: '<path d="M10 6h10M10 12h10M10 18h10M4 4.5 5.5 4v5M4 9h3M4 14.2c.3-.7.9-1.2 1.6-1.2.8 0 1.4.6 1.4 1.3 0 1.4-3 2.2-3 4.2h3"/>',
  task: '<rect x="3.5" y="4.5" width="5" height="5" rx="1"/><path d="m4 16.5 1.5 1.5 3-3.5M12 7h8M12 17h8"/>',
  table: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 10h18M3 15h18M10 10v10"/>',
  footnote: '<path d="M4 18h9M4 13h11M4 8h7"/><path d="M17 4.5 18.5 4v6M17 10h3"/>',
  math: '<path d="M18 5H6l6 7-6 7h12"/>',
  hr: '<path d="M4 12h16"/><path d="M9 7h6M9 17h6" opacity=".5"/>',
  note: '<rect x="3" y="5" width="18" height="14" rx="2"/><path d="M8 10h8M8 14h5"/>',
  undo: '<path d="M9 14 4 9l5-5"/><path d="M4 9h10.5a5.5 5.5 0 0 1 0 11H11"/>',
  redo: '<path d="m15 14 5-5-5-5"/><path d="M20 9H9.5a5.5 5.5 0 0 0 0 11H13"/>',
  more: '<circle cx="5" cy="12" r="1.1"/><circle cx="12" cy="12" r="1.1"/><circle cx="19" cy="12" r="1.1"/>',
  chev: '<path d="m7 10 5 5 5-5"/>',
  focus: '<path d="M4 8V5a1 1 0 0 1 1-1h3M16 4h3a1 1 0 0 1 1 1v3M20 16v3a1 1 0 0 1-1 1h-3M8 20H5a1 1 0 0 1-1-1v-3"/>',
  type: '<path d="M4 12h16M4 6h10M4 18h7"/>',
  check: '<path d="m5 12.5 4.5 4.5L19 7.5"/>',
  image: '<rect x="3" y="4" width="18" height="16" rx="2"/><circle cx="9" cy="10" r="2"/><path d="m21 16-5-5-9 9"/>',
  panel: '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M15 4v16"/>',
  close: '<path d="M6 6l12 12M18 6 6 18"/>',
  plum: '<path d="M12 7c4.5 0 7 3 7 7a7 7 0 0 1-14 0c0-4 2.5-7 7-7z" fill="currentColor" stroke="none"/><path d="M12 7c0-2 1-3.5 3-4"/>',
} as const;

export type IconName = keyof typeof PATHS;

/** Themes and plugins name their group icons; one this set lacks is simply not drawn. */
export const hasIcon = (name: string): name is IconName => Object.hasOwn(PATHS, name);

const SVG = "http://www.w3.org/2000/svg";

export function icon(name: IconName, cls = ""): SVGSVGElement {
  const svg = document.createElementNS(SVG, "svg");
  svg.setAttribute("class", cls ? `i ${cls}` : "i");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("aria-hidden", "true");
  // Constant markup from the table above, never user text.
  svg.innerHTML = PATHS[name];
  return svg;
}
