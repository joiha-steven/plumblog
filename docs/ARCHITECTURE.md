# Architecture

The single design document. Every decision lives here, in the section it affects.
When a decision changes, edit the section and add one line to the Decision log at the bottom.

## Product

A multilingual blog engine, written in Rust, that runs entirely on the Cloudflare free plan.
The person running a blog is expected to have their own AI agent: they clone the repo, give the
agent a Cloudflare API token, and the agent sets up, writes, translates, themes and maintains.

- Open source, AGPL-3.0-only. Themes and blog content are not covered by the AGPL (theme exception, to be added to LICENSE notice before first release).
- One install serves one blog.
- Git holds code, default themes and guides. It never holds a user's data. Updating means
  `git pull` + `blog deploy`; a user never edits a tracked file, so a pull never conflicts.

## Principles (each one is enforced by `scripts/check` where a machine can check it)

0. **Reading and writing come first.** Every screen is judged first by how well it reads (or how good it feels to write in), then by UI/UX polish. Typography is tuned per font, book-like, never default browser values. Phones, tablets, desktops and foldables (folded and unfolded) are all first-class.
1. **Lightest and simplest that works.** Fixed budgets, see Budgets. New dependency needs a `# why` line in the workspace `Cargo.toml`.
2. **Fast and smooth.** PageSpeed 100 on mobile and desktop for every default theme. No layout shift. Animations use only `transform`/`opacity`, 150-250 ms, and stop under `prefers-reduced-motion`.
3. **English everywhere** in the repo: code, comments, paths, file names, guides. Only `locales/` and each theme's `locales/` hold other languages. Comments say why, not what.
4. **Built for AI operators.** If an AI agent can do a job by following a guide, plumblog ships a guide, not a feature.
5. **No drifting docs.** Fixed list of Markdown files (see Documentation). Anything a machine can generate from code is generated.
6. **Heavy work happens off the Worker.** The browser (admin) or the user's machine (CLI) does expensive work; the Worker validates, stores and serves.
7. **No hard-coded text.** Every user-facing string, English included, lives in a locale file (`locales/` for core and admin, the theme's own locale files for theme text). Code and templates only reference keys.
8. **No hard-coded layout.** Core produces data and semantic content; every reader-facing page structure (what appears, where, in which order) is defined by the theme's templates. Core ships no reader HTML layout of its own beyond the default theme.
9. **Small core, extensible edges.** Core contains only what a working blog needs. Themes and declarative plugins add the rest (see Plugins and options); complex needs use the API and webhooks from outside.

## Platform

| Need | Cloudflare service | Notes |
|---|---|---|
| Public site, admin API, MCP | One Worker (Rust, `workers-rs`, wasm) | 10 ms CPU per request on free plan |
| Posts, users, settings, sessions | D1 | FTS5 for search |
| Images, theme bundles | R2 | |
| Admin JS/CSS, default theme assets | Workers static assets | |
| Rendered pages | Cache API | Cache keys include a site content version that every publish bumps, so no purge calls. Cloudflare docs only promise working cache operations on custom domains; behaviour on `*.workers.dev` is unconfirmed and must be measured (issue #8). The blog works either way: without cache every view is rendered (about 1-2 ms CPU, more D1 reads). Free plan: cache calls share the 50-per-request subrequest budget |
| Scheduled publishing | Cron Trigger | |
| Spam protection | Turnstile | Login after failures, comments |
| Analytics | Cloudflare Web Analytics | Not built into admin; an AI guide fetches numbers |

Free-plan rules that shape the design:
- Markdown is rendered to HTML **on save**, stored, and cached. Reading a post never parses Markdown.
- Password hashing uses WebCrypto PBKDF2 (argon2 in wasm exceeds 10 ms).
- No per-view database writes.

## Code layout

One crate per module, each with a small public API, so a module can be fixed, upgraded or replaced without touching the others. Dependencies only point down the list; `scripts/check` enforces the allowed graph.

```
crates/model      shared types, ids, errors, permissions table; no I/O
crates/markdown   Markdown -> HTML, TOC, excerpt, heading ids (pulldown-cmark + own extensions)
crates/i18n       locale files, lookup, plural rules
crates/auth       password scheme, passkey verification, sessions, roles; crypto behind a trait
crates/syslog     system log event types and the logging interface
crates/store      storage traits (posts, users, media, settings, log...) + in-memory impl for tests
crates/render     theme runtime: minijinja templates, template data, theme/plugin options
crates/worker     wiring only: routes per feature (pages, api, mcp, media, cron), D1/R2/cache adapters
crates/cli        the `blog` binary: setup, deploy, content and theme commands, guides
guides/           AI guides, embedded into `blog guide` and exposed over MCP
locales/          core and admin UI strings, one TOML file per language
migrations/       D1 SQL, forward-only, numbered
themes/           default themes
plugins/          default plugins (declarative, see Plugins and options)
.claude/skills/   agent skills for workflows repeated 3+ times
web/admin         admin frontend
scripts/check     the one check; pre-commit hook and CI run it
```

Each feature (posts, media, auth, search, comments, webhooks, feeds...) is a module with the same shape: types in `model`, logic in its own crate or module, storage behind a `store` trait, one route module in `worker`. Logic never calls D1, R2 or `fetch` directly, so every module is testable without Cloudflare.

Users do not need Rust: releases ship the built Worker and `blog` binaries for macOS, Linux and Windows. Only contributors compile.

## Site-specific state

- `.env`: `CLOUDFLARE_API_TOKEN` (scoped: Workers, D1, R2; never the global key), `CLOUDFLARE_ACCOUNT_ID`.
- `site.local.toml`: resource names, domain. Ignored by git.
- `wrangler.toml` is generated by `blog` at deploy time; never committed.
- Everything else (site name, languages, menu, theme choice, colors) is stored in D1 and edited in the admin.

## Storage

- Schema: `migrations/`, forward-only and numbered; each index says in a comment which query it serves. Times are UTC milliseconds. Only hashes of session, token and invite secrets are stored (webhook signing secrets are the exception: they must be readable).
- `crates/store` holds the traits and an in-memory store; `crates/worker/src/d1/` is the only D1 code. One contract suite runs against both, so they cannot drift.
- Every multi-row write is one D1 batch (a transaction): a post save writes the row, its tags, its search text, a revision, an automatic slug redirect and the content-version bump together, or nothing.
- Ids that leave their row (URLs, translation groups, log entries) use AUTOINCREMENT, so a deleted id is never reused.
- Search: SQLite FTS5 with `unicode61 remove_diacritics 2`, which folds Vietnamese tone and horn marks; the app folds d-stroke to `d` itself because the tokenizer treats it as a separate letter. Results are newest first, so the in-memory store answers identically.
- Lists page by keyset (last row's sort value and id), never OFFSET: D1 bills every row read, including skipped ones.
- Queries per reader page (cache miss): settings 1, theme options 1, content version 1, plus the page itself: post 1 (with tags) + translations 1; listing 1; search 1. Reader pages never write.

## Users and auth

- Two roles. **owner**: everything. **writer**: write, publish, edit and delete any post, upload media, create own API tokens with writer scope. Cannot touch settings, themes, users or comments moderation.
- Password (PBKDF2 via WebCrypto) and passkeys (WebAuthn, verified with WebCrypto). Session cookie: HttpOnly, Secure, SameSite=Strict.
- First owner: `blog setup` prints a one-time setup code; the site refuses to create an owner without it.
- Recovery: `blog admin reset` (whoever holds the Cloudflare token owns the blog). No email recovery.
- Writers join through a one-time invite link the owner copies from the admin.
- Login rate limited; Turnstile after repeated failures. Audit log of sign-ins and privileged actions.

## Content

- Posts and pages; drafts, scheduled, revisions (author recorded), trash with restore.
- Tags and series; pinned posts; cover image; excerpt (written by the author, else cut from the text at a word boundary; the length is the theme option `excerpt_words`, default 55 words); auto table of contents; reading time.
- Translations: one row per language, linked by a translation group. URLs `/<lang>/<slug>`. Missing translation shows the original with a notice. Translation itself is done by the user's AI.
- Changing a slug creates a permanent redirect automatically.
- Author pages.

### Markdown

CommonMark + GFM (tables, task lists, strikethrough, autolinks), footnotes, callouts, math (rendered to MathML), code blocks, image captions, galleries, embeds (click-to-load facade, no third-party load until clicked).

**Raw HTML in Markdown is not allowed** (stripped). Revisit later only with a safe-tag allowlist.

Code highlighting runs on the reader page: a small deferred script (<= 8 KB gzip) loaded only on pages that contain code. Nothing is highlighted at save time, so posts from the admin, the CLI and MCP all look the same.

### Images

- Encoded in the browser at upload: three AVIF sizes plus one small JPEG fallback, using a Rust AVIF encoder compiled to wasm, loaded only when the user uploads. Runs off the main thread with progress.
- Original kept in R2 for future re-encoding.
- Always `width`/`height`, `srcset`, lazy below the fold.
- Social card image = cover image, else site default. No generated cards.

## Reader site

Home, pagination, tag/author/series pages, yearly archive, search (accent-insensitive; Vietnamese `d-stroke (U+0111)` normalised to `d`), related posts by tag, RSS/Atom/JSON Feed per language, sitemap, `robots.txt`, `llms.txt`, full SEO (meta, JSON-LD, hreflang, canonical), friendly 404 with suggestions, light/dark toggle.

Navigation feels instant without JS frameworks: Speculation Rules prefetch, cross-document View Transitions.

Comments: own system, off by default, Turnstile, moderation queue (owner).

Installable web apps: the reader site (scope `/`) and the admin (scope `/admin/`) each have their own manifest and icon, so both can sit on a home screen and are told apart by color. Blog icon = the owner's site icon (uploaded in Settings; default generated from the site name on the theme accent). Admin icon = the same mark on a contrasting dark plate, named "<Site> Admin". No offline reading for the reader site; the admin caches its own shell and keeps drafts offline.

Not built: offline reading, newsletter (later), reactions.

## Themes

- Three default themes: **literary** (serif, warm paper), **minimal** (sans, monochrome, one accent), **technical** (monospace accents). Each has light and dark.
- A theme is a folder, distributed as a zip:
  ```
  theme.toml     name, version, light + dark color tokens, fonts
  style.css      uses tokens only, no hard-coded colors
  templates/     optional minijinja overrides; missing files fall back to the default theme
  assets/        images, fonts, JS
  locales/       the theme's own UI text, one TOML per language; templates call t("section.key", name=value)
  ```
- Options: `theme.toml` declares `[options.<key>]` in the shape of "Plugins and options": `type` is one of the admin field types (`short_text`, `long_text`, `number`, `toggle`, `choice`, `color`, `image`, `list`), plus `default`, limits, `group`, `icon` and locale keys for `label`/`help`. Templates read `theme.options.<key>`; a `color` option names the token it overrides. A `choice` option may map each value to a CSS file (a **style**): only the selected file ships after `style.css`. Values listed in `draft` are not offered to owners.
- Slots: themes expose named empty points for plugin partials with `{{ slot("<name>") }}`: `head`, `body_start`, `body_end`, `before_post`, `after_post`, `footer`.
- Themes may contain JS. Installing a theme not made locally shows a warning.
- **minimal** is built first; literary and technical follow once it is approved.
- Fonts: pick the most readable font for the theme's role that covers Latin (incl. Vietnamese), Cyrillic and Greek; modify (subset) it if needed, within its license. Loading rules:
  - Self-hosted on the blog's own domain, never a font CDN.
  - Split into `unicode-range` subsets (latin, latin-ext, vietnamese, cyrillic, greek) so a page downloads only the scripts it uses. Variable weight axis only, trimmed to the weights used.
  - Preload only the body latin subset. `font-display: swap` with a metric-matched fallback (`size-adjust`, `ascent-override`...) so the swap causes zero layout shift.
  - CJK, Arabic, Thai and other scripts use system fonts.
- minimal uses **Inter** (v4, OFL): high x-height, screen-first, wide language coverage. Body 17px phone to 19px desktop (rem, so user font size still scales), line-height 1.7 (Vietnamese stacked diacritics), measure 32 x font size (~66 characters), near-black on white, off-white on near-black in dark mode, `cv05`/`cv08` to tell I/l/1 apart, balanced headings, no orphan words. Accent: plum.
  - Preload the body, title and mono latin subsets plus the page language's subsets (`preload_by_lang`), so their letters never swap in late and move the layout; mono is above the fold in the writer style (site name, dates), and preloading it keeps mobile Lighthouse at 100.
  - `scripts/subset-fonts` builds the woff2 files and `faces.css` from the pinned official release. Weight axis 400-700 (starting at the default saves ~20% per file, so dark mode softens text by color, not weight); optical size pinned to the text cut. Subsets never overlap, so a Vietnamese page fetches latin + vietnamese only. Fallback face is Arial with metrics matched to Inter.
  - Typography values are CSS custom properties at the top of `style.css`; the editor's writing column reuses them.
  - Renderer contract: color tokens are emitted once as `--c-<key>: light-dark(<light>, <dark>)`, so `color-scheme` (system, toggle, print) switches the palette without a second block; footnotes as `<sup class="fnref"><a href="#fn-N">` plus `<section class="footnotes">` with `<li id="fn-N">` and `a.fnback`; code blocks as `<div class="code" data-lang="rust"><pre><code>` with `tok-keyword|string|number|constant|function|type|builtin|property|attr|tag|punct|operator|comment` spans; wide tables wrapped in `<div class="table">`; task-list items as `<li><label><input type="checkbox" disabled> text</label></li>`; a `wide` class on a figure lets it bleed to the screen edges on phones.
  - Type scale: five sizes as steps of the body size (`--t-xs` .8125, `--t-sm` .875, body, `--t-md` 1.125, `--t-lg` 1.375, plus `--t-xl` for h1), never em multiples. Mono sits one step below the Inter beside it: same x-height, wider letters. Rows that mix fonts share a baseline.
  - Search: the header magnifier opens a search panel under the header (`/` also opens it, Esc closes); results appear as you type, read from the search page's own HTML, so core needs no second endpoint. Without JS the magnifier is a link to `search.html`, which gets `query` and `results` (post rows) and works as a plain GET form. Shown when `site.search_url` is set.
  - Comments: `post.html` gets `comments` only when comments are on: `open`, `items` (`id`, `author`, `owner`, `date_iso`, `date_display`, sanitized `html`), `action_url`, `turnstile_key`, `max_length`, `notice` (`pending` or `error`). Turnstile loads only when the reader focuses the form; the draft survives a failed send.
  - Titles use **Inter Display** (Inter's display optical size, one static weight, Latin scripts only; Cyrillic and Greek titles fall back to the text cut).
  - Styles: **writer** (default; text first like a writing app), **book** and **magazine** (drafts, offered once they reach the writer standard). Options: style, accent, list images (off), excerpt words (55), sidenotes (on).
  - writer home: one row per post, pinned first, in a grid with a narrow mono date column that hangs in the left margin on wide screens and moves under the excerpt on phones; title, excerpt, optional small 4:3 picture. Rows are separated by space, not rules.
  - Code and metadata use **JetBrains Mono** (OFL, one weight, no ligatures so readers see what is typed). No uppercase or small caps anywhere.
  - Shared tokens at the top of `style.css`: radii (`--r-sm` 4, `--r-md` 6, `--r-lg` 8), `--tap` 44px, `--ctl-sm`, spacing `--s-1`..`--s-7` (rem), shadows `--raised`, `--pressed`, `--inset`, `--pop`. Controls use only these; spacing inside running text stays in em so it follows the text size.
  - Controls follow the admin design language: one state system (hover, pressed, selected, focus ring, disabled) shared by the toggle, copy button, checkboxes and footnote buttons.
  - Sidenotes move into the margin from 76rem (~1216px) wide; narrower screens, folded or unfolded foldables included, get the inline toggle.
- Footnotes are sidenotes: in the margin beside the sentence on wide screens, an inline toggle under the sentence on narrow screens, classic bottom footnotes when JS is off. About 1 KB JS.
- AI workflow: `blog theme new | dev | check | push | rollback`. `check` reports template errors, missing dark tokens, contrast failures and size, each with file:line and a fix. Last 5 versions kept for rollback.
- The template variable reference is generated from Rust types, never written by hand.
- Owner-only admin page for colors, fonts, logo.

## Plugins and options

- Themes and plugins declare options; they never ship admin UI. A declaration lists fields from a fixed set of types owned by the admin (short text, long text, number, toggle, single choice, color, image, list), with labels and help as locale keys, grouping, and icons by name from the admin icon set. The admin renders every field with its own components, so every theme and plugin tab follows the admin design language.
- Settings: **Appearance** shows the active theme's options; **Plugins** lists plugins (enable/disable) with one tab per plugin, generated from its declaration.
- Plugins are declarative only: settings, HTML partials for named slots that themes must expose (head, body end, before/after post, footer...), browser JS/CSS, locale files. No server code inside the blog. A disabled plugin costs zero bytes.
- Complex needs are the user's own project: a separate Worker or service talking to plumblog through the API and outgoing webhooks (post published/updated/deleted). Core adds webhooks, nothing more.
- Isolation: no theme or plugin HTML, CSS or JS ever runs in the admin. Theme CSS needed in the admin (editor typography, live preview) loads in an isolated frame. Admin has a strict CSP. `blog theme check` and `blog plugin check` reject unknown field types, hard-coded text and admin-targeted files.
- A broken plugin is disabled automatically and named in the admin; the blog keeps working.

## Admin and editor

- Markdown source editor (CodeMirror 6) with live styling: syntax marks dimmed, headings and emphasis styled as typed. Modes: Write / Split / Preview (tabs on mobile).
- The writing column uses the active theme's font and line-height, so writing looks like the result, at a calmer size (see Design language).
- Toolbar that fades while typing; selection bubble (bold, italic, link); `/` menu for blocks.
- Paste or drop images: placeholder with progress, encode, insert, ask for alt text. Paste from Word/Google Docs converts to Markdown. Pasting a URL over a selection makes a link.
- Focus mode, typewriter scrolling, word count, discreet "Saved", offline draft in IndexedDB.
- Post settings in a collapsible side panel. Publish button always in the same place.
- Admin in light and dark; English and Vietnamese UI.
- Dashboard (drafts, recent posts, last backup time), media library, users/invites, settings, tokens, audit log, update notice from GitHub Releases.

The approved prototype (https://claude.ai/artifact/JEowACAfZPJ5VJydhFcgFt) is the visual reference for every admin screen. Its stylesheets live in `web/admin/css/`: `tokens.css`, `components.css` and `pages/*.css`, loaded in that order; the admin code builds on them rather than restyling. The rules they follow are in Design language below.

### Design language

Models: Bear and iA Writer for focus; Braun and Dieter Rams (Bauhaus) for the controls. Everything serves writing; the chrome is quiet and practical, and every control reads as a physical key you can press.

- **Type.** UI: system font. Scale 11 / 12 / 13 / 14 / 16 / 20 px, weights 400 / 500 / 600 only. Labels and metadata 12-13 px; section labels are 12 px, weight 600, muted, in normal case. No uppercase or small caps anywhere (scripts/check enforces it). Numbers, sizes and dates use tabular figures; file names use the mono font at 12 px. Writing column: the active theme's font and line-height, at a calmer size than the reader page (16 px phone, 17 px desktop, line-height 1.65, max 64ch); title 1.5em, weight 650.
- **Space.** 4 / 8 / 12 / 16 / 24 / 32 / 48 px. Rows are dense (list row 56-64 px) and separated by spacing and a soft hover or selected surface, never by full-width rules; whitespace goes around the writing column, not inside controls.
- **Color.** Off-white and warm greys, never pure grey; ink (near-black, near-white in dark) for primary actions. One accent, Braun signal orange (light `#c4561f`, dark `#e0804b`), only for caret, selection, links, the focus ring and lit lamps (a switch that is on, a latched key). Status colors are muted, Pantone-like: draft = warm grey, scheduled = ochre, published = sage, error = brick; text on a status tint keeps 4.5:1.
- **Shape.** Generous rounding: buttons, segmented controls, tags, status chips, toasts and the editor toolbar are pills; icon buttons, switch knobs, radios, lamps and status dots are circles; fields 10 px, menu items and nav items 9-10 px, list rows 12 px (0 on phones), popovers 14 px, checkboxes 6 px.
- **Depth, kept shallow.** Keys: a key face (`--key`) with a lit top edge and a 1-2 px cast shadow (`--sh-rest`). Wells: text fields, selects, switch slots and segmented tracks sit slightly recessed (`--well`, `--sh-well`). Raised and sunk must read at a glance but never look deep or heavy. Popovers, the floating toolbar and the phone bar cast a soft shadow; nothing else does. No gradients on control faces.
- **States, one system for every control.** rest = raised key (quiet icon buttons are flat until hovered); hover = the key lifts (bigger shadow, 1 px up), flat items get a 6% ink tint; pressed = the key sinks (inner shadow, 1 px down, darker well color); selected = the key stays latched down and its lamp lights in signal orange, while a selected flat item (nav, menu, row) becomes a raised key outlined on all four edges; focus-visible = 2 px signal ring outside a 2 px gap, never removed; disabled = 40% opacity, no shadow, no motion. Transitions 120 ms ease-out.
- **Icons.** One line set, 1.5 px stroke, round caps and joins, 16 px in lists and sidebars, 20 px in the editor toolbar; icon-only buttons are round, 32 px (36 px in the editor toolbar, 44 px on touch), with a tooltip and an accessible label. The editor toolbar is a contained pill centered on the writing column, never an edge-to-edge strip; what does not fit goes into an overflow menu.
- **Layout.** Page header 60 px, its middle level with the logo in the sidebar. Title left; actions right; search, where a page has it, is always the rightmost control, so it sits in the same place on every page. The account entry is an icon with name and role, no avatar. On phones the navigation (posts, pages, media, settings, account) is a floating glass pill above the bottom edge; the current item is a lighter lens with a signal-orange icon and label.
- **Media.** A grid of pictures with name and size below; a missing description shows as a lamp badge on the picture. Each picture has an eye button (on hover, always on touch) that opens a full-screen dark viewer with previous/next, arrows and Esc, and focus returns to the picture. Details sit in a card: the picture at its true proportions, then description, where it is used (links to posts), file size and actions. Filters: all, no description, unused.
- **Color swatches.** Flat discs of color with a 1 px rim and no shading; hover adds an outer line ring, the selected disc gets a 2 px ink ring outside a 2 px gap, focus the signal ring.
- **One stylesheet system.** `web/admin/css/tokens.css` holds every raw value: colors per theme, type roles, the space scale, control sizes (32 px, 44 px on touch), radii, shadows, durations, z-index. `components.css` defines each shared piece once: buttons, fields, switches, segmented controls, nav items, page header, filter row, list rows, sections, empty and loading states, popovers, toasts, the drawer, the toolbar, the viewer. Page files (`pages/*.css`) only lay out and compose; a page that needs a variant adds a modifier to the component. `scripts/check-admin-css` enforces it: no raw value outside tokens, no component restyled in a page file. Every screen is checked at 344, 375, 673, 768, 884 and 1440 px, light and dark, English and Vietnamese, including empty, long text, loading, error, offline and writer cases: no overflow or misaligned edges, text contrast 4.5:1, touch targets 44 px, a visible focus ring on every control.
- **Motion.** 120 ms for states, 180 ms for panels and popovers (opacity + 4 px translate), nothing while typing except the toolbar fade. Off under `prefers-reduced-motion`.
- **Never:** big rounded cards; soft drop shadows on everything; gradients on controls or swatches; glass except on the phone bar and on buttons over pictures; emoji or filled cartoon icons; everything centered; oversized hero text; colored icon tiles; decorative illustrations; more than one accent; status shown by color alone; uppercase or small-caps text; full-width rules between list rows; edge-to-edge toolbars.

## AI surface

- `blog` CLI: setup, deploy, and primitives (posts, media, settings, themes, tokens).
- HTTP API with generated reference.
- MCP endpoint on the Worker: a user can connect an AI client with the site URL and a token, without cloning.
- Guides (`guides/*.md`) embedded in `blog guide <topic>` and served over MCP. Every `blog ...` command in a guide is validated by a test.
- Done as guides, not features: import (WordPress, Ghost, Hugo/Jekyll/Zola), full export, backup, translation, unused-media cleanup, analytics.
- The admin shows "last backup" as recorded by the backup guide's final command, as a nudge.

## System log

- Owner-only page in Settings: a readable timeline of what the system did, with filters (type, user, date) and a request id on each entry.
- Recorded: sign-ins and failures, content changes (who, what), settings/theme/plugin changes, deploys and migrations, scheduled jobs, webhook deliveries, automatic plugin disables, errors with a plain message and an error code.
- Never recorded: page views, passwords, tokens, personal data beyond the acting user.
- Stored in D1 with a retention cap (age and row count) so it stays inside free-plan write limits; one write per event, never per view.
- Worker logs carry the same request id, so an AI can go from an admin entry to the full trace.

## Testing

| Layer | What it proves | Runs in |
|---|---|---|
| Unit | markdown, i18n, auth, permissions, render | `cargo test` |
| Fixtures | Markdown output byte-for-byte against a corpus | `cargo test` |
| Worker integration | every route, roles (writer cannot reach settings), publish flow, cache versioning | local `wrangler dev` |
| Browser | admin flows: write, paste image, publish | Playwright |
| Performance | Lighthouse 100 + CLS 0; render of a long post under a CPU budget | local |
| Migrations | applied to sample data, no rows lost | local |

Node.js is a contributor-only tool (wrangler, Playwright, Lighthouse). Users never need it.

## Budgets

| Item | Budget |
|---|---|
| Worker wasm, gzip | <= 1 MiB |
| Reader page JS | 0 KB by default, <= 5 KB with theme toggle and TOC |
| Reader page CSS (inlined) | <= 15 KB |
| Admin JS | measured every build, budget set when the editor lands |
| Lighthouse (mobile + desktop, sample content) | 100, CLS 0 |

## Documentation

Which Markdown files may exist, and how long each may be, is enforced by `scripts/check` (the list lives there, not here). `README.md` is for people; `AGENTS.md` and `guides/` are for AI agents. Generated references (CLI, API, theme variables, settings) come from code.

## Roadmap

Work is tracked as GitHub Issues grouped in one milestone per phase; this list only names the phases. One issue = one session = one PR; `scripts/check` and CI green, reviewed, then merged. Each phase ends with a demo Hung can click through.

0. Design prototypes merged (minimal theme, admin prototype).
1. Foundations: module crates, shared types, error and log conventions, dependency-graph check, local Worker test harness.
2. In parallel: markdown + i18n; storage + migrations + `blog setup`/`blog deploy`; reader rendering (theme runtime, cache, minimal templates).
3. Auth, API, system log; real admin and editor; media upload (AVIF).
4. MCP, theme options and plugins, search, comments, webhooks, feeds and SEO.
5. Release pipeline, guides, first real deploy on a custom domain, production measurements.

## Decision log

- 2026-11-27 Rust on Cloudflare free plan; data only on Cloudflare; git holds code and guides only.
- 2026-11-27 Own auth (password + passkey), not Cloudflare Access. Roles: owner, writer.
- 2026-11-27 AGPL-3.0-only. Independent of any other project.
- 2026-11-27 No layout variants; themes define light and dark. Three default themes. Themes may ship JS.
- 2026-11-27 Editor: Markdown source with toolbar, live styling, paste/drop images.
- 2026-11-27 Images: AVIF encoded in the browser.
- 2026-11-27 Import, export, backup, translation, analytics: AI guides, not features. Analytics via Cloudflare Web Analytics.
- 2026-11-27 Comments built, off by default. Email/newsletter deferred.
- 2026-11-27 Raw HTML in Markdown forbidden for now.
- 2026-11-27 Docs: guides and AGENTS.md short with no repetition (line caps in check); README in plain, honest human voice.
- 2026-11-27 Reading and writing first; book-like typography tuned per font; foldables first-class.
- 2026-11-27 minimal theme first, font Inter with unicode-range subsets; footnotes as sidenotes.
- 2026-11-27 No hard-coded text (all languages incl. English in locale files); no hard-coded layout (themes define page structure); small core, features via themes and plugins.
- 2026-11-27 minimal: Inter weight axis 400-700 and text optical size only; dark mode softens text by color instead of a lighter weight; plum accent.
- 2026-11-27 minimal v0.2 after owner review: Inter Display titles, syntax colors, drawn checkboxes, home with excerpts and year groups; color tokens via `light-dark()`.
- 2026-11-27 Reversed 'no layout variants': themes may declare options incl. styles; minimal ships writer first, book and magazine later at the same standard.
- 2026-11-27 Admin design language: Bear / iA Writer as models, flat with slight depth on controls, one state system, muted status palette.
- 2026-11-27 Admin controls follow Braun / Dieter Rams: raised keys that lift, sink and latch with a lamp; pills and circles; signal orange as the one accent.
- 2026-11-27 Admin depth kept shallow; search always rightmost in the page header; phone navigation is a floating glass pill; media gets a full-screen viewer.
- 2026-11-27 Admin CSS is one system: raw values only in tokens.css, each component defined once in components.css, pages only compose; enforced by scripts/check-admin-css.
- 2026-11-27 Theme and plugin options are declarations rendered by the admin; plugins are declarative only; complex needs use API + webhooks; nothing from themes/plugins runs in the admin.
- 2026-11-27 Reader site and admin are installable web apps with distinct manifests and icon colors; no offline reading.
- 2026-11-27 One crate per module with an enforced dependency graph; logic is storage-agnostic behind store traits.
- 2026-11-27 Code highlighting on the reader page (deferred, <= 8 KB, only pages with code); nothing at save time.
- 2026-11-27 Blog works on `*.workers.dev`; Cache API guaranteed only on custom domains (workers.dev to be measured). Cache invalidation by content version.
- 2026-11-27 Owner-facing system log in Settings; Node.js is contributor-only.
- 2026-11-27 Worker integration tests: Node tools pinned in `tools/` (keeps repo root Rust-only), real wasm via pinned worker-build under `wrangler dev` with a dev-only `tools/wrangler.dev.toml`; run by `scripts/worker-test`, part of `scripts/check`.
- 2026-11-27 Storage: D1 schema v1, store traits with in-memory and D1 implementations checked by one contract suite; atomic batches; FTS5 search with app-side d-stroke folding; keyset paging.
