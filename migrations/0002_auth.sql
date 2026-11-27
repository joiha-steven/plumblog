-- plumblog schema v2: sign-in rate limits and WebAuthn challenges (issue #9).
-- Conventions as in 0001_init.sql. Both tables hold only hashes: never a raw login, IP or
-- challenge. Rows are short-lived and cleaned lazily by the app (no cron).

-- Failed sign-ins per key in a fixed window. `key_hash` is the SHA-256 hex of
-- "login:" + lowercase login or "ip:" + IP (IPv6 /64), computed by the caller.
-- One write per failed attempt; a failure after the window ended restarts it at 1.
CREATE TABLE login_failures (
  key_hash     TEXT    PRIMARY KEY,                     -- every sign-in: read the keys of this attempt
  count        INTEGER NOT NULL CHECK (count >= 1),
  window_start INTEGER NOT NULL
) WITHOUT ROWID;

-- Single-use WebAuthn challenges (SHA-256 hex of the challenge), valid until expires_at.
CREATE TABLE auth_challenges (
  challenge_hash TEXT    PRIMARY KEY,                   -- passkey ceremony: take the challenge by hash
  purpose        TEXT    NOT NULL CHECK (purpose IN ('register', 'login')),
  user_id        INTEGER REFERENCES users (id) ON DELETE CASCADE, -- NULL for a login challenge
  expires_at     INTEGER NOT NULL
) WITHOUT ROWID;
-- Lazy cleanup: DELETE ... WHERE expires_at <= now.
CREATE INDEX auth_challenges_expiry ON auth_challenges (expires_at);
-- Deleting a user cascades to their challenges (FK action needs this index).
CREATE INDEX auth_challenges_user ON auth_challenges (user_id);
