/** Escaping for text placed inside Markdown links and pictures. No imports: paste-md and the editor share it. */

/** A link target that cannot end early: `\`, `<`, `>`, `(`, `)` and any whitespace are percent-encoded. */
export const mdUrl = (u: string): string =>
  u.replace(/[\\<>()\s]/gu, (c) => (c < "\x80" ? `%${c.charCodeAt(0).toString(16).toUpperCase().padStart(2, "0")}` : encodeURIComponent(c)));

/** A picture description on one line, with the characters that would close `![...]` escaped. */
export const mdAlt = (s: string): string => s.replace(/\s+/g, " ").trim().replace(/[\\[\]]/g, "\\$&");
