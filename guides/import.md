# Import

Move posts, tags and images from WordPress, Ghost, Hugo, Jekyll or Zola into plumblog.

You read the old blog's export on this machine and write through the API. Token and request
conventions, Markdown rules and media upload are in `blog guide write`; use a `writer` token
(`owner` if you also add redirects).

```sh
blog guide write
blog guide api
```

## Read the source

| From | Read | Body |
|---|---|---|
| WordPress | Tools > Export WXR file (`<item>` with `wp:post_type` post or page) | HTML: convert to Markdown |
| Ghost | Settings > Export JSON (`db[0].data.posts`, `tags`, `posts_tags`) | `lexical` or `mobiledoc`; prefer `html`, convert |
| Hugo, Jekyll, Zola | `content/`, `_posts/` Markdown with YAML or TOML front matter | Markdown: strip shortcodes and Liquid tags |

Images: from the export's uploads folder, or download the URLs the posts use.

## Steps

1. Dry run first: parse everything, list counts (posts, pages, drafts, tags, images) and every
   problem (missing image, slug clash, unsupported block), and show the owner before writing.
2. Tags and series: create each once (`POST /api/v1/tags` with `slug`, `name`), keep a map
   from the old name to the new id.
3. Images: upload each file once (`POST /api/v1/media`), keep a map from every old URL
   (full size and resized variants) to the answer's `src`.
4. Posts: convert the body to Markdown, rewrite image and internal links with the maps, then
   `POST /api/v1/posts` with the old `slug` (folded to `a-z0-9-`), `lang`, `title`,
   `excerpt`, `tags`, `kind`, and status: published posts as `published` with the original
   `published_at` (UTC ms), drafts as `draft`. Keep a log: old id, new id, outcome.
5. Old URLs: for each post whose old path differs from `/<slug>`, an owner token adds
   `PUT /api/v1/redirects` `{"from": "/2019/05/old-slug/", "to": "/old-slug"}`.
6. Check: counts match the dry run, `GET /api/v1/media?missing_alt=true` is empty or reported,
   and every failure in the log is explained to the owner.

## Converting

- Raw HTML is dropped by the renderer, so convert it all: `<figure>` to an image with a
  caption title, `<iframe>` YouTube or Vimeo to the bare URL on its own line, tables to GFM.
- WordPress blocks (`<!-- wp:... -->`) and shortcodes (`[caption]`, `[gallery]`): keep their
  content, drop the markers. Hugo `{{< figure >}}`, Jekyll `{% include %}`: same.
- The title is the post's top heading: shift body headings so they start at `##`.
- Comments, authors and users are not imported (no API route); say so in the report.

Re-running is safe only if you skip posts whose `(lang, slug)` already exists (409 on `slug`)
and reuse media from your log: a repeated upload makes a second copy.
