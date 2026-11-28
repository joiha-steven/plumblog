# Export

Copy the blog's content out as portable files: one Markdown file per post plus a JSON index.

For a restorable copy of everything use `blog guide backup` instead; this guide is for moving
content elsewhere or keeping a readable archive. Tokens: a `read` token for steps 1-4, a
`writer` or `owner` token for comments (step 5), an `owner` token for users (step 6); see
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
3. Media: page through `GET /api/v1/media`, then save each original from
   `GET /api/v1/media/{id}/original` as `media/<id>/<file name of src>`: the bytes as
   uploaded, `Content-Type` its `mime`. A 404 there means the stored file is gone: list it
   as missing, do not stop.
4. Site: `GET /api/v1/settings`, `GET /api/v1/themes/minimal/options`, `GET /api/v1/plugins`.
5. Comments (`writer` or `owner` token): page through `GET /api/v1/comments?limit=100`. Every
   status comes back, spam and trash included; keep `post_id` and `parent_id` for threads.
6. Users (`owner` token): page through `GET /api/v1/users?limit=100`. No password, session,
   passkey or token data is ever returned, so accounts cannot be restored with their sign-in.
7. Write `posts/<lang>/<slug>.md` with front matter, then the Markdown body unchanged:

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
8. Write `export.json` with tags, series, redirects, media metadata, comments, users, settings
   and options as the API returned them, plus the export time and the site origin.
9. Count the files you wrote against the posts the pages returned (the API gives no totals)
   and report any post you skipped and why.

## Not covered yet

- The system log has no API route.

Say what was left out in your report; never present a partial export as complete.
