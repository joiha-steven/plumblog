---
name: admin-lighthouse
description: Measure the admin's Lighthouse performance (mobile and desktop) on the real build with a signed-in session. Use for any admin load-speed issue or before claiming "Lighthouse 100" for the admin.
---

# Admin Lighthouse on the real build

Measured this way in #84, #91 and #85. Run everything from the repo (or worktree) root.
Never use `scripts/worker-test --serve` for this: it serves the MOCK admin build.

## 1. Build and serve the real Worker

```sh
scripts/admin-build                              # real client -> target/admin (+ target/admin.preload)
wb=$(scripts/worker-build); export PLUMBLOG_ADMIN_PRELOAD=$PWD/target/admin.preload
(cd crates/worker && "$wb" --release --no-opt --out-dir ../../target/worker-dev \
  -- --features contract-tests --target-dir $PWD/../../target/contract-tests)
scripts/assemble-assets target/worker-dev/assets target/admin   # real admin, not admin-dev
tools/node_modules/.bin/wrangler d1 migrations apply DB --local --config tools/wrangler.dev.toml --persist-to target/worker-dev/state
nohup tools/node_modules/.bin/wrangler dev --config tools/wrangler.dev.toml \
  --persist-to target/worker-dev/state --ip 127.0.0.1 --port 8790 > /tmp/wr.log 2>&1 &
timeout 90 bash -c 'until curl -s localhost:8790/healthz >/dev/null; do sleep 2; done'
```

Forgetting `PLUMBLOG_ADMIN_PRELOAD` silently drops the shell's modulepreload links (score 99).
After any Rust or admin change, rebuild all four steps and restart wrangler.

## 2. Owner, session cookie, about 26 posts

Setup needs a setup code and a client-stretched password, so instead insert the owner and a
session straight into the local D1, then create posts through the API with that cookie.
The cookie is `__Host-plumblog_session=<43 chars>`; D1 stores its SHA-256.

```js
// seed.mjs: node seed.mjs   (cwd = repo root)
import { createHash } from "node:crypto";
import { execFileSync } from "node:child_process";
const tok = "A".repeat(43), T = Date.now();
const h = createHash("sha256").update(tok).digest("hex");
const sql = `INSERT OR REPLACE INTO settings (key,value_json,updated_at) VALUES ('site_id','"perf-site"',${T});
INSERT OR REPLACE INTO users (id,login,slug,display_name,role,password_hash,disabled_at,created_at,updated_at) VALUES (1,'Owner','owner','Owner','owner','p1$1000$AAAA$AAAA',NULL,${T},${T});
INSERT OR REPLACE INTO sessions (token_hash,user_id,created_at,expires_at) VALUES ('${h}',1,${T},${T + 29 * 864e5});`;
execFileSync("tools/node_modules/.bin/wrangler", ["d1","execute","DB","--local","--config","tools/wrangler.dev.toml","--persist-to","target/worker-dev/state","--command",sql.replace(/\n/g," ")], {stdio:"inherit"});
const base = "http://127.0.0.1:8790", hd = { cookie: `__Host-plumblog_session=${tok}`, "content-type": "application/json", origin: base };
const words = "lorem ipsum dolor sit amet consectetur adipiscing elit sed do eiusmod tempor".split(" ");
for (let i = 1; i <= 26; i++) {
  const markdown = Array.from({ length: 60 }, (_, j) => words[(i + j) % words.length]).join(" ") + ".";
  const r = await fetch(base + "/api/v1/posts", { method: "POST", headers: hd,
    body: JSON.stringify({ lang: "en", slug: `perf-post-${i}`, title: `Perf post number ${i}`, markdown, kind: i % 5 === 0 ? "page" : "post" }) });
  if (!r.ok) { console.log(i, r.status, await r.text()); break; }
  if (i % 3 === 0) { const p = await r.json();   // every third one published, the rest drafts
    await fetch(`${base}/api/v1/posts/${p.id}`, { method: "PATCH", headers: hd, body: JSON.stringify({ expected_version: p.version, status: "published" }) }); }
}
```

Check: `curl -H "cookie: __Host-plumblog_session=AAAA...(43 A)" "localhost:8790/api/v1/posts?limit=3"`.

## 3. Run Lighthouse (offline npx cache)

Find the cached binary: `ls ~/.npm/_npx/*/node_modules/.bin/lighthouse` (use the first). Per page, 3 runs:

```sh
LH=<that path>; C='{"Cookie":"__Host-plumblog_session=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}'
for i in 1 2 3; do
  $LH "http://127.0.0.1:8790/admin/" --output=json --output-path=/tmp/lh-$i.json --quiet \
    --only-categories=performance --chrome-flags="--headless=new --no-sandbox" --extra-headers="$C"   # add --preset=desktop for desktop
  node -e 'const r=require(process.argv[1]),a=r.audits;console.log(r.categories.performance.score*100,"LCP",a["largest-contentful-paint"].displayValue,"FCP",a["first-contentful-paint"].displayValue,"TBT",a["total-blocking-time"].displayValue,"CLS",a["cumulative-layout-shift"].displayValue)' /tmp/lh-$i.json
done
```

Pages: `/admin/`, `/admin/pages`, `/admin/?status=draft`, an editor page `/admin/posts/1`;
desktop on `/admin/`. Roughly 25 s per run, so run the batch in the background and wait once.
zsh does not word-split `set -- $x`; use bash for loops over "name path" pairs.

## 4. Report

A before/after table per page: score of each of the 3 runs, LCP, FCP, TBT, CLS (CLS must be 0).
Take "before" on main with the same data. To see what gates LCP, read `audits["network-requests"]`
(start/end per request) and `audits["largest-contentful-paint-element"]`; try one change at a
time: here removing 60 KB of unused font preloads moved LCP 1.8 s to 1.2 s, more than anything else.

## Traps

- Only the performance category is the goal. SEO shows 54 by design (`noindex` on the admin).
- localhost is HTTP/1.1 (6 connections per origin); production is HTTP/2. Scores can differ there.
- Editor FCP is noisy (1.1 s or 2.2 s between identical runs); judge it over 3+ runs.
- Run only one `scripts/worker-test` or `scripts/check` at a time (shared ports and target dirs).
- Shut the server down afterwards: `kill $(pgrep -f "wrangler dev --config tools/wrangler.dev.toml --persist-to target/worker-dev")`.
  Do not `pkill wrangler`: other sessions may run theirs.
- Rebuild the Worker after changing `PLUMBLOG_ADMIN_PRELOAD` or the admin: the list is embedded at compile time.
