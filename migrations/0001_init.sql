-- plumblog schema v1. Forward-only: never edit a shipped migration; add 0002_*.sql instead.
-- Conventions:
--   * Times are INTEGER milliseconds since the Unix epoch, UTC (`*_at`).
--   * Ids are INTEGER PRIMARY KEY (the rowid). Tables whose ids leave the row (URLs, caches,
--     translation groups, log entries) add AUTOINCREMENT so a deleted id is never reused:
--     a reused post id would silently join a dead translation group.
--   * Booleans are INTEGER 0/1 with a CHECK.
--   * Secrets are never stored, only their SHA-256 hex hash (`*_hash`), except webhook
--     signing secrets, which must be readable to sign deliveries.
--   * JSON is stored as TEXT and only parsed by the app; the store never looks inside.
--   * Every index names the query it serves. No index exists without a query.
-- D1 enforces foreign keys; ON DELETE rules below are part of the contract that the
-- in-memory store mirrors (crates/store, contract tests).

-- Site-wide counters. Exactly one row. `content_version` is part of every page cache key;
-- bumping it is how a publish invalidates cached pages without purge calls.
CREATE TABLE site (
  id              INTEGER PRIMARY KEY CHECK (id = 1),
  content_version INTEGER NOT NULL DEFAULT 1
);
INSERT INTO site (id, content_version) VALUES (1, 1);

-- People who can sign in. No email: recovery is `blog admin reset`.
CREATE TABLE users (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  login         TEXT    NOT NULL COLLATE NOCASE UNIQUE, -- sign-in name; lookup by login uses this unique index
  slug          TEXT    NOT NULL UNIQUE,                -- author page URL; lookup by slug uses this unique index
  display_name  TEXT    NOT NULL,
  bio           TEXT    NOT NULL DEFAULT '',
  role          TEXT    NOT NULL CHECK (role IN ('owner', 'writer')),
  password_hash TEXT,                                   -- NULL for passkey-only accounts; format owned by crates/auth
  disabled_at   INTEGER,                                -- NULL = active; disabled users keep their posts
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL
);

-- Signed-in browsers. Only the hash of the cookie token is stored.
CREATE TABLE sessions (
  id         INTEGER PRIMARY KEY,
  token_hash TEXT    NOT NULL UNIQUE,                   -- every authenticated request: lookup by hash
  user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL
);
-- "Sign out everywhere" and deleting a user's sessions when disabled.
CREATE INDEX sessions_user ON sessions (user_id);
-- Scheduled cleanup: DELETE ... WHERE expires_at < now.
CREATE INDEX sessions_expiry ON sessions (expires_at);

-- WebAuthn credentials.
CREATE TABLE passkeys (
  id            INTEGER PRIMARY KEY,
  user_id       INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  credential_id TEXT    NOT NULL UNIQUE,                -- sign-in: lookup by credential id (base64url)
  public_key    TEXT    NOT NULL,                       -- COSE key, base64url
  sign_count    INTEGER NOT NULL DEFAULT 0,
  name          TEXT    NOT NULL DEFAULT '',
  created_at    INTEGER NOT NULL,
  last_used_at  INTEGER
);
-- Account page: list a user's passkeys.
CREATE INDEX passkeys_user ON passkeys (user_id);

-- API and MCP tokens. Scope is a role; a writer can only hold writer scope (enforced in auth).
CREATE TABLE api_tokens (
  id         INTEGER PRIMARY KEY,
  user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  name       TEXT    NOT NULL,
  token_hash TEXT    NOT NULL UNIQUE,                   -- every API call: lookup by hash
  scope      TEXT    NOT NULL CHECK (scope IN ('owner', 'writer')),
  created_at INTEGER NOT NULL,
  expires_at INTEGER,                                   -- NULL = no expiry
  revoked_at INTEGER                                    -- NULL = active; kept for the audit trail
);
-- Account page: list a user's tokens.
CREATE INDEX api_tokens_user ON api_tokens (user_id);

-- One-time invite links for writers (and owners).
CREATE TABLE invites (
  id         INTEGER PRIMARY KEY,
  code_hash  TEXT    NOT NULL UNIQUE,                   -- accepting an invite: lookup by hash
  role       TEXT    NOT NULL CHECK (role IN ('owner', 'writer')),
  created_by INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  used_at    INTEGER,
  used_by    INTEGER REFERENCES users (id) ON DELETE SET NULL
);
-- Users page: list pending invites, newest first.
CREATE INDEX invites_created ON invites (created_at);
-- Deleting a user: cascade to the invites they created (FK action needs this index).
CREATE INDEX invites_creator ON invites (created_by);
-- Deleting a user: clear used_by on the invite they accepted (FK action needs this index).
CREATE INDEX invites_used_by ON invites (used_by);

-- Uploaded images. Bytes live in R2; this is the catalogue.
CREATE TABLE media (
  id            INTEGER PRIMARY KEY AUTOINCREMENT,
  r2_key        TEXT    NOT NULL UNIQUE,                -- the original; also guards against double inserts
  mime          TEXT    NOT NULL,
  bytes         INTEGER NOT NULL CHECK (bytes >= 0),
  width         INTEGER NOT NULL CHECK (width > 0),
  height        INTEGER NOT NULL CHECK (height > 0),
  alt           TEXT    NOT NULL DEFAULT '',            -- '' = missing description (media filter)
  variants_json TEXT    NOT NULL DEFAULT '[]',          -- encoded sizes: [{key, width, height, bytes, mime}]
  uploaded_by   INTEGER REFERENCES users (id) ON DELETE SET NULL,
  created_at    INTEGER NOT NULL
);
-- Media library grid, newest first, paged by (created_at, id).
CREATE INDEX media_created ON media (created_at, id);
-- Deleting a user: clear uploaded_by on their media (FK action needs this index).
CREATE INDEX media_uploader ON media (uploaded_by);

CREATE TABLE series (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  slug        TEXT    NOT NULL UNIQUE,                  -- series page: lookup by slug
  title       TEXT    NOT NULL,
  description TEXT    NOT NULL DEFAULT ''
);

CREATE TABLE tags (
  id   INTEGER PRIMARY KEY AUTOINCREMENT,
  slug TEXT    NOT NULL UNIQUE,                         -- tag page: lookup by slug
  name TEXT    NOT NULL
);

-- Posts and pages. One row per language; translations share `translation_group`.
CREATE TABLE posts (
  id                INTEGER PRIMARY KEY AUTOINCREMENT,
  kind              TEXT    NOT NULL CHECK (kind IN ('post', 'page')),
  translation_group INTEGER NOT NULL,                   -- id of the first post in the group
  lang              TEXT    NOT NULL,
  slug              TEXT    NOT NULL,
  status            TEXT    NOT NULL CHECK (status IN ('draft', 'scheduled', 'published', 'trashed')),
  trashed_from      TEXT    CHECK (trashed_from IN ('draft', 'scheduled', 'published')), -- restore target
  published_at      INTEGER,                            -- set for scheduled and published
  author_id         INTEGER NOT NULL REFERENCES users (id) ON DELETE RESTRICT, -- reassign posts before deleting a user
  title             TEXT    NOT NULL,
  markdown          TEXT    NOT NULL,
  html              TEXT    NOT NULL,                   -- rendered on save; reading never parses Markdown
  toc_json          TEXT    NOT NULL DEFAULT '[]',
  excerpt           TEXT    NOT NULL DEFAULT '',        -- author-written; '' = cut from text by the theme
  word_count        INTEGER NOT NULL DEFAULT 0,
  reading_minutes   INTEGER NOT NULL DEFAULT 0,
  pinned            INTEGER NOT NULL DEFAULT 0 CHECK (pinned IN (0, 1)),
  comments_enabled  INTEGER NOT NULL DEFAULT 1 CHECK (comments_enabled IN (0, 1)),
  cover_media_id    INTEGER REFERENCES media (id) ON DELETE RESTRICT, -- media in use cannot be deleted
  series_id         INTEGER REFERENCES series (id) ON DELETE SET NULL,
  series_position   INTEGER,
  created_at        INTEGER NOT NULL,
  updated_at        INTEGER NOT NULL,
  version           INTEGER NOT NULL DEFAULT 1 CHECK (version >= 1), -- optimistic lock: every save must name the version it edited
  CHECK ((status = 'trashed') = (trashed_from IS NOT NULL)),
  CHECK (status NOT IN ('scheduled', 'published') OR published_at IS NOT NULL)
);
-- Reader URL /<lang>/<slug>: lookup by (lang, slug); a slug is unique per language.
CREATE UNIQUE INDEX posts_lang_slug ON posts (lang, slug);
-- Translation switcher and hreflang: all languages of one group; one row per language.
CREATE UNIQUE INDEX posts_group_lang ON posts (translation_group, lang);
-- Home, pagination, yearly archive, feeds, sitemap: published posts of a kind in one
-- language, newest first. Also the cron query for due scheduled posts (status, published_at).
CREATE INDEX posts_listing ON posts (status, kind, lang, published_at);
-- Author page: an author's published posts, newest first.
CREATE INDEX posts_author ON posts (author_id, status, published_at);
-- Series page: posts in reading order.
CREATE INDEX posts_series ON posts (series_id, series_position);
-- Admin post list: by status, most recently edited first.
CREATE INDEX posts_admin ON posts (status, updated_at);
-- Deleting a media item: refuse while a post uses it as cover (FK check needs this index).
CREATE INDEX posts_cover ON posts (cover_media_id);

CREATE TABLE post_tags (
  post_id INTEGER NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
  tag_id  INTEGER NOT NULL REFERENCES tags (id) ON DELETE CASCADE,
  PRIMARY KEY (post_id, tag_id)                         -- tags of a post
) WITHOUT ROWID;
-- Tag page and related posts: posts with a tag.
CREATE INDEX post_tags_tag ON post_tags (tag_id, post_id);

-- Media used in a post body (images, galleries), written on every save from the rendered
-- post. Media still used here or as a cover cannot be deleted.
CREATE TABLE post_media (
  post_id  INTEGER NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
  media_id INTEGER NOT NULL REFERENCES media (id) ON DELETE RESTRICT,
  PRIMARY KEY (post_id, media_id)                       -- media of a post (replaced on save)
) WITHOUT ROWID;
-- Media details "used in" list, the unused filter, and the delete check.
CREATE INDEX post_media_media ON post_media (media_id, post_id);

-- Snapshots of the title and text, with who saved them; written only when a save asks for one, so the newest snapshot may be older than the post.
CREATE TABLE revisions (
  id         INTEGER PRIMARY KEY AUTOINCREMENT,
  post_id    INTEGER NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
  author_id  INTEGER REFERENCES users (id) ON DELETE SET NULL,
  title      TEXT    NOT NULL,
  markdown   TEXT    NOT NULL,
  created_at INTEGER NOT NULL
);
-- Revision list of a post, newest first.
CREATE INDEX revisions_post ON revisions (post_id, created_at);
-- Deleting a user: clear author_id on their revisions (FK action needs this index).
CREATE INDEX revisions_author ON revisions (author_id);

-- Old URL -> new URL, created automatically when a slug changes. Paths start with '/'.
CREATE TABLE redirects (
  from_path  TEXT    PRIMARY KEY,                       -- before a 404: lookup by path
  to_path    TEXT    NOT NULL,
  created_at INTEGER NOT NULL,
  CHECK (from_path <> to_path)
) WITHOUT ROWID;
-- Collapsing chains: when /b moves to /c, rewrite every redirect that points to /b.
CREATE INDEX redirects_to ON redirects (to_path);

-- Reader comments. Only a display name is kept: no email, no IP.
CREATE TABLE comments (
  id          INTEGER PRIMARY KEY AUTOINCREMENT,
  post_id     INTEGER NOT NULL REFERENCES posts (id) ON DELETE CASCADE,
  parent_id   INTEGER REFERENCES comments (id) ON DELETE CASCADE,
  author_name TEXT    NOT NULL,
  body        TEXT    NOT NULL,
  status      TEXT    NOT NULL CHECK (status IN ('pending', 'approved', 'spam', 'trashed')),
  created_at  INTEGER NOT NULL
);
-- Post page: approved comments of a post, oldest first.
CREATE INDEX comments_post ON comments (post_id, status, created_at);
-- Moderation queue: pending comments, oldest first.
CREATE INDEX comments_status ON comments (status, created_at);
-- Deleting a comment cascades to replies (FK action needs this index).
CREATE INDEX comments_parent ON comments (parent_id);

-- Site settings edited in the admin (name, languages, menu, theme choice...).
CREATE TABLE settings (
  key        TEXT    PRIMARY KEY,                       -- every page: read all settings in one query
  value_json TEXT    NOT NULL,
  updated_at INTEGER NOT NULL
) WITHOUT ROWID;

-- Options declared by themes and plugins, as edited in Settings.
CREATE TABLE extension_options (
  owner_kind TEXT    NOT NULL CHECK (owner_kind IN ('theme', 'plugin')),
  owner_id   TEXT    NOT NULL,                          -- theme or plugin id
  key        TEXT    NOT NULL,
  value_json TEXT    NOT NULL,
  updated_at INTEGER NOT NULL,
  PRIMARY KEY (owner_kind, owner_id, key)               -- render: all options of one theme or plugin
) WITHOUT ROWID;

-- Installed plugins: enabled flag and why a broken plugin was disabled automatically.
CREATE TABLE plugins (
  id              TEXT    PRIMARY KEY,
  enabled         INTEGER NOT NULL DEFAULT 0 CHECK (enabled IN (0, 1)),
  disabled_reason TEXT,                                 -- NULL unless disabled automatically
  updated_at      INTEGER NOT NULL
) WITHOUT ROWID;

-- Outgoing webhooks (post published/updated/deleted).
CREATE TABLE webhooks (
  id          INTEGER PRIMARY KEY,
  url         TEXT    NOT NULL,
  events      TEXT    NOT NULL,                         -- comma-separated event codes
  secret      TEXT    NOT NULL,                         -- HMAC signing key; readable by design (see header)
  enabled     INTEGER NOT NULL DEFAULT 1 CHECK (enabled IN (0, 1)),
  created_at  INTEGER NOT NULL
);

-- Owner timeline. One row per event, never per view; pruned by age and count.
CREATE TABLE system_log (
  id         INTEGER PRIMARY KEY AUTOINCREMENT, -- monotonic, so the timeline cursor never revisits an id
  at_ms      INTEGER NOT NULL,
  request_id TEXT    NOT NULL,
  kind       TEXT    NOT NULL,                          -- syslog::EventKind code
  actor_id   INTEGER REFERENCES users (id) ON DELETE SET NULL,
  error_code TEXT,                                      -- model::ErrorCode code
  detail     TEXT    NOT NULL DEFAULT ''
);
-- Timeline newest first, date filter, and retention (DELETE WHERE at_ms < cutoff).
CREATE INDEX system_log_at ON system_log (at_ms);
-- Type filter.
CREATE INDEX system_log_kind ON system_log (kind, at_ms);
-- User filter.
CREATE INDEX system_log_actor ON system_log (actor_id, at_ms);

-- Search text of every post, rowid = posts.id. Reader search joins posts for status and
-- language. Kept by the app in the same batch as the post, not by triggers: the app folds
-- d-stroke (U+0111/U+0110) to 'd' before indexing and querying, because unicode61 removes
-- tone and horn marks but treats d-stroke as its own letter.
CREATE VIRTUAL TABLE posts_fts USING fts5 (
  title,
  body,
  tokenize = 'unicode61 remove_diacritics 2'
);
