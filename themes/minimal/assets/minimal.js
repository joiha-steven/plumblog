// minimal theme: light/dark toggle, copy-code buttons and sidenotes. Every page works fully without this file.
(() => {
  const d = document, root = d.documentElement;

  // Theme toggle. The choice is applied before paint by the inline script in base.html.
  const toggle = d.querySelector(".toggle");
  if (toggle) toggle.addEventListener("click", () => {
    const now = root.dataset.theme || (matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light");
    const next = now === "dark" ? "light" : "dark";
    root.dataset.theme = next;
    try { localStorage.setItem("theme", next); } catch (e) { /* private mode: choice lasts this page only */ }
  });

  const prose = d.querySelector(".prose");
  if (!prose) return;

  // Copy button on code blocks. If the clipboard is refused, select the code so the reader can
  // copy it by hand instead of nothing happening.
  for (const box of prose.querySelectorAll(".code")) {
    const pre = box.querySelector("pre");
    if (!pre) continue;
    const btn = d.createElement("button");
    btn.type = "button";
    btn.className = "copy";
    btn.textContent = prose.dataset.copy || "";
    btn.addEventListener("click", () => {
      navigator.clipboard.writeText(pre.innerText).then(() => {
        btn.textContent = prose.dataset.copied || "";
        setTimeout(() => (btn.textContent = prose.dataset.copy || ""), 1500);
      }, (err) => {
        console.warn("copy failed, code selected instead:", err);
        getSelection().selectAllChildren(pre);
      });
    });
    box.append(btn);
  }

  // Sidenotes: copy each footnote next to its reference. CSS shows it in the margin on wide
  // screens and as a toggle under the sentence on narrow ones; the bottom list stays for print.
  // Owner option "sidenotes" off: keep the classic notes at the bottom.
  if (prose.dataset.sidenotes === "off" || !prose.querySelector(".footnotes")) return;
  const label = prose.dataset.noteLabel || "{n}"; // text comes from the theme locale
  for (const a of prose.querySelectorAll(".fnref > a[href^='#']")) {
    const li = d.getElementById(decodeURIComponent(a.hash.slice(1)));
    if (!li) continue;
    const n = a.textContent.trim();
    const note = d.createElement("span");
    note.className = "sidenote";
    note.id = "sn-" + li.id;
    note.setAttribute("role", "note");
    const num = d.createElement("span");
    num.className = "sidenote-n";
    num.textContent = n;
    note.append(num);
    const body = li.cloneNode(true);
    body.querySelectorAll(".fnback, [id]").forEach((el) => (el.matches(".fnback") ? el.remove() : el.removeAttribute("id")));
    [...body.childNodes].forEach((c, i) => {
      if (c.nodeName === "P") { if (i && note.childNodes.length > 1) note.append(d.createElement("br")); note.append(...c.childNodes); }
      else note.append(c);
    });
    const btn = d.createElement("button");
    btn.type = "button";
    btn.textContent = n;
    btn.setAttribute("aria-expanded", "false");
    btn.setAttribute("aria-controls", note.id);
    btn.setAttribute("aria-label", label.replace("{n}", n));
    btn.addEventListener("click", () => btn.setAttribute("aria-expanded", note.classList.toggle("open")));
    a.replaceWith(btn);
    btn.parentNode.after(note);
  }
  prose.classList.add("has-sidenotes");
})();
