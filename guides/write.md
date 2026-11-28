# Write

Create, edit, publish and delete posts and media through the HTTP API, and how to call it.

```sh
blog guide api
```

`blog guide api` is the full route reference (fields, access, answers); this guide does not repeat it. Other guides use the conventions below.

## Token and requests

The owner creates a token while signed in at `<origin>/admin` (no admin screen yet), in the console:
`await (await fetch('/api/auth/tokens', {method: 'POST', body: JSON.stringify({name: 'agent', scope: 'writer'})})).json()`.
Scopes: `read` (look only), `writer` (posts, media), `owner` (also settings, themes, plugins).
The answer shows the token once; keep it in an environment variable, never in a file in git.

- Send `Authorization: Bearer $TOKEN` and JSON bodies (`curl --json`); integers only.
- Times are UTC milliseconds. Lists: `limit` <= 100, then `cursor=<next_cursor>` until null.
- `GET /api/v1/posts` also returns `counts` (posts per status) on the first page; `null` when `cursor` is set.
- Errors are `{"error": "<code>", "field": "<name>"}`: fix the named field, never retry blindly.

## Posts

- Create: `POST /api/v1/posts` with `lang`, `slug`, `title`, `markdown`; `status` is `draft`
  (default), `scheduled` (needs a future `published_at`) or `published` (`published_at`
  defaults to now; a past time is allowed). `kind`: `post` (default) or `page`. Add a new
  `client_id` (a UUID) per post to retry a create with no answer: it returns the same post.
  Never reuse a `client_id` for another post: a repeated one returns the existing post whatever the body.
- Slug: `a-z`, `0-9` and inner `-`, 1-100 characters. Never use `page`, `tag`, `archive`,
  `search`, `theme`, `admin` or `api`: the API saves them but readers can never open them.
  One `(lang, slug)` per site (409 on `slug`).
- Edit: `GET /api/v1/posts/{id}` for the full post and its `version`, then
  `PATCH /api/v1/posts/{id}` with only the changed fields and `expected_version`. A 409
  `store.conflict` means someone else saved: read again, merge, never overwrite (a retried PATCH that already landed gets 200).
- Tags and series are ids: find or create them first (`/api/v1/tags`, `/api/v1/series`; lists are paged, follow `next_cursor`).
- Delete: `POST /api/v1/posts/{id}/trash` (undo with `/restore`); `DELETE` only from the trash and only when the owner asked.
- The answer may carry `warnings` (for example `markdown.math_invalid`): fix and save again.

## Markdown

CommonMark and GFM, footnotes, math (`$...$`, `$$...$$`), GitHub alerts (`> [!NOTE]`,
`TIP`, `IMPORTANT`, `WARNING`, `CAUTION`). No raw HTML: tags are dropped. An image alone in a
paragraph is a figure; its title is the caption: `![alt](/media/12/heron.jpg "A heron")`.
A YouTube or Vimeo URL alone on a line is an embed. The title is the only top heading: start
the body at `##`.

## Media

Upload the original bytes: `POST /api/v1/media?name=heron.jpg&mime=image/jpeg&width=1600&height=1067&sha256=...&alt=...`
with `curl --data-binary @heron.jpg`. `width`, `height`: the file's own (read them, never guess);
AVIF, WebP, JPEG, PNG or GIF, at most 20 MiB. Use the answer's `src` in Markdown. Always send
`sha256`, the lowercase hex SHA-256 of the bytes you send: if it matches one of your items, that
item is the answer (200, unchanged, these bytes not checked); else R2 checks it (wrong: 400).
Always write `alt` (later: `PATCH /api/v1/media/{id}`). Delete own uploads only (owner: any); used: 409.

## Not built yet

The public site still renders the built-in sample content, not posts saved through the API,
and `/media/...` is not served yet. Check results with `GET /api/v1/posts/{id}`.
