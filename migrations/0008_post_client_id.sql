-- plumblog schema v8: a client request id on posts, so a create retried after a lost answer
-- returns the post the first request made instead of a second one (issue #86,
-- `SavePost::client_id`). Per author: the UNIQUE index is on (author_id, client_id). A
-- partial index: posts without an id (every post from before, every API create without one)
-- take no entry, and NULLs never collide anyway.
--
-- Existing rows: the new column is NULL for all of them; no row is read, changed or dropped.
-- Re-running: wrangler and `blog` record applied files and never run one twice; run by hand
-- a second time, the ALTER fails ("duplicate column name") before anything else, so the
-- schema stays as it is. Rollback: DROP INDEX posts_author_client; ALTER TABLE posts DROP
-- COLUMN client_id; a Worker from before never names the column.
--
-- Cost: one more index entry per post created with an id.
ALTER TABLE posts ADD COLUMN client_id TEXT;
CREATE UNIQUE INDEX IF NOT EXISTS posts_author_client ON posts (author_id, client_id)
  WHERE client_id IS NOT NULL;
