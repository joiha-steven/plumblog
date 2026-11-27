# plumblog

A blog that runs on Cloudflare's free plan and that your AI agent can run for you.

**It doesn't work yet.** This is the start of the project. No release has been published yet,
so there is nothing to install today. Watch the repo if you want to know when there is.

## The idea

Most blogs need a server, or a paid plan, or both. plumblog needs neither. It runs on your own
Cloudflare account: your posts go in a small database there, your images go in storage there,
and the free plan is enough.

You don't need to know how any of that works. You clone this repo, give your AI agent a
Cloudflare token, and ask it to set up your blog. After that you can write in the browser like
any other blog, or ask your agent to write, translate, change the look, or back everything up.

Your posts never live in this repo. It only holds the code. Updating to a new version is a
`git pull` and one command, and it can't touch your posts.

## Installing

You need no programming tools. Everything comes as one program, `blog`.

1. On the [Releases page](https://github.com/joiha-steven/plumblog/releases), download the file
   for your computer (for example `blog-v0.2.0-aarch64-apple-darwin.tar.gz` for a recent Mac),
   plus `SHA256SUMS` and `SHA256SUMS.minisig`.
2. Check that the download is the real one. With [minisign](https://jedisct1.github.io/minisign/)
   and this repo's `release/minisign.pub`:
   ```sh
   minisign -V -p release/minisign.pub -m SHA256SUMS
   grep blog-v0.2.0-aarch64-apple-darwin.tar.gz SHA256SUMS | shasum -a 256 -c
   ```
   Or, with the GitHub CLI: `gh attestation verify <file> --repo joiha-steven/plumblog`.
3. Unpack it and put `blog` in a folder on your PATH (for example `~/.local/bin`).

`blog deploy` then fetches the matching website files by itself and checks them too.

## Updating

```sh
blog update
git pull
blog deploy
```

`blog update` replaces `blog` with the newest release, but only if its signature checks out.
`git pull` brings the newest guides for your agent, and `blog deploy` puts the new version
online. `blog update --check` only tells you whether there is a newer one.

## What it will have

- A clean writing screen with a toolbar, full Markdown, and drag-and-drop images
- Posts in several languages, linked to each other
- Three themes, each with a light and a dark mode, and an easy way to make your own
- Pages that load fast and score 100 on Google PageSpeed
- Sign-in with a password or a passkey, for you and the people who write with you

## Working on it

To change plumblog itself you need Rust, `jq` (`brew install jq` on a Mac) and Node.js 22 or newer (Node only runs the local Worker tests; people running a blog never need it). Then:

```sh
scripts/check
```

A build from source has no release files of its own: deploy it with `blog deploy --build`
inside this repo.

If you are an AI agent, start with `AGENTS.md`.

## License

AGPL-3.0. Your posts and your themes are yours.
