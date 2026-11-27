# plumblog

A fast, multilingual blog that runs entirely on Cloudflare's free plan, written in Rust,
and designed to be run by your AI agent.

> Status: early development. Nothing to install yet.

## How it works

- Your blog lives on your Cloudflare account: one Worker, a D1 database, an R2 bucket. No server to rent.
- This repository holds only the code, the default themes and the guides. Your posts never live in git.
- You clone it, give your AI agent a scoped Cloudflare API token, and ask it to set up the blog.
  From then on you write in the browser, or ask your agent to write, translate, restyle or back up.

## For AI agents

Run `blog guide` to list the guides, then `blog guide <topic>`. Contributors start at `AGENTS.md`.

## Development

Requires Rust (the toolchain is pinned in `rust-toolchain.toml`).

```sh
scripts/check
```

## License

AGPL-3.0-only. See `LICENSE`.
