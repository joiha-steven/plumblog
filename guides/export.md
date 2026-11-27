# Export

Copy the blog's content out as portable files: one Markdown file per post plus a JSON index.

For a restorable copy of everything use `blog guide backup` instead; this guide is for moving
content elsewhere or keeping a readable archive. A `read` token is enough; see
`blog guide write` for the token and request conventions.

```sh
blog guide backup
blog guide api
```

## Steps

1. Posts: page through `GET /api/v1/posts?limit=100` (then `&cursor=<next_cursor>` until it
   is null). Without `status` every post comes back, drafts and trash included: skip
   `status: trashed` unless the owner wants the trash. Read each in full with
   `GET /api/v1/posts/{id}`.
2. Taxonomy and links: `GET /api/v1/tags`, `GET /api/v1/series`, `GET /api/v1/redirects`.
3. Media list: page through `GET /api/v1/media`.
4. Site: `GET /api/v1/settings`, `GET /api/v1/themes/minimal/options`, `GET /api/v1/plugins`.
5. Write `posts/<lang>/<slug>.md` with front matter, then the Markdown body unchanged:

   ```yaml
   ---
   id: 12
   title: "..."
   lang: en
   slug: hello
   status: published
   published_at: 2026-11-28T09:00:00Z
   tags: [rust, notes]
   series: ...
   translation_group: 7
   excerpt: "..."
   ---
   ```

   Convert UTC milliseconds to ISO 8601, tag and series ids to slugs, and keep the original
   ids so links between files survive.
6. Write `export.json` with tags, series, redirects, media metadata, settings and options as
   the API returned them, plus the export time and the site origin.
7. Count the files you wrote against the posts the pages returned (the API gives no totals)
   and report any post you skipped and why.

## Not covered yet

- Media files: no route reads an original back (`/media/<id>` is not served yet), so only
  media metadata can be exported.
- Comments, users and the system log have no API route.

Say what was left out in your report; never present a partial export as complete.
