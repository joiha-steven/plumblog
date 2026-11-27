/** What the Worker puts in the shell (crates/worker/src/admin.rs `boot_json`). */
export interface Boot {
  lang: string;
  langs: string[];
  site: string;
  /** This page's CSP nonce: the editor's base styles go in a `<style>` that must carry it. */
  nonce: string;
}

/** A language's UI text: `/admin/i18n/<lang>.js`, built by scripts/admin-build. */
export interface Text {
  /** Flat locale keys of the language; plural forms as `key.one`, `key.other`... */
  messages: Record<string, string>;
  /** English, for keys the language lacks. */
  fallback: Record<string, string>;
}

export function readBoot(): Boot {
  const text = document.getElementById("boot")?.textContent ?? "";
  try {
    return JSON.parse(text) as Boot;
  } catch (e) {
    // Without text the UI shows keys; still usable, and the log says why.
    console.error("admin: boot data unreadable", e);
    return { lang: "en", langs: ["en"], site: "", nonce: "" };
  }
}

/**
 * The UI text for `lang` (preloaded by the shell, so no extra wait). Falls back to English,
 * then to no text: the UI then shows keys, still usable, and the log says why.
 */
export async function loadText(lang: string): Promise<Text> {
  for (const l of lang === "en" ? ["en"] : [lang, "en"]) {
    // A URL in a variable: the bundler leaves it alone, the module is the build's own file.
    const url = `/admin/i18n/${encodeURIComponent(l)}.js`;
    try {
      const mod = (await import(url)) as { default: Text };
      return mod.default;
    } catch (e) {
      console.error(`admin: UI text ${url} failed to load`, e);
    }
  }
  return { messages: {}, fallback: {} };
}
