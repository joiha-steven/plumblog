//! The SQL of the reader reads (and of the admin and export listings), free of the Worker
//! runtime so a test can run `EXPLAIN QUERY PLAN` on every statement against real SQLite with
//! the migrations applied. Each doc line names the index the query is meant to use; the test
//! fails when the plan drifts.

use plumblog_store::{ReaderFilter, year_bounds};

use super::comments::COMMENT_COLS;
use super::posts::{POST_COLS, SUMMARY_COLS};
use super::users::USER_COLS;

/// Tags of `p` as JSON `[[id, slug, name], ...]`: names may hold any character, so no
/// separator-joined list. JSON order is not relied on; `read_tags` sorts by id.
const TAGS_COL: &str = "(SELECT json_group_array(json_array(t.id, t.slug, t.name)) \
     FROM post_tags pt JOIN tags t ON t.id = pt.tag_id WHERE pt.post_id = p.id)";

/// The post's series slug and title, NULL without one: its primary key, once, on the post page.
const SERIES_COLS: &str = "(SELECT s.slug FROM series s WHERE s.id = p.series_id), \
     (SELECT s.title FROM series s WHERE s.id = p.series_id)";

/// Every reader listing: published posts, never pages.
pub(super) const LISTED: &str = "p.status = 'published' AND p.kind = 'post'";

/// FROM and WHERE of a listing. A tag drives from `post_tags_tag` (CROSS JOIN fixes the join
/// order), so a tag page reads its own posts, not every published post. `None` for a year
/// outside 1..=9999.
pub(super) fn listing_from(filter: ReaderFilter<'_>) -> Option<String> {
    Some(match filter {
        ReaderFilter::Home(_) | ReaderFilter::Lang(_) => {
            format!("posts p WHERE {LISTED} AND p.lang = ?")
        }
        ReaderFilter::Tag(..) => format!(
            "post_tags pt CROSS JOIN posts p ON p.id = pt.post_id \
             WHERE pt.tag_id = ? AND {LISTED} AND p.lang = ?"
        ),
        // Named: with `status`, `kind` and `lang` all equal the planner may take
        // `posts_listing` and read the whole language instead of the series.
        ReaderFilter::Series(..) => format!(
            "posts p INDEXED BY posts_series \
             WHERE p.series_id = ? AND {LISTED} AND p.lang = ?"
        ),
        ReaderFilter::Year(y) => {
            year_bounds(y)?;
            format!("posts p WHERE {LISTED} AND p.published_at >= ? AND p.published_at < ?")
        }
    })
}

/// The count of a listing: a covering index (`posts_home`, `posts_by_date`, `post_tags_tag`).
pub(super) fn count(from: &str) -> String {
    format!("SELECT COUNT(*) FROM {from}")
}

/// Posts per status (admin list pills), any status. A covering scan of `posts_listing`
/// (status, kind, lang): grouped by its first column, so no sort and no table read. Named
/// explicitly: without it the planner takes `posts_admin` (no filter) or `posts_lang_slug`
/// (lang, then a TEMP B-TREE for the GROUP BY).
pub(super) fn post_counts(kind: bool, lang: bool) -> String {
    let mut wh = Vec::new();
    if kind {
        wh.push("kind = ?");
    }
    if lang {
        wh.push("lang = ?");
    }
    let wh = if wh.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", wh.join(" AND "))
    };
    format!("SELECT status, COUNT(*) FROM posts INDEXED BY posts_listing{wh} GROUP BY status")
}

/// One page of a listing. The inner query pages over the index alone (ids and sort keys);
/// only the page's rows are then read in full, with their tags.
pub(super) fn rows(from: &str, all: bool) -> String {
    let (keys, inner_order, outer_order) = if all {
        (
            "p.id, p.pinned, p.published_at",
            "p.pinned DESC, p.published_at DESC, p.id DESC",
            "k.pinned DESC, k.published_at DESC, k.id DESC",
        )
    } else {
        (
            "p.id, p.published_at",
            "p.published_at DESC, p.id DESC",
            "k.published_at DESC, k.id DESC",
        )
    };
    format!(
        "SELECT {SUMMARY_COLS}, {TAGS_COL} FROM \
         (SELECT {keys} FROM {from} ORDER BY {inner_order} LIMIT ? OFFSET ?) k \
         CROSS JOIN posts p ON p.id = k.id ORDER BY {outer_order}"
    )
}

/// One page of a series: reading order. The index gives `series_position` (after the
/// `series_id` equality); the date and id tie-breaks sort the rows of one series in a TEMP
/// B-TREE, bounded by the series' size and unavoidable without an index on the date.
pub(super) fn rows_series(from: &str) -> String {
    format!(
        "SELECT {SUMMARY_COLS}, {TAGS_COL} FROM \
         (SELECT p.id, p.series_position, p.published_at FROM {from} \
          ORDER BY p.series_position, p.published_at, p.id LIMIT ? OFFSET ?) k \
         CROSS JOIN posts p ON p.id = k.id \
         ORDER BY k.series_position, k.published_at, k.id"
    )
}

/// A post page: `posts_lang_slug`, then the tags through the `post_tags` primary key, the
/// series through the `series` primary key and the cover through the `media` primary key
/// (LEFT JOIN: no cover is all NULL).
pub(super) fn post() -> String {
    format!(
        "SELECT {POST_COLS}, {TAGS_COL}, {SERIES_COLS}, \
         m.id, m.r2_key, m.width, m.height, m.alt, m.variants_json \
         FROM posts p LEFT JOIN media m ON m.id = p.cover_media_id \
         WHERE p.lang = ? AND p.slug = ? AND p.status = 'published'"
    )
}

/// A translation group: `posts_group_lang`, named explicitly because without ANALYZE the
/// planner prefers `posts_admin` (status) and reads every published row.
pub(super) const TRANSLATIONS: &str = "SELECT lang, slug, title FROM posts \
     INDEXED BY posts_group_lang WHERE translation_group = ? AND status = 'published' \
     ORDER BY lang";

/// Both neighbours: two LIMIT 1 range reads on `posts_listing` (lang, then date).
pub(super) fn neighbours() -> String {
    let side = |cmp: &str, order: &str, tag: &str| {
        format!(
            "SELECT * FROM (SELECT '{tag}', p.lang, p.slug, p.title \
             FROM posts me JOIN posts p ON {LISTED} AND p.lang = me.lang \
             AND (p.published_at, p.id) {cmp} (me.published_at, me.id) \
             WHERE me.id = ? AND me.status = 'published' AND me.kind = 'post' \
             ORDER BY p.published_at {order}, p.id {order} LIMIT 1)"
        )
    };
    format!(
        "{} UNION ALL {}",
        side("<", "DESC", "older"),
        side(">", "ASC", "newer")
    )
}

/// Years with a post: a covering scan of `posts_by_date`. '%Y' of ms / 1000.0 is the UTC year,
/// agreeing with `plumblog_store::year_of` at the Jan 1 edges (contract case). Posts outside
/// years 1..=9999 are left out (strftime answers NULL there), as `ReaderFilter::Year` does.
/// The DISTINCT/ORDER BY run on the few years, so a TEMP B-TREE here is small by design.
pub(super) fn years() -> String {
    format!(
        "SELECT DISTINCT CAST(strftime('%Y', p.published_at / 1000.0, 'unixepoch') \
         AS INTEGER) AS y FROM posts p WHERE {LISTED} \
         AND p.published_at >= ? AND p.published_at < ? ORDER BY y DESC"
    )
}

/// Feed: the newest posts of one language, bodies and tags included, on `posts_listing`.
pub(super) fn feed() -> String {
    format!(
        "SELECT {POST_COLS}, {TAGS_COL} FROM posts p WHERE {LISTED} AND p.lang = ? \
         ORDER BY p.published_at DESC, p.id DESC LIMIT ?"
    )
}

/// Sitemap: the newest posts and the newest pages, each an ordered read of `posts_by_date`
/// (one `kind` per arm, so no sort); the caller merges them by (`published_at`, id).
pub(super) fn sitemap() -> String {
    let arm = |kind: &str| {
        format!(
            "SELECT * FROM (SELECT p.lang, p.slug, p.updated_at, p.translation_group, \
             p.published_at, p.id, p.kind = 'page' FROM posts p WHERE p.status = 'published' \
             AND p.kind = '{kind}' ORDER BY p.published_at DESC, p.id DESC LIMIT ?)"
        )
    };
    format!("{} UNION ALL {}", arm("post"), arm("page"))
}

/// Reader search: FTS5 `MATCH` first, then each hit's post by its integer primary key, with
/// the status and language filter on that row. CROSS JOIN fixes the order: without it the
/// planner (no ANALYZE on D1) drives from `posts_admin` and runs one MATCH per published post.
/// The only TEMP B-TREE is the sort of the hits
/// (a wide query is cut to `LIMIT` after it); there is no index on `published_at` to avoid it.
pub(super) fn search() -> String {
    format!(
        "SELECT {SUMMARY_COLS} FROM posts_fts CROSS JOIN posts p ON p.id = posts_fts.rowid \
         WHERE posts_fts MATCH ? AND p.status = 'published' AND p.lang = ? \
         ORDER BY p.published_at DESC, p.id DESC LIMIT ?"
    )
}

/// A published post or page at `(lang, slug)`: the unique `posts_lang_slug` lookup, no columns.
pub(super) fn post_exists() -> String {
    "SELECT 1 FROM posts p WHERE p.lang = ? AND p.slug = ? AND p.status = 'published' LIMIT 1"
        .to_owned()
}

/// Which of `n` languages have a published post: one `EXISTS` each on `posts_home`
/// (status, kind, lang), a covering index search that stops at the first row.
pub(super) fn langs_with_posts(n: usize) -> String {
    let values = vec!["SELECT ? AS v"; n].join(" UNION ALL ");
    format!(
        "SELECT l.v FROM ({values}) l WHERE EXISTS (SELECT 1 FROM posts p \
         WHERE {LISTED} AND p.lang = l.v)"
    )
}

/// `(SELECT ? AS v, 0 AS o) UNION ALL ...`: `n` languages, `o` keeping the caller's order.
fn lang_values(n: usize) -> String {
    (0..n)
        .map(|i| format!("SELECT ? AS v, {i} AS o"))
        .collect::<Vec<_>>()
        .join(" UNION ALL ")
}

/// Tag pages that exist: each (tag, language) stops at its first published post
/// (`post_tags_tag`, then the post). Binds the `n` languages. `tags` is read whole; the
/// sort is of the tags.
pub(super) fn tag_pages(n: usize) -> String {
    format!(
        "SELECT t.slug, t.name, l.v FROM tags t CROSS JOIN ({}) l WHERE EXISTS (SELECT 1 \
         FROM post_tags pt CROSS JOIN posts p ON p.id = pt.post_id \
         WHERE pt.tag_id = t.id AND {LISTED} AND p.lang = l.v) ORDER BY t.name, t.id, l.o",
        lang_values(n)
    )
}

/// Series pages that exist: each (series, language) stops at its first published post
/// (`posts_series`). Binds the `n` languages.
pub(super) fn series_pages(n: usize) -> String {
    format!(
        "SELECT s.slug, s.title, l.v FROM series s CROSS JOIN ({}) l WHERE EXISTS (SELECT 1 \
         FROM posts p INDEXED BY posts_series WHERE p.series_id = s.id AND {LISTED} \
         AND p.lang = l.v) ORDER BY s.title, s.id, l.o",
        lang_values(n)
    )
}

/// Which of `n` languages (bound first) have a post with tag `?` (bound last): one `EXISTS`
/// each on `post_tags_tag`.
pub(super) fn tag_langs(n: usize) -> String {
    format!(
        "SELECT l.v FROM ({}) l WHERE EXISTS (SELECT 1 \
         FROM post_tags pt CROSS JOIN posts p ON p.id = pt.post_id \
         WHERE pt.tag_id = ? AND {LISTED} AND p.lang = l.v) ORDER BY l.o",
        lang_values(n)
    )
}

/// As [`tag_langs`] for series `?`, on `posts_series`.
pub(super) fn series_langs(n: usize) -> String {
    format!(
        "SELECT l.v FROM ({}) l WHERE EXISTS (SELECT 1 \
         FROM posts p INDEXED BY posts_series WHERE p.series_id = ? AND {LISTED} \
         AND p.lang = l.v) ORDER BY l.o",
        lang_values(n)
    )
}

/// A page of tags or series by slug: `slug > ?` then `LIMIT ?` walk the slug's unique
/// autoindex in order, with no sort and no scan. The first page binds `''`, which sorts
/// before every slug (a slug is never empty).
pub(super) fn taxonomy_page(table: &str, cols: &str) -> String {
    format!("SELECT {cols} FROM {table} WHERE slug > ? ORDER BY slug LIMIT ?")
}

/// Every comment by id after a cursor id (0 = from the start), for export: a range of the
/// primary key, so a page reads its own rows plus one, however many are spam. No status
/// filter: `status` leads no index that also orders by id.
pub(super) fn comments_by_id() -> String {
    format!("SELECT {COMMENT_COLS} FROM comments WHERE id > ? ORDER BY id LIMIT ?")
}

/// Every user by id after a cursor id (0 = from the start): a range of the primary key.
pub(super) fn users_by_id() -> String {
    format!("SELECT {USER_COLS} FROM users WHERE id > ? ORDER BY id LIMIT ?")
}

#[cfg(test)]
mod plans {
    use super::*;
    use plumblog_model::{SeriesId, TagId};
    use rusqlite::{Connection, params_from_iter, types::Value};

    /// The migrations in order, in real SQLite, with no rows and no ANALYZE: the planner
    /// knows only the schema, like a fresh D1.
    fn db() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        for sql in [
            include_str!("../../../../migrations/0001_init.sql"),
            include_str!("../../../../migrations/0002_auth.sql"),
            include_str!("../../../../migrations/0003_webhook_deliveries.sql"),
            include_str!("../../../../migrations/0004_comment_html.sql"),
            include_str!("../../../../migrations/0005_token_read_scope.sql"),
            include_str!("../../../../migrations/0006_reader_indexes.sql"),
            include_str!("../../../../migrations/0007_media_sha256.sql"),
            include_str!("../../../../migrations/0008_post_client_id.sql"),
        ] {
            c.execute_batch(sql).unwrap();
        }
        // Rows, so the plan is the one a site with posts gets (no ANALYZE: D1 has none to
        // offer): 2,000 published posts in 1,000 two-language groups, some pages and drafts.
        c.execute_batch(
            "INSERT INTO users (id, login, slug, display_name, role, created_at, updated_at) \
             VALUES (1, 'u', 'u', 'U', 'owner', 0, 0); \
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 2000) \
             INSERT INTO posts (kind, translation_group, lang, slug, status, published_at, \
             author_id, title, markdown, html, created_at, updated_at) \
             SELECT CASE WHEN i % 50 = 0 THEN 'page' ELSE 'post' END, (i + 1) / 2, \
             CASE WHEN i % 2 = 0 THEN 'vi' ELSE 'en' END, 's' || i, \
             CASE WHEN i % 10 = 0 THEN 'draft' ELSE 'published' END, i * 1000, 1, 't', 'm', 'h', \
             i, i FROM n;",
        )
        .unwrap();
        c
    }

    /// The `detail` lines of `EXPLAIN QUERY PLAN <sql>`.
    fn plan(c: &Connection, sql: &str) -> Vec<String> {
        let n = sql.matches('?').count();
        let mut st = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let args = std::iter::repeat_n(Value::Integer(1), n);
        st.query_map(params_from_iter(args), |r| r.get::<_, String>(3))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    /// `want`: every fragment must appear in the plan (an index name, `COVERING`).
    /// `temp`: whether a TEMP B-TREE is expected (a sort or DISTINCT on a small set).
    fn check(c: &Connection, name: &str, sql: &str, want: &[&str], temp: bool) {
        check_scans(c, name, sql, want, temp, &[]);
    }

    /// `check`, allowing a full scan of the named tables (`tags` is read whole, by design).
    fn check_scans(
        c: &Connection,
        name: &str,
        sql: &str,
        want: &[&str],
        temp: bool,
        scans: &[&str],
    ) {
        let lines = plan(c, sql);
        let all = lines.join("\n");
        for w in want {
            assert!(all.contains(w), "{name}: plan lacks {w:?}:\n{all}");
        }
        assert_eq!(
            all.contains("TEMP B-TREE"),
            temp,
            "{name}: TEMP B-TREE:\n{all}"
        );
        // A scan of `posts` or `tags` itself is a full table read; only the page's own row
        // keys (the `k` co-routine) and LIMIT 1 subqueries are scanned in full.
        for l in &lines {
            assert!(
                !(l.starts_with("SCAN")
                    && !l.contains("INDEX")
                    && l != "SCAN k"
                    && !scans.iter().any(|t| l == &format!("SCAN {t}"))
                    && !l.starts_with("SCAN (subquery")),
                "{name}: full scan {l:?}:\n{all}"
            );
        }
    }

    /// `find_client_post` (d1/posts.rs) and the partial unique index behind the create replay.
    #[test]
    fn client_id_lookup_uses_its_index_and_is_unique_per_author() {
        let c = db();
        check(
            &c,
            "find_client_post",
            "SELECT id FROM posts WHERE author_id = ? AND client_id = ?",
            &["posts_author_client"],
            false,
        );
        c.execute_batch(
            "INSERT INTO users (id, login, slug, display_name, role, created_at, updated_at) \
             VALUES (2, 'v', 'v', 'V', 'writer', 0, 0); \
             INSERT INTO posts (kind, translation_group, lang, slug, status, author_id, title, \
             markdown, html, created_at, updated_at, client_id) \
             VALUES ('post', 5001, 'en', 'k1', 'draft', 1, 't', 'm', 'h', 0, 0, 'key'), \
             ('post', 5002, 'fr', 'k2', 'draft', 2, 't', 'm', 'h', 0, 0, 'key');",
        )
        .unwrap();
        let again = c.execute(
            "INSERT INTO posts (kind, translation_group, lang, slug, status, author_id, title, \
             markdown, html, created_at, updated_at, client_id) \
             VALUES ('post', 5003, 'de', 'k3', 'draft', 1, 't', 'm', 'h', 0, 0, 'key')",
            [],
        );
        assert!(
            again
                .unwrap_err()
                .to_string()
                .contains("UNIQUE constraint failed"),
            "one author, one key"
        );
    }

    /// A language home (`/vi/`) is the same statements as `/` with another bound language.
    #[test]
    fn a_language_home_uses_the_home_index() {
        let c = db();
        let home = listing_from(ReaderFilter::Home("en")).unwrap();
        let vi_home = listing_from(ReaderFilter::Home("vi")).unwrap();
        assert_eq!(vi_home, home, "one SQL text for every language's home");
        check(
            &c,
            "rows vi home",
            &rows(&vi_home, true),
            &["posts_home"],
            false,
        );
        check(
            &c,
            "count vi home",
            &count(&vi_home),
            &["COVERING INDEX posts_"],
            false,
        );
    }

    /// The language-home helpers: a point lookup, and one covering `EXISTS` per language.
    /// Search reads the FTS5 hits, then their posts by primary key: no scan of `posts`.
    #[test]
    fn search_matches_first_then_reads_posts_by_primary_key() {
        let c = db();
        let lines = plan(&c, &search());
        let all = lines.join("\n");
        assert_eq!(
            lines[0], "SCAN posts_fts VIRTUAL TABLE INDEX 0:M2",
            "MATCH first:\n{all}"
        );
        assert!(
            all.contains("SEARCH p USING INTEGER PRIMARY KEY (rowid=?)"),
            "posts by rowid:\n{all}"
        );
        assert!(
            !lines.iter().any(|l| l == "SCAN p"),
            "no scan of posts:\n{all}"
        );
        assert_eq!(
            lines.iter().filter(|l| l.contains("TEMP B-TREE")).count(),
            1,
            "one sort of the hits:\n{all}"
        );
    }

    #[test]
    fn language_home_helpers_use_their_indexes() {
        let c = db();
        check(
            &c,
            "post exists",
            &post_exists(),
            &["posts_lang_slug"],
            false,
        );
        for n in [1, 3] {
            let sql = langs_with_posts(n);
            assert_eq!(sql.matches('?').count(), n);
            // Only the language list itself (constants) is scanned; posts are searched.
            check_scans(
                &c,
                "langs with posts",
                &sql,
                &[
                    "SEARCH p EXISTS USING COVERING INDEX posts_home (status=? AND kind=? AND lang=?)",
                ],
                false,
                &["CONSTANT ROW", "l"],
            );
        }
    }

    #[test]
    fn every_reader_query_uses_the_index_it_is_meant_to() {
        let c = db();
        let home = listing_from(ReaderFilter::Home("en")).unwrap();
        let tag = listing_from(ReaderFilter::Tag(TagId(1), "en")).unwrap();
        let year = listing_from(ReaderFilter::Year(2024)).unwrap();
        let lang = listing_from(ReaderFilter::Lang("en")).unwrap();

        // Counts read an index alone.
        check(
            &c,
            "count home",
            &count(&home),
            &["COVERING INDEX posts_"],
            false,
        );
        check(&c, "count tag", &count(&tag), &["post_tags_tag"], false);
        check(
            &c,
            "count year",
            &count(&year),
            &["COVERING INDEX posts_by_date"],
            false,
        );
        check(
            &c,
            "count lang",
            &count(&lang),
            &["COVERING INDEX posts_"],
            false,
        );

        // The admin's status counts: an index alone, grouped without a sort.
        for (kind, lang) in [(false, false), (true, false), (false, true), (true, true)] {
            check(
                &c,
                &format!("post counts kind={kind} lang={lang}"),
                &post_counts(kind, lang),
                &["COVERING INDEX posts_listing"],
                false,
            );
        }

        // Pages walk the index in order: no sort.
        check(&c, "rows home", &rows(&home, true), &["posts_home"], false);
        check(&c, "rows tag", &rows(&tag, false), &["post_tags_tag"], true);
        check(
            &c,
            "rows year",
            &rows(&year, false),
            &["posts_by_date"],
            false,
        );
        check(
            &c,
            "rows lang",
            &rows(&lang, false),
            &["posts_listing"],
            false,
        );

        // The cover and the series are primary-key lookups, not scans of `media` or `series`.
        check(
            &c,
            "post",
            &post(),
            &[
                "posts_lang_slug",
                "SEARCH m USING INTEGER PRIMARY KEY",
                "SEARCH s USING INTEGER PRIMARY KEY",
            ],
            false,
        );
        // The D1 engine chose posts_admin (status) here and read every published row; the
        // SQLite bundled for tests does not, so the pin itself is asserted as well.
        assert!(TRANSLATIONS.contains("INDEXED BY posts_group_lang"));
        check(
            &c,
            "translations",
            TRANSLATIONS,
            &["posts_group_lang"],
            false,
        );
        check(&c, "neighbours", &neighbours(), &["posts_listing"], false);
        check(
            &c,
            "years",
            &years(),
            &["COVERING INDEX posts_by_date"],
            true,
        );
        check(&c, "feed", &feed(), &["posts_listing"], false);
        // The sitemap also reads each row's kind (language homes skip pages): same plan.
        assert!(sitemap().contains("p.kind = 'page'"));
        check(&c, "sitemap", &sitemap(), &["posts_by_date"], false);

        // API pages of tags and series: a SEARCH on the slug's unique autoindex, no sort.
        for (table, cols, index) in [
            ("tags", "id, slug, name", "sqlite_autoindex_tags_1"),
            (
                "series",
                "id, slug, title, description",
                "sqlite_autoindex_series_1",
            ),
        ] {
            check(
                &c,
                &format!("{table} page"),
                &taxonomy_page(table, cols),
                &[&format!("SEARCH {table} USING INDEX {index} (slug>?)")],
                false,
            );
        }
    }

    /// Series pages and the per-language tag and series lists (issue #132).
    #[test]
    fn series_and_per_language_queries_use_their_indexes() {
        let c = db();
        let series = listing_from(ReaderFilter::Series(SeriesId(1), "en")).unwrap();
        check(
            &c,
            "count series",
            &count(&series),
            &["posts_series (series_id=?)"],
            false,
        );
        // One series, read by posts_series: only the date and id tie-breaks sort it.
        check(
            &c,
            "rows series",
            &rows_series(&series),
            &["posts_series (series_id=?)"],
            true,
        );
        // Per-language pages and hreflang sets: one EXISTS per (taxon, language), stopping at
        // the first post. The taxon tables are read whole, like `tags`; the language list is a
        // handful of constant rows.
        for (name, sql, want, scans) in [
            (
                "tag pages",
                tag_pages(3),
                "post_tags_tag",
                &["t", "l", "CONSTANT ROW"][..],
            ),
            (
                "series pages",
                series_pages(3),
                "posts_series (series_id=?)",
                &["s", "l", "CONSTANT ROW"][..],
            ),
        ] {
            check_scans(&c, name, &sql, &[want], true, scans);
        }
        check_scans(
            &c,
            "tag langs",
            &tag_langs(3),
            &["post_tags_tag"],
            true,
            &["l", "CONSTANT ROW"],
        );
        check_scans(
            &c,
            "series langs",
            &series_langs(3),
            &["posts_series (series_id=?)"],
            true,
            &["l", "CONSTANT ROW"],
        );
    }

    /// The sitemap's tag and series lookups on a large site (10,000 posts, 200 tags with 5 per
    /// post, 50 series (20 used), 3 languages): prints the time of each query (`--nocapture`); no
    /// wall-clock gate (#57), only that every page that should exist is found.
    #[test]
    fn taxon_sitemap_queries_at_ten_thousand_posts() {
        let c = db();
        c.execute_batch(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 8000) \
             INSERT INTO posts (kind, translation_group, lang, slug, status, published_at, \
             author_id, title, markdown, html, created_at, updated_at) \
             SELECT 'post', 100000 + i, CASE i % 3 WHEN 0 THEN 'en' WHEN 1 THEN 'vi' ELSE 'fr' END, \
             'b' || i, 'published', i * 1000, 1, 't', 'm', 'h', i, i FROM n; \
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 200) \
             INSERT INTO tags (id, slug, name) SELECT i, 't' || i, 'Tag ' || i FROM n; \
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 50) \
             INSERT INTO series (id, slug, title) SELECT i, 's' || i, 'Series ' || i FROM n; \
             WITH RECURSIVE k(k) AS (SELECT 0 UNION ALL SELECT k + 1 FROM k WHERE k < 4) \
             INSERT INTO post_tags (post_id, tag_id) SELECT p.id, (p.id + k * 37) % 200 + 1 \
             FROM posts p CROSS JOIN k; \
             UPDATE posts SET series_id = id % 50 + 1, series_position = id WHERE id % 5 < 2;",
        )
        .unwrap();
        let langs = ["en", "vi", "fr"];
        let run = |name: &str, sql: &str, extra: Option<i64>| {
            let mut st = c.prepare(sql).unwrap();
            let mut args: Vec<Value> = langs.iter().map(|l| Value::Text((*l).to_owned())).collect();
            args.extend(extra.map(Value::Integer));
            let t = std::time::Instant::now();
            let n = st
                .query_map(params_from_iter(args), |_| Ok(()))
                .unwrap()
                .count();
            eprintln!("sitemap query {name}: {n} rows in {:?}", t.elapsed());
            n
        };
        assert_eq!(run("tag pages", &tag_pages(3), None), 600);
        assert_eq!(run("series pages", &series_pages(3), None), 60);
        assert_eq!(run("tag langs", &tag_langs(3), Some(1)), 3);
        assert_eq!(run("series langs", &series_langs(3), Some(1)), 3);
    }

    /// The export listings: a primary-key range read in key order, nothing else.
    #[test]
    fn export_listings_are_primary_key_ranges() {
        let c = db();
        for (name, sql, table) in [
            ("comments by id", comments_by_id(), "comments"),
            ("users by id", users_by_id(), "users"),
        ] {
            check(&c, name, &sql, &[], false);
            assert_eq!(
                plan(&c, &sql),
                [format!(
                    "SEARCH {table} USING INTEGER PRIMARY KEY (rowid>?)"
                )],
                "{name}"
            );
        }
    }
}
