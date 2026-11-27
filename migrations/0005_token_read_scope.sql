-- plumblog schema v5: API tokens get a read-only scope (issue #10).
-- SQLite cannot change a CHECK in place, so api_tokens is rebuilt: same columns, same ids
-- (INTEGER PRIMARY KEY = rowid, copied explicitly), same UNIQUE on token_hash, same index.
-- Every row is copied as is; nothing is dropped until the copy exists.
--
-- Foreign keys: D1 always enforces them. No table, trigger or view references api_tokens, so
-- dropping it deletes no other row; the new table references users exactly like the old one,
-- and every copied user_id already satisfied that reference. Deferring the check to the end
-- of the migration's transaction, as D1 documents for table rebuilds, costs nothing here.
--
-- Re-running is safe: api_tokens_new exists only between CREATE and RENAME of one run, so a
-- second run rebuilds the table again with the same rows. Plain CREATE (not IF NOT EXISTS)
-- makes a leftover api_tokens_new fail loudly instead of copying rows twice.
--
-- Rollback: rebuild the same way with the 0001 CHECK ('owner', 'writer') after revoking and
-- deleting read tokens; the old CHECK refuses them otherwise.

PRAGMA defer_foreign_keys = true;

CREATE TABLE api_tokens_new (
  id         INTEGER PRIMARY KEY,
  user_id    INTEGER NOT NULL REFERENCES users (id) ON DELETE CASCADE,
  name       TEXT    NOT NULL,
  token_hash TEXT    NOT NULL UNIQUE,                   -- every API call: lookup by hash
  scope      TEXT    NOT NULL CHECK (scope IN ('read', 'writer', 'owner')),
  created_at INTEGER NOT NULL,
  expires_at INTEGER,                                   -- NULL = no expiry
  revoked_at INTEGER                                    -- NULL = active; kept for the audit trail
);

INSERT INTO api_tokens_new (id, user_id, name, token_hash, scope, created_at, expires_at, revoked_at)
  SELECT id, user_id, name, token_hash, scope, created_at, expires_at, revoked_at FROM api_tokens;

DROP TABLE api_tokens;
ALTER TABLE api_tokens_new RENAME TO api_tokens;

-- Account page: list a user's tokens; deleting a user cascades through it.
CREATE INDEX api_tokens_user ON api_tokens (user_id);
