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
        ReaderFilter::Tag(_) => format!(
            "post_tags pt CROSS JOIN posts p ON p.id = pt.post_id \
             WHERE pt.tag_id = ? AND {LISTED}"
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

/// A post page: `posts_lang_slug`, then the tags through the `post_tags` primary key.
pub(super) fn post() -> String {
    format!(
        "SELECT {POST_COLS}, {TAGS_COL} FROM posts p \
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
             p.published_at, p.id FROM posts p WHERE p.status = 'published' \
             AND p.kind = '{kind}' ORDER BY p.published_at DESC, p.id DESC LIMIT ?)"
        )
    };
    format!("{} UNION ALL {}", arm("post"), arm("page"))
}

/// Tags with a published post: each tag stops at its first one (`post_tags_tag`, then the post).
pub(super) fn tags() -> String {
    format!(
        "SELECT t.id, t.slug, t.name FROM tags t WHERE EXISTS (SELECT 1 \
         FROM post_tags pt CROSS JOIN posts p ON p.id = pt.post_id \
         WHERE pt.tag_id = t.id AND {LISTED}) ORDER BY t.name, t.id"
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
    use plumblog_model::TagId;
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

    #[test]
    fn every_reader_query_uses_the_index_it_is_meant_to() {
        let c = db();
        let home = listing_from(ReaderFilter::Home("en")).unwrap();
        let tag = listing_from(ReaderFilter::Tag(TagId(1))).unwrap();
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

        check(&c, "post", &post(), &["posts_lang_slug"], false);
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
        check_scans(&c, "tags", &tags(), &["post_tags_tag"], true, &["t"]);
        check(&c, "feed", &feed(), &["posts_listing"], false);
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
