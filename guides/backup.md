# Backup

Keep the blog's data safe: what Cloudflare keeps for you, what to copy off it, and how to restore.

All data lives in D1 (posts, users, settings, log) and R2 (media). Code lives in git and is
never backed up. Run commands in the site's folder; see `blog guide setup` for the token.

## What already exists

- **Time Travel**: D1 can return to any minute of the last 7 days (free plan). Every
  `blog deploy` records a bookmark under `deploys` in `site.local.toml`.
- **SQL exports**: `blog deploy` writes `backups/<utc>-<database>.sql` (mode 0600, ignored by
  git) when migrations are pending, or always with `--export`.
- **`blog backup`**: copies the data off Cloudflare without deploying. It exports the database
  the same way and copies every media file (originals and resized variants) from R2 to
  `backups/media/<r2 key>`. The database answers no queries while the export runs, so do it
  when traffic is low.

```sh
blog backup
```

`--no-media` copies only the database; `--out <dir>` chooses another folder than `backups/`.

Media copying resumes: `backups/media/manifest.json` lists each file with its size and SHA-256,
and a re-run fetches only what is missing or changed. Every file is checked against the size
the database records, and against R2's checksum when it has a plain MD5. A file that fails a
check is not saved, the run goes on, and it ends with exit code 1 (`backup.incomplete`) and a
list of what failed; run it again to retry those. A database row whose file is not in R2 is
reported (`backup.object_missing`) but cannot be fixed by retrying; it makes the backup
"not complete", not failed. A run also needs the token's R2 permission (`blog guide token`).

Each run records itself as the site's `last_backup` setting
(`{"at": <unix seconds>, "database", "media": {"objects", "bytes", "missing", "failed"} or null,
"complete"}`) and in `site.local.toml`. `complete` is true only when the database and every
media file were copied. The API never writes that setting; only `blog backup` does.

Copy `backups/` somewhere off this machine: it holds password hashes and drafts, so keep it
private.

## Restore

```sh
blog rollback --db <bookmark> --yes
```

Overwrites the whole database with its state at the bookmark (within 7 days) and cancels
running queries. It prints a bookmark that undoes it; keep that line. Ask the owner first.

## Not covered yet

- Restoring from a `.sql` file older than 7 days: no `blog` command.
- Restoring media from `backups/media`: no `blog` command.

Tell the owner these gaps instead of improvising with other tools.
