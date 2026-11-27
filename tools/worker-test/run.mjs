// Runs every tools/worker-test/*.test.mjs like `node --test` (one process per file, in
// parallel), with a bound on the whole run: when it is exceeded, the files that have not
// finished are named and the run fails, instead of sitting idle until CI cancels the job.
// A single stuck test is named sooner by the per-test timer in harness.mjs.
import { readdirSync } from "node:fs";
import { availableParallelism } from "node:os";
import { dirname, join } from "node:path";
import { run } from "node:test";
import { spec, tap } from "node:test/reporters";
import { fileURLToPath } from "node:url";

// 95 s measured on 16 idle cores (about 2 min on CI, 2 cores). 10 min is 5x CI; a hang is
// normally caught much sooner by the per-test timer, so this only backstops what that cannot see.
const TOTAL_MS = 600_000;

const dir = dirname(fileURLToPath(import.meta.url));
const files = readdirSync(dir)
  .filter((f) => f.endsWith(".test.mjs"))
  .sort()
  .map((f) => join(dir, f));

const unfinished = new Set(files);
let failed = false;
const controller = new AbortController();
const stream = run({ files, concurrency: Math.max(1, availableParallelism() - 1), signal: controller.signal });
stream.on("test:summary", (d) => unfinished.delete(d.file));
stream.on("test:fail", () => (failed = true));
stream.compose(process.stdout.isTTY ? spec : tap).pipe(process.stdout);
stream.on("close", () => process.exit(failed ? 1 : 0));

setTimeout(() => {
  const names = [...unfinished].map((f) => `  ${f.slice(dir.length + 1)}`).join("\n");
  console.error(`\nworker-test: FAIL: run exceeded ${TOTAL_MS / 1000} s; files not yet reported finished (reports arrive in file order, so a later file may be done):\n${names}`);
  controller.abort();
  // scripts/worker-test's exit trap kills any wrangler group the aborted files left behind.
  setTimeout(() => process.exit(1), 2_000);
}, TOTAL_MS).unref();
