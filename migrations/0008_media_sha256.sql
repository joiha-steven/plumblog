-- plumblog schema v7: content-hash dedupe of media uploads (issue #60).
-- `sha256` is the lowercase hex SHA-256 of the original bytes, as the client sent it with the
-- upload and R2 verified it. An upload whose (uploader, sha256) already exists answers that
-- item instead of storing the bytes again, so a retried upload makes no second copy.
--
-- Existing rows keep every value and get sha256 NULL: they are never matched. SQLite treats
-- NULLs as distinct in a UNIQUE index, so old rows, uploads sent without a hash and media of a
-- deleted uploader (uploaded_by NULL) never collide.
--
-- Compatibility: the previous Worker never names the column, so its inserts leave it NULL.
--
-- Re-running: SQLite has no ADD COLUMN IF NOT EXISTS, so a second run fails at the ALTER
-- ("duplicate column name") before changing anything; the index is IF NOT EXISTS.
-- Rollback: DROP INDEX media_uploader_sha256, then ALTER TABLE media DROP COLUMN sha256 (only
-- once no Worker that reads it runs); no other row depends on either.
--
-- Cost: one more index entry per media INSERT (and per uploader cleared on a user delete).

ALTER TABLE media ADD COLUMN sha256 TEXT;

-- Upload dedupe: lookup by (caller, sha256); the uniqueness also settles two identical
-- uploads at once (the loser reads the winner's row).
CREATE UNIQUE INDEX IF NOT EXISTS media_uploader_sha256 ON media (uploaded_by, sha256);
