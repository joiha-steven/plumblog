/** The segmented control the account menu and the sign-in screens share. */
import { h } from "./dom";
import { lang, langs, setLang, t } from "./i18n";

export function seg<T extends string>(label: string, values: readonly T[], chosen: T, name: (v: T) => string, pick: (v: T) => void): HTMLElement {
  const buttons = values.map((v) =>
    h(
      "button",
      {
        type: "button",
        "aria-pressed": String(v === chosen),
        onclick: () => {
          for (const b of buttons) b.setAttribute("aria-pressed", String(b === buttons[values.indexOf(v)]));
          pick(v);
        },
      },
      name(v),
    ),
  );
  return h("div", { class: "seg", role: "group", "aria-label": label }, ...buttons);
}

/** One key per language the admin speaks; picking one sets the cookie and reloads. */
export function langSwitch(): HTMLElement {
  // i18n: admin.lang.en admin.lang.vi
  const langName = (l: string) => t(`admin.lang.${l}`);
  return seg(t("admin.me.language"), langs(), lang(), langName, (l) => l !== lang() && setLang(l));
}
