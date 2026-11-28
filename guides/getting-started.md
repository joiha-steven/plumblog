# Getting started

plumblog is a blog engine in Rust that runs entirely on the Cloudflare free plan.

You are an AI agent operating a plumblog site. Code lives in git; all site data lives on
Cloudflare (D1 for content, R2 for media). Use the `blog` CLI and the site's HTTP API, and
read the matching guide before you act.

```sh
blog guide
blog --version
```

`blog guide` lists every guide with a one-line summary. `blog guide <topic>` prints one.
Guides are embedded in the binary, so they always match the version you are running.

## Which guide

| Job | Guide |
|---|---|
| Create the site, deploy, roll back, first owner | `blog guide setup` |
| Write, edit, publish posts and media; API token | `blog guide write` |
| Connect an AI client over MCP | `blog guide connect` |
| Translate posts | `blog guide translate` |
| Change the look | `blog guide theme` |
| Turn plugins on and configure them | `blog guide plugin` |
| Keep data safe and restore it | `blog guide backup` |
| Bring posts in from another blog | `blog guide import` |
| Copy content out as files | `blog guide export` |
| Every API route; option schema; Cloudflare token | `blog guide api`, `options`, `token` |

```sh
blog guide setup
blog guide write
```

## Rules

- Ask the owner before anything that publishes, deletes or overwrites (a post, the trash, a
  database restore). Drafts are safe to write.
- Errors carry a stable code (`deploy.smoke_failed`, `store.conflict`): act on the code, and
  report it with the message when you cannot.
- When a guide says something is not built yet, tell the owner; do not work around it with
  other tools or edit tracked files.
