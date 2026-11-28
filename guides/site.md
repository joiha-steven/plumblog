# Site

Set the site title, description, footer, default language and page size, and know where posts are.

Owner token needed; conventions are in `blog guide write`.

```sh
blog guide write
```

## Settings

`PUT /api/v1/settings/{key}` with `{"value": ...}`. All are optional: a fresh site renders
with the defaults. A malformed value falls back to its default.

| Key | Value | Default |
|---|---|---|
| `site_title` | string | "Untitled site" |
| `site_description` | string | empty |
| `site_footer` | string | empty |
| `site_lang` | language code (`en`, `pt-br`) | `en` |
| `site_languages` | array of language codes | `[site_lang]` |
| `site_per_page` | integer 1-50 | 10 |

Changing a setting changes the public site at once. Set `site_lang` before publishing:
changing it moves that language's posts to `/<slug>` and breaks old links.

## Where a post is

- Default language (`site_lang`): `/<slug>`. Any other language: `/<lang>/<slug>`, never at `/<slug>`.
- Not usable as a slug in the default language: `page`, `tag`, `archive`, `search`, `theme`,
  `plugin`, `media`, `admin`, `api`, `healthz` (400 on `slug`); another language may.
- `status: published` with a future `published_at` is 400 `post.published_in_future`: use
  `scheduled`. The home page lists the default language only.
- Only published posts and pages are public; drafts, scheduled and trashed posts are 404.
- Unpublish: `PATCH` the post with `status: "draft"` and `published_at: null`.
- Listings are numbered (`/page/2`); a page past the last, or past 10,000 posts, is a 404.
- Feeds: `/feed.xml`, `/rss.xml`, `/feed.json` (default language), `/<lang>/feed.xml` for others.
