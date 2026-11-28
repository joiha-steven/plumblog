// A cron job without a subrequest test cannot be merged. `jobs::run` ends every job with
// `crate::subrequests::lap(<JOB>, ..)` (the count logged per job). This lists those names
// from jobs.rs and requires each in a worker-test that passes the job's logged runs to
// `assertWithinBudget(jobRuns(w, "<name>"), "<name>")` (subreq.mjs: counted <= budgeted <= 50).
// It reads text only: no build, runs in a blink. Reach: every `*_JOB` constant in
// crates/worker/src must be lapped or listed in COUNTED_INSIDE; a job with no `*_JOB`
// constant, or a test that calls the assertion without ever seeding the worst case, is not
// caught: review for those.
import assert from "node:assert/strict";
import { readdirSync, readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, "..", "..");
const jobsSrc = readFileSync(join(root, "crates/worker/src/jobs.rs"), "utf8").split("#[cfg(test)]")[0];

function rustFiles(dir) {
  return readdirSync(dir, { withFileTypes: true }).flatMap((e) => (e.isDirectory() ? rustFiles(join(dir, e.name)) : e.name.endsWith(".rs") ? [join(dir, e.name)] : []));
}
const allRust = rustFiles(join(root, "crates")).map((f) => readFileSync(f, "utf8")).join("\n");
const jobName = (konst) => {
  const m = allRust.match(new RegExp(`const ${konst}: &str = "([^"]+)"`));
  assert.ok(m, `no \`const ${konst}: &str\` found under crates/`);
  return m[1];
};

// Jobs whose calls are counted inside another job's lap, so they have no count of their own.
const COUNTED_INSIDE = {
  CLEANUP_JOB: "PRERENDER_JOB (pages::prerender::cron runs both against one budget)",
};

test("every job is lapped (or listed above), and every lapped job has a worker-test holding it to its budget", () => {
  const lapped = [...jobsSrc.matchAll(/subrequests::lap\(\s*([A-Z_]+)\s*,/g)].map((m) => m[1]);
  // Every job constant the Worker defines or jobs.rs names, so a job started inside another
  // job's function (cleanup, in the pre-render) cannot slip by.
  const workerSrc = rustFiles(join(root, "crates/worker/src")).map((f) => readFileSync(f, "utf8")).join("\n");
  const named = new Set([...(workerSrc.match(/const [A-Z][A-Z_]*_JOB\b/g) ?? []).map((c) => c.slice(6)), ...jobsSrc.match(/\b[A-Z][A-Z_]*_JOB\b/g)]);
  for (const konst of named) {
    assert.ok(lapped.includes(konst) || konst in COUNTED_INSIDE, `${konst} is a job but is never passed to subrequests::lap in jobs.rs (or listed in COUNTED_INSIDE)`);
  }
  for (const konst of Object.keys(COUNTED_INSIDE)) assert.ok(named.has(konst) && !lapped.includes(konst), `COUNTED_INSIDE lists ${konst}, which is gone or lapped now`);
  assert.ok(lapped.length >= 5, `expected the 5 jobs, found ${lapped.join(", ")}`);
  const tests = readdirSync(here)
    .filter((f) => f.endsWith(".test.mjs") && f !== "subreq-guard.test.mjs")
    .map((f) => readFileSync(join(here, f), "utf8"));
  for (const konst of lapped) {
    const name = jobName(konst);
    const call = `assertWithinBudget(jobRuns(w, "${name}"), "${name}")`;
    assert.ok(tests.some((src) => src.includes(call)), `job ${name} (${konst}) has no worker-test calling ${call}`);
  }
});
