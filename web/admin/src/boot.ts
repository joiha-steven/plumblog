/** What the Worker puts in the shell (crates/worker/src/admin.rs `boot_json`). */
export interface Boot {
  lang: string;
  langs: string[];
  site: string;
  /** Flat locale keys of the chosen language; plural forms as `key.one`, `key.other`... */
  messages: Record<string, string>;
  /** English, for keys the chosen language lacks. */
  fallback: Record<string, string>;
}

export function readBoot(): Boot {
  const text = document.getElementById("boot")?.textContent ?? "";
  try {
    return JSON.parse(text) as Boot;
  } catch (e) {
    // Without text the UI shows keys; still usable, and the log says why.
    console.error("admin: boot data unreadable", e);
    return { lang: "en", langs: ["en"], site: "", messages: {}, fallback: {} };
  }
}
