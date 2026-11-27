# Backup

Keep the blog's data safe: what Cloudflare keeps for you, what to copy off it, and how to restore.

All data lives in D1 (posts, users, settings, log) and R2 (media). Code lives in git and is
never backed up. Run commands in the site's folder; see `blog guide setup` for the token.

## What already exists

- **Time Travel**: D1 can return to any minute of the last 7 days (free plan). Every
  `blog deploy` and `blog backup` records a bookmark in `site.local.toml`.
- **SQL dumps**: `blog` reads the database page by page and writes
  `backups/<utc>-<database>.sql` itself (mode 0600, ignored by git); Cloudflare's own export
  refuses a database with a search index, which every blog has. The site stays up, but every
  row read counts against D1's daily read limit (5 million on the free plan). The copy is not
  one instant: its first lines name the bookmark taken just before, its last lines the rows
  per table. `blog deploy` writes one when migrations are pending, or always with `--export`.
- **`blog backup`**: a dump without deploying, plus every media file (originals and resized
  variants) from R2 to `backups/media/<r2 key>`. A row too large for one D1 statement
  (100,000 bytes as SQL) stops it with `backup.row_too_large` and no file.

```sh
blog backup
```

`--no-media` copies only the database; `--out <dir>` picks another folder than `backups/`.
Media copying resumes: `backups/media/manifest.json` lists each file's size and SHA-256, and a
re-run fetches only what is missing or changed. Each file is checked against the size the
database records, and against R2's checksum when it is a plain MD5. A file that fails is not
saved; the run goes on and ends with exit code 1 (`backup.incomplete`) and a list: run it
again to retry. A row whose file is not in R2 is reported (`backup.object_missing`); retrying
cannot fix it, and the backup counts as not complete. The token needs R2 permission
(`blog guide token`).

Each run records itself as the site's `last_backup` setting (`at`, `database`, `media`
counts or null, `complete`) and in `site.local.toml`; only `blog backup` writes it.

Copy `backups/` somewhere off this machine: it holds password hashes and drafts, so keep it
private.

## Restore

To a Time Travel bookmark (within 7 days), or from a `.sql` file of any age:

```sh
blog rollback --db <bookmark> --yes
blog restore backups/<utc>-<database>.sql --yes
```

Both overwrite the whole database and cancel running queries; ask the owner first, and keep
the undo command they print. Without `--yes`, `blog restore` only checks the file and prints
its plan. It refuses a file that is not a blog backup, another site's (`--allow-other-site`),
one from newer code (`restore.backup_newer`: update `blog` first) or a cut one (no row
counts). It saves a bookmark, imports, checks every table's row count
(`restore.count_mismatch` names the undo command to run), then reruns missing migrations.

## Not covered yet

Restoring media from `backups/media` has no `blog` command. Tell the owner this gap instead
of improvising with other tools.
