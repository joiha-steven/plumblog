# Plugin

Turn plugins on and off and set their options through the API.

Plugins are declarative: HTML partials for theme slots, browser CSS/JS and locale files, never
server code. They come only from `plugins/` in the repository, built into the Worker; a disabled
plugin costs nothing. Needs an owner token (`plugin.manage`); see `blog guide write` for the
token and request conventions, `blog guide api` for every field.

```sh
blog guide api
blog guide options
```

## Enable, configure, disable

Bundled plugins are the folders in `plugins/` of the deployed version. The API accepts any
well-formed name without checking it exists, so take names from that folder.

| Do | Request |
|---|---|
| List plugins with a stored state (never set = disabled) | `GET /api/v1/plugins` |
| Enable or disable | `PUT /api/v1/plugins/{name}` `{"enabled": true}` |
| Read options (owner only: may hold secrets) | `GET /api/v1/plugins/{name}/options` |
| Set one option | `PUT /api/v1/plugins/{name}/options/{key}` `{"value": ...}` |
| Reset all options to defaults | `DELETE /api/v1/plugins/{name}/options` |

Option keys and value shapes come from the plugin's `plugin.toml`; `blog guide options` gives
the rules each value must pass. A value that fails them is not refused by the API: the page
uses the default and logs `option.invalid`. Read the plugin's `plugin.toml` and check values
yourself before writing.

After a change, load a page that has the plugin's slot and confirm it shows. A plugin whose
partial fails is left out of the page and logged as `plugin.broken`; the blog keeps working.
A partial for a slot the active theme does not offer is not shown (`plugin.slot_missing`).

## Writing a new plugin

A new plugin is a folder `plugins/<name>/` in the repository (`plugin.toml`, `partials/`,
`assets/`, `locales/`), so today it is contributor work: it means a fork and a build, and the
owner's blog must deploy that build. There is no `blog plugin` command yet.

For anything that needs server code (newsletters, syncing, notifications), build a separate
service that uses the API instead of a plugin (no route creates outgoing webhooks yet).
