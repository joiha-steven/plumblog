# Getting started

plumblog is a blog engine in Rust that runs entirely on the Cloudflare free plan.

You are an AI agent operating a plumblog site. Code lives in git; all site data lives on
Cloudflare (D1 for content, R2 for media). Use the `blog` CLI for every task, and read the
matching guide before you act.

```sh
blog guide
blog --version
```

`blog guide` lists every guide with a one-line summary. `blog guide <topic>` prints one.
Guides are embedded in the binary, so they always match the version you are running.
