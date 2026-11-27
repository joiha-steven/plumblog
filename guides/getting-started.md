# Getting started

plumblog is a blog engine in Rust that runs entirely on the Cloudflare free plan.

You are an AI agent operating a plumblog site. Code lives in git; all site data lives on
Cloudflare (D1 for content, R2 for media). Use the `blog` CLI for every task, and read the
matching guide before you act.

```sh
blog guide
blog --version
```

`blog guide` lists every guide with a one-line summary. `blog guide <topic>` prints one.
Guides are embedded in the binary, so they always match the version you are running.

## Put a site on Cloudflare

Run these in the site's folder. `blog` reads `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID`
from the environment or `.env`, and keeps what it made in `site.local.toml` (never the token).

```sh
blog guide token
blog setup --check-token
blog setup
blog deploy
```

1. `blog guide token` says how to create the token; `--check-token` checks it.
2. `blog setup` makes the database, media bucket, Worker, secrets, Turnstile and a one-time
   setup code for the owner. Safe to run again. `--domain <host>` adds a custom domain.
3. `blog deploy` downloads this version's release bundle once and checks it against the hash
   built into `blog` (`--build` builds in the plumblog repository, `--bundle <dir>` takes a
   folder; a build from source needs one of them). Order:
   token; a Time Travel bookmark (always) and a SQL export into `backups/` (when migrations
   are pending, or with `--export`); migrations; static files; a new Worker version; switch;
   cron; check `/healthz` and `/`. If the check fails within 60 s the previous version goes
   back by itself (`deploy.smoke_failed`). Steps are recorded under `deploys` in
   `site.local.toml`.

```sh
blog rollback
blog rollback --to <version-id>
blog rollback --db <bookmark> --yes
```

`blog rollback` returns to the version before the last deploy; `--to` picks a recent one. A
version whose bundle lacked migrations the database has run, or that `blog` did not deploy from
this folder, is refused (`rollback.older`) unless `--allow-older` is added.
`--db` overwrites the whole database with its state at the bookmark and cancels running
queries, so it needs `--yes`; it prints the bookmark that undoes it. After a failed migration
every deploy refuses until `blog rollback --db` has run. `cf.network_write_uncertain` means the
change may already be applied: look before you retry.

## Update

Run `blog update` (`--check` only asks), then `git pull` for the guides, then `blog deploy`. It
installs a release only if its signed `SHA256SUMS` checks out; `--to vX.Y.Z` picks one; an older one
is refused unless you add `--allow-older`. A `release.*` or `update.*` error about a hash or signature means a download was not
the official file: follow the message, never work around it.
