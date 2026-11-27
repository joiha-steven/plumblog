# AGENTS.md

How to work on plumblog. What to build, and why, is in `docs/ARCHITECTURE.md`; read the section
you are touching. The owner does not read code: report with evidence (output, screenshots, scores).

## Commands

```sh
scripts/check                      # the only check; also runs on commit and in CI
cargo test -p plumblog-<crate>     # fast loop
scripts/worker-test                # real Worker over HTTP (wrangler dev, local D1/R2); tests in tools/worker-test/
scripts/perf                       # real CPU budgets as a table; never gates (--strict does, on a quiet machine)
cargo run -p plumblog-cli -- guide # the `blog` CLI
```

Never bypass or weaken `scripts/check`. If a rule is wrong, change it in the same commit and say why.

## Rules

- English everywhere except `locales/`. Comments say why, not what.
- No user-facing string in code or templates, English included: use locale keys.
- No reader layout in code: page structure belongs to theme templates.
- Core stays minimal. A feature that is not essential to a working blog belongs in a theme or plugin.
- New dependency: only in the workspace `Cargo.toml`, with a `# why` comment that beats writing the code.
- Never write notes, plans or reports as files. Decisions go in `docs/ARCHITECTURE.md`.
- Generate references from code; never hand-write them.
- Small, scoped changes. No renames or refactors outside the task.
- Errors are never silent: a plain message for the user, enough in the log for the next agent.
- A workflow done 3+ times becomes a skill: `.claude/skills/<name>/SKILL.md`, English, <= 120 lines. Update an existing skill instead of adding a near-duplicate.

## Where things go

| Adding | Goes in |
|---|---|
| Types, ids, error codes, permissions | `crates/model` |
| Markdown, i18n, auth, system log, theme runtime (no I/O) | `crates/{markdown,i18n,auth,syslog,render}` |
| Storage trait or in-memory impl | `crates/store` |
| Routes, D1/R2/fetch, MCP (only crate with I/O) | `crates/worker` |
| A `blog` command | `crates/cli` |
| Instructions for an AI running a blog | `guides/<topic>.md` |
| UI text | `locales/<lang>.toml` |
| Schema change | new numbered file in `migrations/` |
| A default plugin | `plugins/<name>/` |
| A repeated workflow for agents | `.claude/skills/<name>/SKILL.md` |

## Writing docs

- Guides and this file: short, precise, no repetition. Say each thing once, in one place.
- `README.md`: written for people. Plain words, honest about what works and what doesn't.

## Done means

- `scripts/check` green.
- Bug fix: a test that failed before and passes after.
- UI change: screenshots at 375, 768, 1440 px in light and dark; Lighthouse 100.
- Feature: "do this, see that" steps, including empty, offline and no-permission cases.
