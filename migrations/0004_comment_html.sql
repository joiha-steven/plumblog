-- Comment HTML rendered once, when the reader submits (plumblog_markdown::render_comment).
-- Page builds then never parse Markdown, and a later renderer change does not silently
-- rewrite comments already shown. NULL for rows from before this migration: the page
-- renders those from `body` when it is built. Nothing is backfilled; no rows change.
ALTER TABLE comments ADD COLUMN body_html TEXT;
