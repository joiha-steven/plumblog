// The minimal theme's comment notice (issue #17): the page is the cached one, so after the
// form redirects back with ?comment=..., minimal.js shows a line from the template's
// whitelist as text, then drops the parameters from the URL. Runs in happy-dom.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import { Window } from "happy-dom";

const SRC = readFileSync(new URL("../../themes/minimal/assets/minimal.js", import.meta.url), "utf8");

// Mirrors themes/minimal/templates/post.html (English locale).
const SECTION = `<section class="comments" id="comments">
  <template id="comment-notices">
    <p data-code="pending">Thanks. Pending.</p>
    <p data-code="error">Generic error.</p>
    <p data-code="comments.too_many_links">Too many links.</p>
    <p data-code="comments.rate_limited">Too many comments.</p>
  </template>
  <form class="comment-form" action="https://blog.test/post/comments" method="post" data-turnstile="">
    <textarea name="body"></textarea>
  </form>
</section>`;

function page(search) {
  const window = new Window({ url: `https://blog.test/post${search}#comments` });
  window.sessionStorage.setItem("draft:https://blog.test/post/comments", "my draft");
  window.document.body.innerHTML = SECTION;
  new Function("document", "matchMedia", "localStorage", "sessionStorage", "navigator", "getSelection", "setTimeout", "location", "history", "DOMParser", "URL", "console", SRC)(
    window.document, (q) => window.matchMedia(q), window.localStorage, window.sessionStorage, window.navigator,
    () => window.getSelection(), window.setTimeout.bind(window), window.location, window.history, window.DOMParser, window.URL, console,
  );
  const notices = [...window.document.querySelectorAll("#comments > .notice")];
  return { window, notices, text: notices.map((n) => n.textContent) };
}

test("pending: the success line, before the form; draft cleared; URL cleaned", () => {
  const { window, notices, text } = page("?comment=pending");
  assert.deepEqual(text, ["Thanks. Pending."]);
  assert.equal(notices[0].className, "notice is-pending");
  assert.equal(notices[0].getAttribute("role"), "status");
  assert.equal(notices[0].nextElementSibling.className, "comment-form");
  assert.equal(window.location.href, "https://blog.test/post#comments");
  assert.equal(window.sessionStorage.getItem("draft:https://blog.test/post/comments"), null);
});

test("a known error code shows its line and keeps the draft", () => {
  const { window, notices, text } = page("?comment=error&code=comments.too_many_links");
  assert.deepEqual(text, ["Too many links."]);
  assert.equal(notices[0].className, "notice is-error");
  assert.equal(window.document.querySelector("textarea").value, "my draft");
  assert.equal(window.location.href, "https://blog.test/post#comments");
});

test("unknown, crafted or HTML codes get the generic error, as text only", () => {
  for (const code of ["nope", "pending", "comments.nope", "<img src=x onerror=alert(1)>", ""]) {
    const { window, text } = page(`?comment=error&code=${encodeURIComponent(code)}`);
    assert.deepEqual(text, ["Generic error."], code);
    assert.equal(window.document.querySelector("#comments img"), null, code);
  }
});

test("other parameters stay; an unknown result shows nothing", () => {
  const { window, text } = page("?x=1&comment=%3Cb%3Ex&code=comments.rate_limited");
  assert.deepEqual(text, []);
  assert.equal(window.location.href, "https://blog.test/post?x=1#comments");
});

test("no comment parameter: no notice, URL untouched", () => {
  const { window, text } = page("?x=1");
  assert.deepEqual(text, []);
  assert.equal(window.location.href, "https://blog.test/post?x=1#comments");
});
