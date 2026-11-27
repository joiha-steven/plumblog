# Setup

Put a blog on Cloudflare, deploy new versions, roll back, and create the first owner.

Run these in the site's folder. `blog` reads `CLOUDFLARE_API_TOKEN` and `CLOUDFLARE_ACCOUNT_ID`
from the environment or `.env`, and keeps what it made in `site.local.toml` (never the token).

```sh
blog guide token
blog setup --check-token
blog setup
blog deploy
blog deploy --bundle <dir>
```

1. `blog guide token` says how to create the token; `--check-token` checks it.
2. `blog setup` makes the database, media bucket, Worker, secrets, Turnstile and a one-time
   setup code for the owner. Safe to run again. `--domain <host>` adds a custom domain.
3. `blog deploy` builds in the plumblog repository; elsewhere pass a release bundle. Order:
   token; a Time Travel bookmark (always) and a SQL export into `backups/` (when migrations
   are pending, or with `--export`); migrations; static files; a new Worker version; switch;
   cron; check `/healthz` and `/`. If the check fails within 60 s the previous version goes
   back by itself (`deploy.smoke_failed`). Steps are recorded under `deploys` in
   `site.local.toml`.
4. Give the owner the site address and the setup code; they open `<origin>/admin/setup` and
   create their account. The code works once, for 24 hours; `blog setup` again prints a new
   one while no owner exists. Never store or log the code.

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
every deploy refuses until `blog rollback --db` to that migration's own bookmark (printed with
the failure) has run; another bookmark also needs `--clear-failed-migration`. `cf.network_write_uncertain` means the
change may already be applied: look before you retry.

## Updating

`git pull`, then `blog deploy`. Never edit tracked files for one site: a pull must not conflict.

## Not built yet

`blog admin reset` (owner access recovery with the Cloudflare token) does not exist: an owner
who loses every password and passkey cannot get back in.
