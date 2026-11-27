# AGENTS.md

Entry point for any AI agent working on plumblog. Read this, then `docs/ARCHITECTURE.md`
for the section you are touching. The project is built and maintained entirely by AI agents;
the owner does not read code, so reports must be evidence (output, screenshots, scores), not diffs.

## Commands

```sh
scripts/check                      # the only check: fmt, clippy, tests, wasm budget, repo rules
cargo test -p plumblog-core        # fast loop on the shared crate
cargo run -p plumblog-cli -- guide # the `blog` CLI
```

`scripts/check` runs on every commit (pre-commit hook, installed by the script itself) and in CI.
Never bypass it (`--no-verify`), never weaken a rule to make it pass. If a rule is wrong,
change it in the same commit and say why in the commit message.

## Hard rules

- English only in the repo: code, comments, paths, file names, guides, commits. Other languages only in `locales/`.
- Comments explain why, never what.
- Dependency versions live only in the workspace `Cargo.toml`, each with a `# why` comment. Adding one needs a reason that beats writing the code.
- Markdown files are limited to the list in `docs/ARCHITECTURE.md` -> Documentation. Do not create notes, plans or reports as files; put decisions in `docs/ARCHITECTURE.md` (and its Decision log).
- Anything that can be generated from code (CLI reference, API reference, theme variables) is generated, not written.
- Every Worker request must fit the free plan: 10 ms CPU. Heavy work goes to the browser or the CLI.
- Raw HTML in Markdown is stripped. No per-view database writes.
- Keep budgets (ARCHITECTURE -> Budgets). Exceeding one is a failure, not a warning.
- Errors are never silent: the user sees a plain message, the log has enough for the next agent.
- Small, scoped changes. Do not rename or refactor outside the task.
- Never commit secrets. `.env`, `.dev.vars`, `site.local.toml` and generated `wrangler.toml` are ignored.

## Where things go

| You are adding | Put it in |
|---|---|
| Markdown, models, validation, i18n logic | `crates/core` (no I/O, must also build for the browser) |
| HTTP routes, D1/R2 access, MCP | `crates/worker` |
| A `blog` command | `crates/cli` |
| Instructions for an AI operating a blog | `guides/<topic>.md` (first line `# Title`, then a one-sentence summary) |
| UI text | `locales/<lang>.toml` (English is the source) |
| Schema change | new numbered file in `migrations/`, forward-only |
| A design decision | `docs/ARCHITECTURE.md` |

## Guides

Guides are product, not documentation: they are compiled into `blog guide` and served over MCP.
Prefer writing a guide over building a feature when an AI agent can do the job with existing
commands. Every `blog ...` line inside a guide's code block is checked against the real CLI by a test.

## Done means

- `scripts/check` green.
- Bug fix: a test that failed before and passes after.
- UI change: screenshots at 375, 768 and 1440 px, light and dark; Lighthouse 100 on the reader site.
- Feature: steps "do this, see that", including empty, offline and no-permission paths.
