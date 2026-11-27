import type { Boot, Text } from "./boot";

export type Args = Record<string, string | number>;

let boot: Boot = { lang: "en", langs: ["en"], site: "", nonce: "" };
let strings: Text = { messages: {}, fallback: {} };
let rules = new Intl.PluralRules("en");
const english = new Intl.PluralRules("en");
const warned = new Set<string>();

export function initI18n(b: Boot, tx: Text): void {
  boot = b;
  strings = tx;
  rules = new Intl.PluralRules(b.lang);
}

export const lang = (): string => boot.lang;
export const langs = (): readonly string[] => boot.langs;

/**
 * The text for `key` (always a literal: scripts/check reads every `t("...")` call), with
 * `{name}` filled from `args`. A plural message picks its form from `args.count`. A missing
 * key shows the key itself and is logged once, like `i18n.missing_key` on the server.
 */
export function t(key: string, args: Args = {}): string {
  const text = lookup(key, args["count"]);
  if (text === undefined) {
    if (!warned.has(key)) {
      warned.add(key);
      console.warn(`i18n.missing_key lang=${boot.lang} key=${key}`);
    }
    return key;
  }
  return text.replace(/\{(\w+)\}/g, (whole, name: string) => {
    const v = args[name];
    return v === undefined ? whole : typeof v === "number" ? num(v) : v;
  });
}

function lookup(key: string, count: string | number | undefined): string | undefined {
  const own = strings.messages;
  const fb = strings.fallback;
  const direct = own[key] ?? fb[key];
  if (direct !== undefined || typeof count !== "number") return direct;
  return (
    own[`${key}.${rules.select(count)}`] ??
    own[`${key}.other`] ??
    fb[`${key}.${english.select(count)}`] ??
    fb[`${key}.other`]
  );
}

export const num = (n: number): string => new Intl.NumberFormat(boot.lang).format(n);

/** Remembered by the Worker (cookie), which sends the next page in that language. */
export function setLang(l: string): void {
  document.cookie = `pb_lang=${encodeURIComponent(l)}; path=/admin; max-age=31536000; samesite=strict`;
  location.reload();
}
