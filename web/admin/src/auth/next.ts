/**
 * Where to go after signing in. `?next=` comes from the address bar, so anyone can write it:
 * only a plain path inside the admin passes, everything doubtful falls back to the admin home.
 * No imports: tests load it straight into Node.
 */

const HOME = "/admin/";
const ORIGIN = "https://admin.invalid";
/** Control characters, whitespace (Unicode included) and invisible format characters. */
const BAD_CHAR = /[\s\u0000-\u001f\u007f-\u009f\u00ad\u180e\u200b-\u200f\u202a-\u202e\u2060-\u2064\ufeff]/;
/** Percent escapes of "/", "\" and "." (a path trick once decoded, by us or a server). */
const ENCODED_SEP = /%(2f|5c|2e)/i;
/** Decoding rounds before a value is called hostile (each round peels one %25). */
const MAX_DECODES = 4;

/** True when `s`, or any decoding of it, holds something a safe path never needs. */
function hostile(s: string): boolean {
  let cur = s;
  for (let i = 0; i <= MAX_DECODES; i++) {
    if (BAD_CHAR.test(cur) || cur.includes("\\") || ENCODED_SEP.test(cur)) return true;
    let next: string;
    try {
      next = decodeURIComponent(cur);
    } catch {
      return true;
    }
    if (next === cur) return false;
    cur = next;
  }
  return true;
}

/** `raw` when it is a same-origin path under `/admin/` (query and hash allowed), else `/admin/`. */
export function safeNext(raw: string | null | undefined): string {
  if (typeof raw !== "string" || !raw.startsWith(HOME) || raw.length > 2048) return HOME;
  if (hostile(raw)) return HOME;
  const path = raw.split(/[?#]/, 1)[0] ?? "";
  // "//" would read as an empty segment; "." and ".." climb or alias.
  const segments = path.slice(1).split("/");
  if (segments.slice(0, -1).some((s) => s === "" || s === "." || s === "..")) return HOME;
  const last = segments[segments.length - 1];
  if (last === "." || last === "..") return HOME;
  // The browser's own reading must agree: same origin, still under /admin/.
  let url: URL;
  try {
    url = new URL(raw, ORIGIN);
  } catch {
    return HOME;
  }
  if (url.origin !== ORIGIN || !url.pathname.startsWith(HOME)) return HOME;
  return raw;
}
