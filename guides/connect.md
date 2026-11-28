# Connect

Connect an AI client to a running blog over MCP, with the site address and an API token.

The site serves MCP at `<origin>/mcp` (Streamable HTTP, JSON answers, no session). Nothing to
install or clone: the client needs the address and a token. How the owner creates a token,
and what its scope allows, is in `blog guide write`.

```sh
blog guide write
```

## Add the server

Any MCP client that speaks Streamable HTTP and can send a header works. Claude Code:

```sh
claude mcp add --transport http plumblog https://example.com/mcp \
  --header "Authorization: Bearer $PLUMBLOG_TOKEN"
```

Other clients: server URL `https://example.com/mcp`, header
`Authorization: Bearer <token>`. Keep the token in the client's secret settings or an
environment variable, never in a file in git. A session cookie is not accepted: only a token.

## What the client gets

- Tools: one per `/api/v1` route (`post_list`, `post_get`, `post_create`, `post_update`,
  `tag_create`, `setting_put`, ...), with the same fields, checks and error codes as the API
  (`blog guide api`). The list shows only what the token may do: a `read` token sees no
  writes. Media upload and reading an original are not tools (raw bytes): use HTTP as in
  `blog guide write`.
- Resources: every guide, as `plumblog://guides/<topic>`. Read `getting-started` first. Where
  a guide says `blog guide <topic>`, read `plumblog://guides/<topic>` instead; `blog guide api`
  is not a resource, as `tools/list` already gives every field.

A tool that fails returns `isError` with `{"error": "<code>", "field": "<name>"}`: fix the named
field and call again; on `store.conflict` read the post again before you write. A request the
server refuses outright (no token, wrong token, body too large) is a JSON-RPC error with the
same `error` code in its `data`.

## Check it

Ask the client to list its plumblog tools, then to call `me`: it answers with the token's
user and effective scope. `auth.unauthenticated` means the token is wrong, expired or revoked.
