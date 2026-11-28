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

A theme is a folder in the repository (`theme.toml`, `style.css`, `templates/`, `assets/`,
`locales/`), built into the Worker, so today a new theme or template edit is contributor work:
a fork, `scripts/check`, and a deploy of that build. The `blog theme` commands described in
`docs/ARCHITECTURE.md` (new, dev, check, push, rollback) do not exist yet.

Rules a theme must keep: colors only through tokens with light and dark values, no text outside
its `locales/`, PageSpeed 100 and no layout shift.
