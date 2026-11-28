# Theme

Change how the blog looks: theme options, styles and colors through the API.

The blog ships the `minimal` theme (sans-serif, light and dark). Its options are declared in
`themes/minimal/theme.toml`; read that file for the keys, types, choices and defaults. Needs a
token with `theme.manage` (owner); see `blog guide write` for the token and request conventions.

```sh
blog guide options
blog guide api
```

`blog guide options` gives every field type and the checks a value must pass.

## Change options

| Do | Request |
|---|---|
| Read the stored values | `GET /api/v1/themes/minimal/options` |
| Set one option | `PUT /api/v1/themes/minimal/options/{key}` `{"value": ...}` |
| Reset all to defaults | `DELETE /api/v1/themes/minimal/options` |

Only stored values come back; anything missing uses its default from `theme.toml`. Values by
type: `choice` the choice's `value` string; `color` a hex string that overrides the token the
option names; `toggle` `true`/`false`; `number` within its limits; `image` a media id from
`blog guide write`. Choices marked `draft` are not offered: do not set them.

The API checks only the key's shape, not the declaration. A value that fails it is not refused:
pages use the default and log `option.invalid`. So check each value against `theme.toml`
before writing, then load the home page and a post and confirm the change shows.

Every option write bumps the content version, so cached pages refresh at once.

## Making or changing a theme

A theme is a folder: `theme.toml`, `style.css`, `styles/`, `templates/`, `assets/`, `locales/`.
Work in the site folder, never inside the plumblog checkout.

1. `blog theme new paper` copies the default theme into `paper/` as theme `paper`.
2. `blog theme dev paper` previews it at http://127.0.0.1:4321/ with sample posts and, like the
   live site, only the folder's own `assets/`; refresh after each edit. Problems show in the page.
3. `blog theme check paper` must end with no errors (warnings are allowed). It lists each
   problem with file, line and a fix.
4. `blog theme push paper` checks again, then deploys a new version with the theme and checks
   that the site serves it; if not, the previous version comes back by itself.
5. `blog theme rollback` goes back to the version before the last push (`blog theme list` shows
   the versions; `--to <version-id>` picks one). `blog theme push --default` returns to the
   default theme.

Every file of a pushed theme is public on the site: never put tokens, keys or private notes in
the folder (check refuses the obvious ones). Later `blog deploy` runs keep the pushed theme; if
one says it cannot (`deploy.theme_unrecoverable`), push the folder again, or deploy with
`--theme-default` only if the owner wants the default theme back.

Rules a theme must keep: colors only through tokens with light and dark values, no text outside
its `locales/`, PageSpeed 100 and no layout shift. Every file it uses (`asset()`, fonts, scripts,
also from default templates it borrows) is in its `assets/`, named with `A-Z a-z 0-9 . _ -` only.
Text files (all but `assets/`) stay under 256 KB in all, or the site shows the default theme.
