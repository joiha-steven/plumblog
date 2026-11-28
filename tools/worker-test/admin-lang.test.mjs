// Sign-in pages speak the site's stored language before the browser's: nobody can switch
// language before an account exists. Other admin pages still follow the browser.
import assert from "node:assert/strict";
import { after, before, test } from "node:test";
import { startWorker } from "./harness.mjs";

let worker;
before(async () => {
  worker = await startWorker({
    seedSql: `INSERT INTO settings (key, value_json, updated_at) VALUES ('site_lang', '"vi"', 0);`,
  });
});
after(async () => {
  await worker?.stop();
});

const get = async (path, headers) => (await fetch(worker.url + path, { headers })).text();

test("auth pages use the stored site language over Accept-Language", async () => {
  for (const path of ["/admin/setup", "/admin/signin"]) {
    const html = await get(path, { "accept-language": "en-US" });
    assert.match(html, /<html lang="vi"/, path);
    assert.match(html, /href="\/admin\/i18n\/vi.js"/, path);
  }
});

test("the language cookie still wins on auth pages", async () => {
  const html = await get("/admin/setup", { "accept-language": "vi", cookie: "pb_lang=en" });
  assert.match(html, /<html lang="en"/);
});

test("other admin pages keep cookie, then Accept-Language, then site language", async () => {
  assert.match(await get("/admin/", { "accept-language": "en-US" }), /<html lang="en"/);
  assert.match(await get("/admin/", {}), /<html lang="vi"/);
});
