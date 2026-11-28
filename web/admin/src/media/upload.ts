/**
 * Adding pictures to the library: the Upload button, drag-and-drop onto the screen and paste.
 * Each file gets a row with progress and Cancel; two at a time go out, the rest wait. Every
 * file is hashed first, so a repeat of one the caller already stored answers with that item
 * (the API's `sha256` dedupe) and the screen says so instead of adding a second tile.
 */

import type { Api, MediaItem } from "../api/client";
import { h } from "../dom";
import { icon } from "../icons";
import { t } from "../i18n";
import { toast } from "../ui";
import { Pool, UPLOAD_PARALLEL, UPLOAD_TYPES, faultOf, refuseFile, sha256Of } from "./logic";

export interface UploadHost {
  api: Api;
  /** The whole screen; pasting works while it is on the page. */
  box: HTMLElement;
  /** The column that takes dropped files. */
  main: HTMLElement;
  /** A new picture, for the top of the grid. */
  added(item: MediaItem): void;
  /** A picture the caller already had; `quiet` when other uploads are still showing their rows. */
  existing(item: MediaItem, quiet: boolean): void;
  /** Opens a picture's details. */
  show(item: MediaItem): void;
}

export interface Uploader {
  /** The button and its hidden file input. */
  button: HTMLElement;
  /** The rows of files being sent, or that failed. */
  list: HTMLElement;
  /** "Drop pictures here". */
  hint: HTMLElement;
}

const pct = (n: number): string => String(Math.round(n * 100));

export function mountUpload(host: UploadHost): Uploader {
  const pool = new Pool(UPLOAD_PARALLEL);
  /** Files that are sending or waiting and not cancelled: what "other work" means to the screen. */
  let live = 0;
  const list = h("div", { class: "ups" });

  const input = h("input", { type: "file", multiple: true, accept: UPLOAD_TYPES.join(","), class: "sr", tabindex: -1, "aria-hidden": "true" });
  input.addEventListener("change", () => {
    add([...(input.files ?? [])]);
    // The same file can be picked again.
    input.value = "";
  });
  const button = h("span", { class: "up-add" }, h("button", { type: "button", class: "btn", onclick: () => input.click() }, icon("upload"), t("admin.media.up_button")), input);
  const hint = h("p", { class: "drop-hint hint" }, icon("upload"), t("admin.media.up_drop"));

  /** One row; `body` is what it shows now. */
  function rowFor(): { el: HTMLElement; set(...c: (Node | string)[]): void } {
    const el = h("div", { class: "up-row" });
    const set = (...c: (Node | string)[]): void => el.replaceChildren(...c);
    list.append(el);
    return { el, set };
  }

  const dismissBtn = (el: HTMLElement): HTMLElement => h("button", { type: "button", class: "btn quiet", onclick: () => el.remove() }, t("admin.media.up_dismiss"));

  function failRow(row: ReturnType<typeof rowFor>, text: string, retry?: () => void): void {
    const acts = h("div", { class: "up-acts" });
    if (retry) acts.append(h("button", { type: "button", class: "btn quiet", onclick: retry }, t("admin.state.retry")));
    acts.append(dismissBtn(row.el));
    row.el.className = "up-row is-err";
    row.set(h("p", { class: "up-msg", role: "alert" }, icon("alert"), text), acts);
  }

  /** The words for a refused or failed file. */
  const faultText = (fault: ReturnType<typeof faultOf>, name: string, field?: string): string => {
    // The field name is for the log; the writer gets what it means.
    const refused = (): string => {
      if (field === "sha256") return t("admin.media.up_bad_hash", { name });
      if (field === "width" || field === "height") return t("admin.media.up_bad_size", { name });
      if (field === "mime") return t("admin.media.up_bad_type", { name });
      return t("admin.media.up_failed", { name });
    };
    switch (fault) {
      case "unreadable":
        return t("admin.media.up_unreadable", { name });
      case "too_big":
        return t("admin.media.up_size", { name });
      case "offline":
        return t("admin.media.up_offline", { name });
      case "forbidden":
        return t("admin.media.up_forbidden");
      case "refused":
        return refused();
      default:
        return t("admin.media.up_failed", { name });
    }
  };

  function send(file: File, row: ReturnType<typeof rowFor>): void {
    const ctl = new AbortController();
    const bar = h("i", {});
    bar.style.transform = "scaleX(0)";
    const track = h("div", { class: "up-bar", role: "progressbar", "aria-label": file.name, "aria-valuemin": 0, "aria-valuemax": 100, "aria-valuenow": 0 }, bar);
    const status = h("span", { class: "up-state" }, t("admin.media.up_waiting"));
    live++;
    let ended = false;
    const end = (): void => {
      if (!ended) live--;
      ended = true;
    };
    const cancel = h("button", { type: "button", class: "btn quiet", onclick: () => (ctl.abort(), end(), row.el.remove()) }, t("admin.media.up_cancel"));
    row.el.className = "up-row";
    row.set(h("div", { class: "up-head" }, h("b", { class: "up-name" }, file.name), status), track, h("div", { class: "up-acts" }, cancel));
    const progress = (sent: number): void => {
      bar.style.transform = `scaleX(${sent})`;
      track.setAttribute("aria-valuenow", pct(sent));
      status.textContent = sent >= 1 ? t("admin.media.up_saving") : t("admin.media.up_sending", { percent: pct(sent) });
    };
    pool.add(async () => {
      try {
        await run();
      } finally {
        end();
      }
    });
    async function run(): Promise<void> {
      if (ctl.signal.aborted) return;
      status.textContent = t("admin.media.up_preparing");
      let sha: string | undefined;
      try {
        sha = await sha256Of(file);
      } catch (e) {
        // Without the hash the file still goes; only the repeat check is lost.
        console.error(`admin media: hashing ${file.name} failed`, e);
      }
      if (sha === undefined) console.error(`admin media: ${file.name} is sent without a sha256 (no hash from this browser); a repeat will not be recognised`);
      if (ctl.signal.aborted) return;
      progress(0);
      const r = await host.api.upload(file, progress, ctl.signal, sha);
      if (ctl.signal.aborted || (!r.ok && r.error.code === "cancelled")) return row.el.remove();
      if (!r.ok) {
        if (r.error.code === "auth.unauthenticated") return row.el.remove();
        console.error(`admin media: uploading ${file.name} failed: ${r.error.code} (${r.error.status})${r.error.field ? ` on ${r.error.field}` : ""}`);
        const text = faultText(faultOf(r.error), file.name, r.error.field);
        // The screen was left (or the row removed) while it was out: a toast, as for a success.
        if (!row.el.isConnected) return toast(text, 8000);
        return failRow(row, text, () => send(file, row));
      }
      const item = r.value.item;
      if (r.value.existing) {
        const open = h("button", { type: "button", class: "btn quiet", onclick: () => host.show(item) }, t("admin.media.up_show"));
        row.el.className = "up-row is-dup";
        row.set(h("p", { class: "up-msg", role: "status" }, icon("check"), t("admin.media.up_exists", { name: file.name })), h("div", { class: "up-acts" }, open, dismissBtn(row.el)));
        // On a phone the details replace the grid and its rows, so the message is also a toast.
        toast(t("admin.media.up_exists", { name: file.name }), 6000);
        end();
        host.existing(item, live > 0);
        return;
      }
      row.el.remove();
      host.added(item);
      toast(t("admin.media.up_added", { name: file.name }));
    }
  }

  /** Checks each file, then queues it; a refused one says why and nothing is sent. */
  function add(files: File[]): void {
    for (const file of files) {
      const row = rowFor();
      const no = refuseFile(file);
      if (no) failRow(row, no === "type" ? t("admin.media.up_type", { name: file.name }) : t("admin.media.up_size", { name: file.name }));
      else send(file, row);
    }
    // The first row of a batch is what the writer should notice.
    list.querySelector<HTMLElement>(".up-row")?.scrollIntoView({ block: "nearest" });
  }

  // ---- drop -----------------------------------------------------------------------------
  const hasFiles = (e: DragEvent): boolean => e.dataTransfer?.types.includes("Files") ?? false;
  let depth = 0;
  host.main.classList.add("drop");
  host.main.addEventListener("dragenter", (e) => {
    if (!hasFiles(e)) return;
    e.preventDefault();
    depth++;
    host.main.classList.add("over");
  });
  host.main.addEventListener("dragover", (e) => {
    if (!hasFiles(e)) return;
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "copy";
  });
  host.main.addEventListener("dragleave", (e) => {
    if (!hasFiles(e)) return;
    depth = Math.max(0, depth - 1);
    if (depth === 0) host.main.classList.remove("over");
  });
  host.main.addEventListener("drop", (e) => {
    if (!hasFiles(e)) return;
    e.preventDefault();
    depth = 0;
    host.main.classList.remove("over");
    add([...(e.dataTransfer?.files ?? [])]);
  });

  // ---- paste ----------------------------------------------------------------------------
  const onPaste = (e: ClipboardEvent): void => {
    // The screen is gone: this listener goes with it.
    if (!host.box.isConnected) return document.removeEventListener("paste", onPaste);
    // Words pasted into the description (or any field) stay words.
    if (e.target instanceof HTMLElement && e.target.closest("input, textarea, [contenteditable]")) return;
    // The full-screen viewer is open over the screen.
    if (document.querySelector(".lb")) return;
    const files = [...(e.clipboardData?.files ?? [])];
    if (!files.length) return;
    e.preventDefault();
    add(files);
  };
  document.addEventListener("paste", onPaste);

  return { button, list, hint };
}

/**
 * A file dropped outside the drop target would make the browser open it and leave the admin,
 * losing what is on screen: while the screen is mounted, every file drag is claimed, and only
 * `main` takes the files (for those who may upload).
 */
export function guardFileDrops(box: HTMLElement, main: HTMLElement): void {
  const claim = (e: DragEvent): void => {
    if (!box.isConnected) {
      document.removeEventListener("dragover", claim);
      document.removeEventListener("drop", claim);
      return;
    }
    if (!(e.dataTransfer?.types.includes("Files") ?? false)) return;
    e.preventDefault();
    if (e.type === "dragover" && e.dataTransfer && !(e.target instanceof Node && main.contains(e.target))) e.dataTransfer.dropEffect = "none";
  };
  document.addEventListener("dragover", claim);
  document.addEventListener("drop", claim);
}
