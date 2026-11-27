/** Light, dark or the system's choice; remembered per browser (a convenience, not a setting). */

export type ThemeChoice = "light" | "dark" | "system";
export const THEMES: readonly ThemeChoice[] = ["light", "dark", "system"];
const KEY = "pb.theme";

export function storedTheme(): ThemeChoice {
  try {
    const v = localStorage.getItem(KEY);
    return v === "light" || v === "dark" ? v : "system";
  } catch {
    return "system";
  }
}

export function applyTheme(choice: ThemeChoice): void {
  const root = document.documentElement;
  if (choice === "system") delete root.dataset["theme"];
  else root.dataset["theme"] = choice;
  try {
    localStorage.setItem(KEY, choice);
  } catch {
    // Private mode: the choice lasts until the tab closes.
  }
}
