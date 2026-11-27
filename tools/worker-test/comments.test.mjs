// Reader comments through the real Worker and local D1 (issue #17, phase 1): the form post,
// its refusals, moderation and the page cache. The test build's gate accepts one fixed token
// and counts per client; `x-plumblog-test-client: unconfigured` gets the production gate.
import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { startWorker, wrangler } from "./harness.mjs";

const TOKEN = "plumblog-test-token"; // crates/worker/src/contract.rs TEST_TOKEN
const LIMIT = 3; // TEST_LIMIT
const POST = "/first-post";

let worker;
before(async () => {
  worker = await startWorker();
  const res = await fetch(`${worker.url}/__seed`, { method: "POST" });
  assert.equal(res.status, 200, await res.text());
  // Site switch on; first-post open, slow-mornings off, why-small-tools a draft with its own
  // switch on. Raw SQL skips the store, so the content version is bumped by hand.
  sql(
    "INSERT INTO settings (key, value_json, updated_at) VALUES ('comments.enabled', 'true', 0); " +
      "UPDATE posts SET comments_enabled = 1 WHERE slug IN ('first-post', 'why-small-tools'); " +
      "UPDATE posts SET status = 'draft', published_at = NULL WHERE slug = 'why-small-tools'; " +
      "UPDATE site SET content_version = content_version + 1",
  );
});
after(async () => {
  await worker?.stop();
});

function sql(command) {
  const out = wrangler(["d1", "execute", "DB", "--local", "--persist-to", worker.state, "--json", "--command", command]);
  const start = out.indexOf("[");
  assert.ok(start >= 0, `no JSON in wrangler output:\n${out}`);
  return JSON.parse(out.slice(start)).at(-1).results ?? [];
}
const count = () => sql("SELECT count(*) AS n FROM comments")[0].n;
/** Everything a refused post must leave alone: comment rows, system log rows, content version. */
const snapshot = () =>
  sql(
    "SELECT (SELECT count(*) FROM comments) AS comments, (SELECT count(*) FROM system_log) AS log, " +
      "(SELECT content_version FROM site WHERE id = 1) AS version",
  )[0];
const bump = () => sql("UPDATE site SET content_version = content_version + 1");

/** Posts the form; returns the status and the Location path (origin stripped). */
async function send(fields, { client = "default", slug = POST, type, raw } = {}) {
  const body = raw ?? new URLSearchParams({ "cf-turnstile-response": TOKEN, ...fields }).toString();
  const res = await fetch(`${worker.url}${slug}/comments`, {
    method: "POST",
    redirect: "manual",
    headers: { "content-type": type ?? "application/x-www-form-urlencoded", "x-plumblog-test-client": client },
    body,
  });
  const loc = res.headers.get("location") ?? "";
  await res.arrayBuffer();
  return { status: res.status, to: loc.replace(worker.url, "") };
}
const pending = `${POST}?comment=pending#comments`;
const refused = (code) => `${POST}?comment=error&code=${code}#comments`;

/** A refused post: the given code, no row written. */
async function refusedWith(code, fields, opts) {
  const before = snapshot();
  const r = await send(fields, opts);
  console.log(`refused ${code}:`, r.status, r.to);
  assert.equal(r.status, 303);
  assert.equal(r.to, opts?.slug ? r.to : refused(code));
  assert.ok(r.to.endsWith(`?comment=error&code=${code}#comments`), r.to);
  assert.deepEqual(snapshot(), before, `${code}: no comment, no system log entry, same content version`);
}

const get = (path) => fetch(`${worker.url}${path}`);
const commentsOf = (html) => html.split('<section class="comments"')[1]?.split("</section>")[0] ?? "";

test("no-JS form markup is on an open post", async () => {
  const html = await (await get(POST)).text();
  const section = commentsOf(html);
  assert.match(section, /<form class="comment-form" action="\/first-post\/comments" method="post"/);
  assert.match(section, /name="name" required maxlength="60"/);
  assert.match(section, /name="body" required rows="5" maxlength="5000"/);
  assert.match(section, /class="hint nojs">Sending a comment needs JavaScript/);
  assert.match(section, /No comments yet\./);
});

test("a comment is stored pending, hidden, and shown once approved (cache refreshes)", async () => {
  const name = "Reader One";
  const text = "Hello from **the** happy path";
  const r = await send({ name, body: text }, { client: "happy" });
  console.log("happy path:", r.status, r.to);
  assert.deepEqual(r, { status: 303, to: pending });
  const [row] = sql("SELECT id, post_id, parent_id, author_name, body, status FROM comments ORDER BY id DESC LIMIT 1");
  console.log("stored row:", JSON.stringify(row));
  assert.equal(row.status, "pending");
  assert.equal(row.parent_id, null);
  assert.equal(row.author_name, name);
  assert.equal(row.body, text);
  const [{ body_html }] = sql(`SELECT body_html FROM comments WHERE id = ${row.id}`);
  console.log("stored body_html:", JSON.stringify(body_html));
  assert.equal(body_html, "<p>Hello from <strong>the</strong> happy path</p>\n", "HTML rendered once, at submit");
  const log = sql("SELECT kind, actor_id, detail FROM system_log WHERE kind = 'comment.created'");
  console.log("system_log:", JSON.stringify(log));
  assert.equal(log.length, 1);
  assert.equal(log[0].detail, `\npost\t${row.post_id}\ncomment\t${row.id}`);

  const first = await get(POST);
  assert.ok(!(await first.text()).includes("happy path"), "pending is not shown");
  assert.equal((await get(POST)).headers.get("x-plumblog-cache"), "hit", "page cached while pending");

  const mod = await fetch(`${worker.url}/__moderate?id=${row.id}&action=approved&role=writer&user=1`, { method: "POST" });
  assert.equal(mod.status, 200, await mod.text());
  const after = await get(POST);
  console.log("after approve:", after.headers.get("x-plumblog-cache"), after.headers.get("etag"));
  assert.equal(after.headers.get("x-plumblog-cache"), "miss", "new content version, new key");
  assert.notEqual(after.headers.get("etag"), first.headers.get("etag"));
  const section = commentsOf(await after.text());
  assert.match(section, new RegExp(`<li class="comment" id="c${row.id}">`));
  assert.match(section, /<b dir="auto">Reader One<\/b>/);
  assert.match(section, /<p>Hello from <strong>the<\/strong> happy path<\/p>/);
  const modLog = sql("SELECT actor_id, detail FROM system_log WHERE kind = 'comment.moderated'");
  console.log("moderation log:", JSON.stringify(modLog));
  assert.deepEqual(modLog, [{ actor_id: 1, detail: `\ncomment\t${row.id}\npost\t${row.post_id}\nstatus\tapproved` }]);
});

test("moderation needs a role", async () => {
  const [row] = sql("SELECT id FROM comments LIMIT 1");
  const res = await fetch(`${worker.url}/__moderate?id=${row.id}&action=spam`, { method: "POST" });
  assert.equal(res.status, 403);
  assert.equal(await res.text(), "auth.unauthenticated");
});

test("?comment= pages are the cached page: same bytes, same cache headers", async () => {
  await get(POST);
  const plain = await get(POST);
  assert.equal(plain.headers.get("x-plumblog-cache"), "hit");
  const want = await plain.text();
  const heads = (r) => ["cache-control", "etag", "content-type", "x-plumblog-cache"].map((h) => r.headers.get(h));
  for (const q of ["comment=pending", "comment=error&code=x", "comment=error&code=%3Cscript%3Ealert(1)%3C%2Fscript%3E"]) {
    const res = await get(`${POST}?${q}`);
    assert.equal(res.status, 200);
    assert.deepEqual(heads(res), heads(plain), q);
    const html = await res.text();
    assert.equal(html, want, `${q}: same bytes`);
    assert.ok(!html.includes("<script>alert") && !html.includes("&lt;script"), q);
  }
  console.log("?comment= headers:", JSON.stringify(heads(plain)));
  // The theme JS reads these lines; the page itself carries no notice.
  assert.match(commentsOf(want), /<template id="comment-notices">/);
  assert.match(commentsOf(want), /<p data-code="comments.rate_limited">Too many comments/);
  assert.ok(!/class="notice/.test(commentsOf(want)));
});

test("a post lists the newest 100 approved comments, oldest first, with a note", async () => {
  // slow-mornings: closed to new comments, but approved ones still show.
  sql(
    "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 101) " +
      "INSERT INTO comments (post_id, author_name, body, body_html, status, created_at) " +
      "SELECT (SELECT id FROM posts WHERE slug = 'slow-mornings'), 'Bulk ' || i, 'bulk ' || i, " +
      "'<p>bulk ' || i || '</p>', 'approved', 1700000000000 + i FROM n; " +
      "UPDATE site SET content_version = content_version + 1",
  );
  const section = commentsOf(await (await get("/slow-mornings")).text());
  const shown = [...section.matchAll(/<p>bulk (\d+)<\/p>/g)].map((m) => Number(m[1]));
  console.log("bulk comments shown:", shown.length, "first", shown[0], "last", shown.at(-1));
  assert.equal(shown.length, 100);
  assert.deepEqual(shown, Array.from({ length: 100 }, (_, i) => i + 2), "newest 100, oldest first");
  assert.match(section, /<span class="count">100<\/span>/);
  assert.match(section, /Only the newest 100 comments are shown\./);
  assert.ok(!commentsOf(await (await get(POST)).text()).includes("Only the newest"), "no note when all fit");
});

test("limits are refused before anything is written", async () => {
  const opts = { client: "limits" };
  await refusedWith("comments.body_invalid", { name: "A", body: "x".repeat(5001) }, opts);
  await refusedWith("comments.body_invalid", { name: "A", body: "   " }, opts);
  await refusedWith("comments.name_invalid", { name: "n".repeat(61), body: "hi" }, opts);
  await refusedWith("comments.name_invalid", { name: "", body: "hi" }, opts);
  await refusedWith("comments.name_invalid", { name: "\u200b\u202e", body: "hi" }, opts);
  await refusedWith("comments.too_many_links", { name: "A", body: "https://a.example/ ".repeat(31) }, opts);
  await refusedWith("comments.form_invalid", {}, { ...opts, raw: `name=A&body=${"x".repeat(64 * 1024)}` });
  await refusedWith("comments.form_invalid", {}, { ...opts, type: "application/json", raw: '{"name":"A","body":"hi"}' });
  await refusedWith("comments.form_invalid", {}, { ...opts, type: "multipart/form-data; boundary=x", raw: "--x--" });
});

test("hostile Markdown is stored raw and rendered safe", async () => {
  // Inline, so the paragraph survives; a line that starts with <script> is an HTML block and
  // is dropped whole, like in posts.
  const body =
    "Hi <script>alert(1)</script> [a](javascript:alert(1)) [b](jav&#x09;ascript:alert(2)) " +
    "[c](JaVaScRiPt:alert(3)) ![x](https://i.example/x.png\" onerror=\"alert(4)) <img src=x onerror=alert(5)> " +
    "[ok](https://ok.example/)";
  const r = await send({ name: "<b>Mallory</b>", body }, { client: "hostile" });
  assert.deepEqual(r, { status: 303, to: pending });
  const [row] = sql("SELECT id, author_name, body FROM comments ORDER BY id DESC LIMIT 1");
  assert.equal(row.body, body, "stored text is the raw Markdown");
  const mod = await fetch(`${worker.url}/__moderate?id=${row.id}&action=approved&role=owner&user=1`, { method: "POST" });
  assert.equal(mod.status, 200);
  const section = commentsOf(await (await get(POST)).text());
  const item = section.split(`id="c${row.id}">`)[1].split("</li>")[0];
  console.log("rendered hostile comment:", item);
  // `onerror=` may survive as visible, escaped text; never as an attribute inside a tag.
  assert.ok(!/<script|<img|<[^>]*\son[a-z]+=|javascript:/i.test(item), item);
  assert.match(item, /<b dir="auto">&lt;b&gt;Mallory&lt;\/b&gt;<\/b>/);
  assert.match(item, /<a href="https:\/\/ok\.example\/" rel="nofollow ugc noopener">ok<\/a>/);
  assert.equal((item.match(/<a /g) ?? []).length, (item.match(/rel="nofollow ugc noopener"/g) ?? []).length);
});

test("a failed spam check writes nothing", async () => {
  await refusedWith("comments.check_failed", { name: "A", body: "hi", "cf-turnstile-response": "wrong" }, { client: "verify" });
  await refusedWith("comments.check_failed", {}, { client: "verify", raw: "name=A&body=hi" });
});

test("the rate limit refuses after the limit, with no extra rows", async () => {
  const before = count();
  for (let i = 0; i < LIMIT; i++) {
    assert.deepEqual(await send({ name: "R", body: `rate ${i}` }, { client: "rate" }), { status: 303, to: pending });
  }
  await refusedWith("comments.rate_limited", { name: "R", body: "one too many" }, { client: "rate" });
  assert.equal(count(), before + LIMIT);
});

test("closed comments are refused: post off, draft, site off", async () => {
  const opts = (slug) => ({ client: "closed", slug });
  const closed = async (slug) => {
    const before = snapshot();
    const r = await send({ name: "A", body: "hi" }, opts(slug));
    console.log(`closed ${slug}:`, r.status, r.to);
    assert.deepEqual(r, { status: 303, to: `${slug}?comment=error&code=comments.closed#comments` });
    assert.deepEqual(snapshot(), before, `${slug}: nothing written`);
  };
  await closed("/slow-mornings");
  assert.match(commentsOf(await (await get("/slow-mornings")).text()), /Comments are closed\./);
  await closed("/why-small-tools");
  const before = snapshot();
  assert.equal((await send({ name: "A", body: "hi" }, opts("/no-such-post"))).status, 404);
  assert.deepEqual(snapshot(), before);

  sql("UPDATE settings SET value_json = 'false' WHERE key = 'comments.enabled'");
  bump();
  try {
    await closed(POST);
    assert.ok(!(await (await get(POST)).text()).includes('id="comments"'), "site off: no section at all");
  } finally {
    sql("UPDATE settings SET value_json = 'true' WHERE key = 'comments.enabled'");
    bump();
  }
});

test("the production gate refuses everything with comments.unavailable", async () => {
  await refusedWith("comments.unavailable", { name: "A", body: "hi" }, { client: "unconfigured" });
});

test("a full-length Vietnamese comment fits the form cap", async () => {
  // Worst case: every character is 3 bytes of UTF-8, 9 bytes percent-encoded (about 45 KB).
  const body = "\u1edd".repeat(4999) + "x";
  assert.equal([...body].length, 5000);
  const encoded = new URLSearchParams({ "cf-turnstile-response": TOKEN, name: "Vi\u1ec7t", body }).toString();
  console.log("Vietnamese form bytes:", encoded.length);
  assert.ok(encoded.length > 40 * 1024 && encoded.length <= 64 * 1024, `${encoded.length}`);
  assert.deepEqual(await send({}, { client: "vietnamese", raw: encoded }), { status: 303, to: pending });
  const [row] = sql("SELECT body FROM comments ORDER BY id DESC LIMIT 1");
  assert.equal(row.body, body);
});

test("CRLF line breaks count once and are stored as LF", async () => {
  // 4,998 characters plus two CRLF breaks: 5,002 raw, 5,000 once normalised.
  const body = `${"a".repeat(2499)}\r\n${"b".repeat(2498)}\rc`;
  assert.equal(body.length, 5001);
  const crlf = `${"a".repeat(2499)}\r\n${"b".repeat(2498)}\r\nc`;
  assert.equal(crlf.length, 5002);
  for (const text of [body, crlf]) {
    assert.deepEqual(await send({ name: "Line", body: text }, { client: "crlf" }), { status: 303, to: pending });
    const [row] = sql("SELECT body, length(body) AS n FROM comments ORDER BY id DESC LIMIT 1");
    assert.ok(!row.body.includes("\r"), "no CR stored");
    assert.equal(row.n, 5000);
  }
});

test("a row from before body_html is rendered from its Markdown", async () => {
  sql(
    "INSERT INTO comments (post_id, author_name, body, body_html, status, created_at) " +
      "SELECT id, 'Old', '*null html*', NULL, 'approved', 1 FROM posts WHERE slug = 'notes-from-da-lat'; " +
      "UPDATE site SET content_version = content_version + 1",
  );
  const section = commentsOf(await (await get("/notes-from-da-lat")).text());
  assert.match(section, /<div class="comment-body" dir="auto"><p><em>null html<\/em><\/p>/);
});
