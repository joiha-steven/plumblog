# plumblog

A blog that runs on Cloudflare's free plan and that your AI agent can run for you.

**It doesn't work yet.** This is the start of the project. There is nothing to install today.
Watch the repo if you want to know when it does.

## The idea

Most blogs need a server, or a paid plan, or both. plumblog needs neither. It runs on your own
Cloudflare account: your posts go in a small database there, your images go in storage there,
and the free plan is enough.

You don't need to know how any of that works. You clone this repo, give your AI agent a
Cloudflare token, and ask it to set up your blog. After that you can write in the browser like
any other blog, or ask your agent to write, translate, change the look, or back everything up.

Your posts never live in this repo. It only holds the code. Updating to a new version is a
`git pull` and one command, and it can't touch your posts.

## What it will have

- A clean writing screen with a toolbar, full Markdown, and drag-and-drop images
- Posts in several languages, linked to each other
- Three themes, each with a light and a dark mode, and an easy way to make your own
- Pages that load fast and score 100 on Google PageSpeed
- Sign-in with a password or a passkey, for you and the people who write with you

## Working on it

You need Rust and `jq` (`brew install jq` on a Mac). Then:

```sh
scripts/check
```

If you are an AI agent, start with `AGENTS.md`.

## License

AGPL-3.0. Your posts and your themes are yours.
