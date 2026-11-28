-- plumblog schema v6: indexes for the reader listings (issue #76, crates/store/src/reader.rs).
-- posts_listing (status, kind, lang, published_at) serves one language by date, but not the
-- home page (pinned first) or the lists that span every language: with `lang` in between, a
-- date order or a year range scans every published post and sorts it in a temp b-tree. Each index ends with the rowid
-- implicitly, so `published_at DESC, id DESC` is index order too.
--
-- Only indexes are added; no row changes. IF NOT EXISTS makes a re-run a no-op. Rollback:
-- DROP INDEX both; no Worker depends on them for correctness, only for cheaper reads.
--
-- Cost: every post INSERT, and every UPDATE that sets status, kind, pinned or published_at
-- (each save, trash, restore, scheduled publish), writes one more index entry per index.

-- Home and /page/N (ReaderFilter::Home): published posts of kind post in one language,
-- pinned first, newest first. The COUNT for the same filter reads the same index without
-- touching the table.
CREATE INDEX IF NOT EXISTS posts_home ON posts (status, kind, lang, pinned, published_at);
-- Yearly archive (ReaderFilter::Year: a published_at range), tag pages by date
-- (ReaderFilter::Tag) and the reader's year list: published posts of kind post by date,
-- every language.
CREATE INDEX IF NOT EXISTS posts_by_date ON posts (status, kind, published_at);
