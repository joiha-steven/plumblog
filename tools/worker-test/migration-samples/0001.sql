-- Sample data valid right after migrations/0001_init.sql: at least one row in every table,
-- shaped like a small real blog. tools/worker-test/migrations.test.mjs loads it, applies every
-- later migration and checks that no table lost rows.
-- Vietnamese text is built with char(N), the character with Unicode code point N, because repo
-- files must be ASCII (e.g. char(272) is D-stroke, char(273) d-stroke, char(432) u-horn).
-- Values in posts_fts are folded like the app does (d-stroke -> d), so search finds them.

UPDATE site SET content_version = 42 WHERE id = 1;

INSERT INTO users (id, login, slug, display_name, bio, role, password_hash, disabled_at, created_at, updated_at) VALUES
  (1, 'owner', 'lan', 'Nguy' || char(7877) || 'n Lan', 'Vi' || char(7871) || 't v' || char(7873) || ' ' || char(273) || 'i' || char(7873) || 'u nh' || char(7887), 'owner', 'scrypt$sample$hash', NULL, 1700000000000, 1700000000000),
  (2, 'Writer', 'minh', 'Tr' || char(7847) || 'n Minh', '', 'writer', NULL, NULL, 1700000100000, 1700000200000);

INSERT INTO sessions (id, token_hash, user_id, created_at, expires_at) VALUES
  (1, '9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08', 1, 1700000300000, 1702592300000);

INSERT INTO passkeys (id, user_id, credential_id, public_key, sign_count, name, created_at, last_used_at) VALUES
  (1, 1, 'AQIDBAUGBwgJCgsMDQ4PEA', 'pQECAyYgASFYIA-sample-key', 7, 'MacBook', 1700000400000, 1700000500000);

INSERT INTO api_tokens (id, user_id, name, token_hash, scope, created_at, expires_at, revoked_at) VALUES
  (1, 1, 'MCP', '60303ae22b998861bce3b28f33eec1be758a213c86c93c076dbe9f558c11c752', 'owner', 1700000600000, NULL, NULL),
  (2, 2, 'old laptop', 'fd61a03af4f77d870fc21e05e7e80678095c92d808cfb3b5c279ee04c74aca13', 'writer', 1700000700000, 1731536700000, 1700000800000);

INSERT INTO invites (id, code_hash, role, created_by, created_at, expires_at, used_at, used_by) VALUES
  (1, 'a665a45920422f9d417e4867efdc4fb8a04a1f3fff1fa07e998e86f7f7a27ae3', 'writer', 1, 1700000050000, 1700604850000, 1700000100000, 2),
  (2, 'b3a8e0e1f9ab1bfe3a36f231f676f78bb30a519d2b21e6c530c0eee8ebb4a5d0', 'writer', 1, 1700000900000, 1700605700000, NULL, NULL);

INSERT INTO media (id, r2_key, mime, bytes, width, height, alt, variants_json, uploaded_by, created_at) VALUES
  (1, 'media/2023/11/da-lat.jpg', 'image/jpeg', 482113, 2400, 1600, 'H' || char(7891) || ' Xu' || char(226) || 'n H' || char(432) || char(417) || 'ng', '[{"key":"media/2023/11/da-lat-800.avif","width":800,"height":533,"bytes":40211,"mime":"image/avif"}]', 1, 1700001000000),
  (2, 'media/2023/11/cafe.png', 'image/png', 90311, 1200, 800, '', '[]', 2, 1700001100000);

INSERT INTO series (id, slug, title, description) VALUES
  (1, 'tay-nguyen', 'T' || char(226) || 'y Nguy' || char(234) || 'n', 'Ghi ch' || char(233) || 'p ' || char(273) || char(432) || char(7901) || 'ng ' || char(273) || char(7845) || 't ' || char(273) || char(7887));

INSERT INTO tags (id, slug, name) VALUES
  (1, 'du-lich', 'Du l' || char(7883) || 'ch'),
  (2, 'ca-phe', 'C' || char(224) || ' ph' || char(234));

INSERT INTO posts (id, kind, translation_group, lang, slug, status, trashed_from, published_at, author_id, title, markdown, html, toc_json, excerpt, word_count, reading_minutes, pinned, comments_enabled, cover_media_id, series_id, series_position, created_at, updated_at) VALUES
  (1, 'post', 1, 'vi', 'da-lat-mua-mua', 'published', NULL, 1700002000000, 1,
   char(272) || char(224) || ' L' || char(7841) || 't m' || char(249) || 'a m' || char(432) || 'a',
   '# ' || char(272) || char(224) || ' L' || char(7841) || 't' || char(10) || char(10) || 'Con ' || char(273) || char(432) || char(7901) || 'ng d' || char(7889) || 'c.',
   '<h1 id="da-lat">' || char(272) || char(224) || ' L' || char(7841) || 't</h1><p>Con ' || char(273) || char(432) || char(7901) || 'ng d' || char(7889) || 'c.</p>',
   '[{"level":1,"id":"da-lat","text":"Da Lat"}]', '', 320, 2, 1, 1, 1, 1, 1, 1700001900000, 1700002000000),
  (2, 'post', 1, 'en', 'da-lat-rainy-season', 'scheduled', NULL, 1800000000000, 2,
   'Da Lat in the rainy season', '# Da Lat', '<h1 id="da-lat">Da Lat</h1>', '[]', 'Rain, pines, coffee.', 300, 2, 0, 1, NULL, 1, 2, 1700002100000, 1700002200000),
  (3, 'page', 3, 'vi', 'gioi-thieu', 'trashed', 'draft', NULL, 1,
   'Gi' || char(7899) || 'i thi' || char(7879) || 'u', 'Xin ch' || char(224) || 'o.', '<p>Xin ch' || char(224) || 'o.</p>', '[]', '', 2, 1, 0, 0, NULL, NULL, NULL, 1700002300000, 1700002400000);

INSERT INTO post_tags (post_id, tag_id) VALUES (1, 1), (1, 2), (2, 1);

INSERT INTO post_media (post_id, media_id) VALUES (1, 2), (2, 1);

INSERT INTO revisions (id, post_id, author_id, title, markdown, created_at) VALUES
  (1, 1, 1, char(272) || char(224) || ' L' || char(7841) || 't', '# ' || char(272) || char(224) || ' L' || char(7841) || 't', 1700001900000),
  (2, 1, 2, char(272) || char(224) || ' L' || char(7841) || 't m' || char(249) || 'a m' || char(432) || 'a', '# ' || char(272) || char(224) || ' L' || char(7841) || 't', 1700002000000);

INSERT INTO redirects (from_path, to_path, created_at) VALUES
  ('/vi/da-lat', '/vi/da-lat-mua-mua', 1700002000000);

INSERT INTO comments (id, post_id, parent_id, author_name, body, status, created_at) VALUES
  (1, 1, NULL, 'H' || char(432) || char(417) || 'ng', 'B' || char(224) || 'i vi' || char(7871) || 't hay qu' || char(225) || '!', 'approved', 1700003000000),
  (2, 1, 1, 'Lan', 'C' || char(7843) || 'm ' || char(417) || 'n b' || char(7841) || 'n.', 'pending', 1700003100000);

INSERT INTO settings (key, value_json, updated_at) VALUES
  ('site.name', '"Nh' || char(7853) || 't k' || char(253) || ' Lan"', 1700000000000),
  ('site.languages', '["vi","en"]', 1700000000000);

INSERT INTO extension_options (owner_kind, owner_id, key, value_json, updated_at) VALUES
  ('theme', 'minimal', 'accent', '"#7a4cc2"', 1700000000000),
  ('plugin', 'reading-time', 'words_per_minute', '200', 1700000000000);

INSERT INTO plugins (id, enabled, disabled_reason, updated_at) VALUES
  ('reading-time', 1, NULL, 1700000000000),
  ('broken-embed', 0, 'render.timeout', 1700000000000);

INSERT INTO webhooks (id, url, events, secret, enabled, created_at) VALUES
  (1, 'https://hooks.example.com/plumblog', 'post.published,post.updated', 'whsec_sample', 1, 1700004000000);

INSERT INTO system_log (id, at_ms, request_id, kind, actor_id, error_code, detail) VALUES
  (1, 1700000300000, '8a1b2c3d4e5f6a7b-SIN', 'auth.sign_in', 1, NULL, 'owner'),
  (2, 1700002000000, '8a1b2c3d4e5f6a7c-SIN', 'content.changed', 1, NULL, '/vi/da-lat-mua-mua'),
  (3, 1700005000000, 'cron-1700005000000', 'job.run', NULL, 'store.failed', 'publish_due');

INSERT INTO posts_fts (rowid, title, body) VALUES
  (1, 'Da L' || char(7841) || 't m' || char(249) || 'a m' || char(432) || 'a', 'Con d' || char(432) || char(7901) || 'ng d' || char(7889) || 'c.'),
  (2, 'Da Lat in the rainy season', 'Rain, pines, coffee.'),
  (3, 'Gi' || char(7899) || 'i thi' || char(7879) || 'u', 'Xin ch' || char(224) || 'o.');
