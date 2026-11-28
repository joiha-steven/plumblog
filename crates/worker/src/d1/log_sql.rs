//! The SQL of `query_log`, free of the Worker runtime so a test can run `EXPLAIN QUERY PLAN`
//! on it against real SQLite with the migrations applied (as `reader_sql` does).
//!
//! The WHERE clause holds only the filters that are present, never `?1 IS NULL OR ...`, so
//! SQLite can choose the index of the filter: `system_log_kind`, `system_log_actor` or
//! `system_log_at`. All three end in `at_ms` (then the rowid), so `ORDER BY at_ms DESC,
//! id DESC` needs no sort. Level, error code and text have no index of their own: they scan
//! `system_log_at` newest first and stop at `LIMIT`, at worst the whole table, which
//! retention caps at 10,000 rows (docs/ARCHITECTURE.md, System log).

use plumblog_store::{LogQuery, kinds_at_least};
use plumblog_syslog::Severity;

/// One bound value, mapped to a `JsValue` by `log.rs` (and to a rusqlite value by the test).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Bind {
    Text(String),
    Int(i64),
    /// A time, compared only.
    Time(u64),
    Limit(u32),
}

/// `LIKE` pattern matching `needle` anywhere, with `%`, `_` and `\` literal (`ESCAPE '\'`).
fn contains(needle: &str) -> String {
    let mut out = String::from("%");
    for c in needle.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

/// The statement for `q` and its values, in order.
pub(super) fn select(q: &LogQuery<'_>) -> (String, Vec<Bind>) {
    let mut cond: Vec<String> = Vec::new();
    let mut binds: Vec<Bind> = Vec::new();
    if let Some(kind) = q.kind {
        cond.push("l.kind = ?".into());
        binds.push(Bind::Text(kind.as_str().into()));
    }
    if let Some(min) = q.min_severity.filter(|m| *m > Severity::Info) {
        // `+` keeps the planner off `system_log_kind`: the kinds are a filter here, and an
        // IN-list lookup on that index would need a sort.
        let kinds = kinds_at_least(min);
        let marks = vec!["?"; kinds.len()].join(", ");
        let by_kind = format!("+l.kind IN ({marks})");
        // An error code raises an event to warning, never to error.
        cond.push(if min <= Severity::Warning {
            format!("(l.error_code IS NOT NULL OR {by_kind})")
        } else {
            by_kind
        });
        binds.extend(kinds.iter().map(|k| Bind::Text(k.as_str().into())));
    }
    if let Some(code) = q.error_code {
        cond.push("l.error_code = ?".into());
        binds.push(Bind::Text(code.as_str().into()));
    }
    if let Some(actor) = q.actor {
        cond.push("l.actor_id = ?".into());
        binds.push(Bind::Int(actor.0));
    }
    if let Some(from) = q.from_ms {
        cond.push("l.at_ms >= ?".into());
        binds.push(Bind::Time(from));
    }
    if let Some(to) = q.to_ms {
        cond.push("l.at_ms < ?".into());
        binds.push(Bind::Time(to));
    }
    if let Some(text) = q.text {
        cond.push("(l.request_id = ? OR l.detail LIKE ? ESCAPE '\\')".into());
        binds.push(Bind::Text(text.into()));
        binds.push(Bind::Text(contains(&plumblog_syslog::escape(text))));
    }
    if let Some((at, id)) = q.before {
        // Not a row-value comparison: this form gives the index a plain range on `at_ms`.
        cond.push("l.at_ms <= ? AND (l.at_ms < ? OR l.id < ?)".into());
        binds.extend([Bind::Time(at), Bind::Time(at), Bind::Int(id)]);
    }
    let wher = if cond.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", cond.join(" AND "))
    };
    binds.push(Bind::Limit(q.limit));
    (
        format!(
            "SELECT l.id, l.at_ms, l.request_id, l.kind, l.actor_id, l.error_code, l.detail, \
             u.display_name FROM system_log l LEFT JOIN users u ON u.id = l.actor_id{wher} \
             ORDER BY l.at_ms DESC, l.id DESC LIMIT ?"
        ),
        binds,
    )
}

#[cfg(test)]
mod plans {
    use super::*;
    use plumblog_model::{ErrorCode, UserId};
    use plumblog_syslog::EventKind;
    use rusqlite::{Connection, params_from_iter, types::Value};

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
        c
    }

    fn values(binds: &[Bind]) -> Vec<Value> {
        binds
            .iter()
            .map(|b| match b {
                Bind::Text(s) => Value::Text(s.clone()),
                Bind::Int(n) => Value::Integer(*n),
                Bind::Time(t) => Value::Integer(i64::try_from(*t).unwrap()),
                Bind::Limit(n) => Value::Integer(i64::from(*n)),
            })
            .collect()
    }

    /// The plan lines of the statement for `q`, which must bind as many values as it has `?`.
    fn plan(c: &Connection, q: &LogQuery<'_>) -> String {
        let (sql, binds) = select(q);
        assert_eq!(sql.matches('?').count(), binds.len(), "{sql}");
        let mut st = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let lines: Vec<String> = st
            .query_map(params_from_iter(values(&binds)), |r| r.get::<_, String>(3))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        lines.join("\n")
    }

    fn base() -> LogQuery<'static> {
        LogQuery {
            limit: 50,
            ..LogQuery::default()
        }
    }

    /// `index`: the `system_log` access must name it; never a sort.
    fn check(c: &Connection, name: &str, q: &LogQuery<'_>, index: &str) {
        let p = plan(c, q);
        println!("{name}: {}", p.replace('\n', " | "));
        let scan = p
            .lines()
            .find(|l| l.contains("system_log"))
            .unwrap_or_default();
        assert!(scan.contains(index), "{name}: want {index}:\n{p}");
        assert!(!p.contains("TEMP B-TREE"), "{name}: sorts:\n{p}");
    }

    #[test]
    fn every_filter_reads_an_index_in_order() {
        let c = db();
        let b = base();
        check(&c, "none", &b, "system_log_at");
        check(
            &c,
            "kind",
            &LogQuery {
                kind: Some(EventKind::SignIn),
                ..b
            },
            "system_log_kind",
        );
        check(
            &c,
            "actor",
            &LogQuery {
                actor: Some(UserId(1)),
                ..b
            },
            "system_log_actor",
        );
        check(
            &c,
            "from",
            &LogQuery {
                from_ms: Some(5),
                ..b
            },
            "system_log_at",
        );
        check(
            &c,
            "from and to",
            &LogQuery {
                from_ms: Some(5),
                to_ms: Some(9),
                ..b
            },
            "system_log_at",
        );
        check(
            &c,
            "cursor",
            &LogQuery {
                before: Some((7, 3)),
                ..b
            },
            "system_log_at",
        );
        check(
            &c,
            "kind and cursor",
            &LogQuery {
                kind: Some(EventKind::SignIn),
                before: Some((7, 3)),
                ..b
            },
            "system_log_kind",
        );
    }

    #[test]
    fn filters_without_an_index_still_read_in_order() {
        let c = db();
        let b = base();
        // No index of their own: they walk `system_log_at` newest first (bounded by the
        // retention cap), still with no sort.
        for (name, q) in [
            (
                "level warning",
                LogQuery {
                    min_severity: Some(Severity::Warning),
                    ..b
                },
            ),
            (
                "level error",
                LogQuery {
                    min_severity: Some(Severity::Error),
                    ..b
                },
            ),
            (
                "code",
                LogQuery {
                    error_code: Some(ErrorCode::Storage),
                    ..b
                },
            ),
            (
                "text",
                LogQuery {
                    text: Some("req-1"),
                    ..b
                },
            ),
            (
                "everything but kind and actor",
                LogQuery {
                    min_severity: Some(Severity::Warning),
                    error_code: Some(ErrorCode::Storage),
                    text: Some("x"),
                    from_ms: Some(1),
                    before: Some((7, 3)),
                    ..b
                },
            ),
        ] {
            check(&c, name, &q, "system_log_at");
        }
    }

    #[test]
    fn like_pattern_escapes_wildcards() {
        assert_eq!(contains(r"100%_a\b"), r"%100\%\_a\\b%");
    }
}
