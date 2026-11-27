# Backup

Keep the blog's data safe: what Cloudflare keeps for you, what to copy off it, and how to restore.

All data lives in D1 (posts, users, settings, log) and R2 (media). Code lives in git and is
never backed up. Run commands in the site's folder; see `blog guide setup` for the token.

## What already exists

- **Time Travel**: D1 can return to any minute of the last 7 days (free plan). Every
  `blog deploy` records a bookmark under `deploys` in `site.local.toml`.
- **SQL exports**: `blog deploy` writes `backups/<utc>-<database>.sql` (mode 0600, ignored by
  git) when migrations are pending, or always with `--export`. The database answers no queries
  while the export runs, so do it when traffic is low.

```sh
blog deploy --export
```

There is no export without a deploy yet; `--export` redeploys the code you already run, which
is safe. Copy `backups/` somewhere off this machine: it holds password hashes and drafts, so
keep it private.

## Restore

```sh
blog rollback --db <bookmark> --yes
```

Overwrites the whole database with its state at the bookmark (within 7 days) and cancels
running queries. It prints a bookmark that undoes it; keep that line. Ask the owner first.

## Not covered yet

- Media in R2: no route reads an original back, so plumblog cannot copy media yet.
- Restoring from a `.sql` file older than 7 days: no `blog` command.
- Recording "last backup" for the admin dashboard: no command or setting yet.

Tell the owner these gaps instead of improvising with other tools.
