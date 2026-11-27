# Translate

Translate a post into another language and link the two, so readers can switch between them.

You do the translating; plumblog stores one post per language, linked by a translation group.
Token and request conventions are in `blog guide write`.

```sh
blog guide write
```

## Steps

1. `GET /api/v1/posts/{id}` for the source post (`markdown`, `title`, `excerpt`, `lang`).
2. Translate the title, the excerpt and the body. Keep the Markdown structure exactly:
   headings, lists, footnote labels, link and image targets, code blocks, math and
   `> [!NOTE]` markers stay as they are; translate only the text, including figure captions
   (the image title), image `alt` and link text.
3. Pick a slug in the target language's words (`a-z`, `0-9`, `-`; transliterate, drop marks).
4. `POST /api/v1/posts` with the target `lang` (a tag like `vi`, `pt-br`), the new `slug`,
   `title`, `markdown`, the same `kind`, `tags`, `series_id` and `cover_media_id`, and
   `translation_of` = the source post's id (any post of the group works).
5. Status: save as `draft` and let the owner publish, unless they asked you to publish.

Errors: `translation_of` "no such post" means a wrong id; a 409 on `lang` means the group
already has that language: `PATCH` that post instead of creating another.

## Updating a translation

When the source changes, list the group's posts (`GET /api/v1/posts?lang=<target>` and match
the translation group), diff the source against what was translated, and `PATCH` only what
changed, with `expected_version`.

## Style

Write as a native speaker would, not word for word. Keep names, product names, code and
numbers unchanged. Vietnamese: keep every tone mark; never strip diacritics from body text.

## Not built yet

The public site does not show translations yet (no language switcher or `/<lang>/` pages);
they are stored and returned by the API.
