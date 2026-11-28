/** The level mark: never colour alone, an icon and the word; info stays a quiet word. */

import type { LogLevel } from "../api/model.gen";
import { h } from "../dom";
import { icon } from "../icons";
import { t } from "../i18n";

const SVG = "http://www.w3.org/2000/svg";

/** The warning triangle (the shell's icon set has the round alert for errors only). */
function triangle(): SVGSVGElement {
  const svg = document.createElementNS(SVG, "svg");
  svg.setAttribute("class", "i");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("aria-hidden", "true");
  // Constant markup, never user text.
  svg.innerHTML = '<path d="M12 4 2.5 20h19z"/><path d="M12 10v4.5M12 17.5v.01"/>';
  return svg;
}

export function levelName(level: LogLevel): string {
  switch (level) {
    case "info":
      return t("admin.syslog.lvl_info");
    case "warning":
      return t("admin.syslog.lvl_warning");
    case "error":
      return t("admin.syslog.lvl_error");
  }
}

export function levelMark(level: LogLevel): HTMLElement {
  if (level === "info") return h("span", { class: "log-info" }, levelName(level));
  return h("span", { class: `st ic ${level}` }, level === "error" ? icon("alert") : triangle(), levelName(level));
}
