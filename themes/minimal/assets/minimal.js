// minimal theme: light/dark toggle, search panel, comment form helpers, copy-code buttons and sidenotes. Every page works fully without this file.
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

  // Comments: Turnstile loads only when the reader starts writing, so reading costs no third-party
  // bytes. The draft is kept in sessionStorage so a failed send never loses the text.
  const form = d.querySelector(".comment-form");
  if (form) {
    const body = form.querySelector("textarea"), key = "draft:" + form.action;
    const store = (fn) => { try { return fn(sessionStorage); } catch (e) { return null; } };
    if (d.querySelector(".notice.is-pending")) store((s) => s.removeItem(key));
    else if (body && !body.value) body.value = store((s) => s.getItem(key)) || "";
    if (body) body.addEventListener("input", () => store((s) => s.setItem(key, body.value)));
    const sitekey = form.dataset.turnstile;
    let started = false;
    if (sitekey) form.addEventListener("focusin", () => {
      if (started) return;
      started = true;
      const fail = (err) => {
        console.error("comments: Turnstile failed to load", err);
        const p = d.createElement("p");
        p.className = "notice is-error";
        p.setAttribute("role", "alert");
        p.textContent = form.dataset.checkFailed || "";
        form.prepend(p);
      };
      window.plumTurnstile = () => {
        try { window.turnstile.render(form.querySelector(".turnstile"), { sitekey, size: "flexible" }); } catch (e) { fail(e); }
      };
      const s = d.createElement("script");
      s.src = "https://challenges.cloudflare.com/turnstile/v0/api.js?render=explicit&onload=plumTurnstile";
      s.async = true;
      s.onerror = fail;
      d.head.append(s);
    });
  }

  // Search panel. The magnifier is a link to the search page; with JS it opens a field under the
  // header instead, and results are read from that same page, so core needs no second endpoint.
  const opener = d.querySelector(".search-open"), panel = d.getElementById("search-panel");
  if (opener && panel) {
    const form = panel.querySelector("form"), input = form.q, live = panel.querySelector(".search-live");
    let timer = 0, ctrl = null;
    const open = () => {
      panel.hidden = false;
      opener.setAttribute("aria-expanded", "true");
      input.focus();
      input.select();
    };
    const close = (back) => {
      if (panel.hidden) return;
      panel.hidden = true;
      opener.setAttribute("aria-expanded", "false");
      if (back) opener.focus();
    };
    const el = (tag, cls, text) => {
      const n = d.createElement(tag);
      if (cls) n.className = cls;
      if (text) n.textContent = text;
      return n;
    };
    const search = async () => {
      const q = input.value.trim();
      if (ctrl) ctrl.abort();
      if (q.length < 2) { live.replaceChildren(); return; }
      ctrl = new AbortController();
      const url = new URL(form.action, location.href);
      url.searchParams.set("q", q);
      try {
        const res = await fetch(url, { signal: ctrl.signal });
        if (!res.ok) throw new Error("HTTP " + res.status);
        const page = new DOMParser().parseFromString(await res.text(), "text/html");
        const list = el("ol", "live-list");
        for (const a of [...page.querySelectorAll(".posts .entry")].slice(0, 6)) {
          const item = el("a");
          item.href = new URL(a.getAttribute("href"), url).href;
          item.append(el("span", "live-title", a.querySelector(".entry-title").textContent));
          const time = a.querySelector("time");
          if (time) item.append(el("span", "meta", time.textContent));
          const li = el("li");
          li.append(item);
          list.append(li);
        }
        // The count line ("3 posts for ...", or "nothing found") is the search page's own text.
        const count = page.querySelector(".search-count");
        const foot = el("p", list.children.length ? "live-foot" : "live-note", count ? count.textContent.trim() : "");
        if (list.children.length) {
          const all = el("a", "", panel.dataset.all);
          all.href = url.href;
          foot.append(all);
        }
        live.replaceChildren(...(list.children.length ? [list] : []), foot);
      } catch (err) {
        if (err.name === "AbortError") return;
        console.error("search: live results failed", err);
        live.replaceChildren(el("p", "live-note", panel.dataset.error));
      }
    };
    opener.addEventListener("click", (e) => {
      e.preventDefault();
      panel.hidden ? open() : close(true);
    });
    panel.querySelector(".search-close").addEventListener("click", () => close(true));
    input.addEventListener("input", () => { clearTimeout(timer); timer = setTimeout(search, 180); });
    d.addEventListener("keydown", (e) => {
      const typing = /^(INPUT|TEXTAREA|SELECT)$/.test(e.target.tagName) || e.target.isContentEditable;
      if (e.key === "/" && !typing && panel.hidden) { e.preventDefault(); open(); return; }
      if (panel.hidden) return;
      if (e.key === "Escape") { close(true); return; }
      // Arrow keys walk from the field through the results.
      if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
      const items = [input, ...live.querySelectorAll(".live-list a")];
      const i = items.indexOf(d.activeElement);
      if (i < 0) return;
      e.preventDefault();
      items[Math.max(0, Math.min(items.length - 1, i + (e.key === "ArrowDown" ? 1 : -1)))].focus();
    });
    d.addEventListener("click", (e) => {
      if (!panel.contains(e.target) && !opener.contains(e.target)) close(false);
    });
  }

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
  const calm = matchMedia("(prefers-reduced-motion: reduce)");
  // One sidenote per footnote, placed at its first reference. A footnote cited again gets a
  // button that opens and scrolls to that same note, so ids stay unique (issue #28).
  const notes = new Map(); // footnote id -> { note, buttons }
  for (const a of prose.querySelectorAll(".fnref > a[href^='#']")) {
    const li = d.getElementById(decodeURIComponent(a.hash.slice(1)));
    if (!li) continue;
    const n = a.textContent.trim();
    let entry = notes.get(li.id);
    if (!entry) {
      const note = d.createElement("span");
      note.className = "sidenote";
      note.id = "sn-" + li.id;
      note.tabIndex = -1;
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
      entry = { note, buttons: [] };
      notes.set(li.id, entry);
    }
    const { note, buttons } = entry;
    const again = buttons.length > 0;
    const sync = () => buttons.forEach((b) => b.setAttribute("aria-expanded", note.classList.contains("open")));
    const btn = d.createElement("button");
    btn.type = "button";
    btn.textContent = n;
    btn.setAttribute("aria-expanded", "false");
    btn.setAttribute("aria-controls", note.id);
    btn.setAttribute("aria-label", label.replace("{n}", n));
    if (again) btn.className = "again";
    btn.addEventListener("click", again ? () => {
      note.classList.add("open");
      sync();
      note.scrollIntoView({ block: "nearest", behavior: calm.matches ? "auto" : "smooth" });
      note.focus({ preventScroll: true });
    } : () => { note.classList.toggle("open"); sync(); });
    buttons.push(btn);
    a.replaceWith(btn);
    if (!again) btn.parentNode.after(note);
  }
  prose.classList.add("has-sidenotes");
})();
