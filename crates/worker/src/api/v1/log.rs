//! `GET /api/v1/log`: the owner's system log, newest first, filtered and keyset-paged by
//! `(at_ms, id)` (docs/ARCHITECTURE.md, System log).

use plumblog_auth::base64url;
use plumblog_model::api::{DEFAULT_LIMIT, MAX_LIMIT};
use plumblog_model::{Error, ErrorCode, Result, UserId};
use plumblog_store::{LogEntry, LogQuery, LogStore};
use plumblog_syslog::{EventKind, Severity};

use super::Input;
use crate::api::body::{Out, object};
use crate::api::{Ctx, Reply};

/// Longest `q`, in characters.
const MAX_TEXT: usize = 100;

fn invalid(field: &str, why: &str) -> Error {
    Error::new(ErrorCode::InvalidInput, format!("{field}: {why}")).at(field)
}

fn limit(input: &Input) -> Result<u32> {
    let limit = input.int("limit").unwrap_or(i64::from(DEFAULT_LIMIT));
    u32::try_from(limit)
        .ok()
        .filter(|l| (1..=MAX_LIMIT).contains(l))
        .ok_or_else(|| invalid("limit", "1-100"))
}

/// A time bound: milliseconds since the epoch, never negative.
fn time(input: &Input, field: &str) -> Result<Option<u64>> {
    input.int(field).map_or(Ok(None), |n| {
        u64::try_from(n)
            .map(Some)
            .map_err(|_| invalid(field, "milliseconds, 0 or more"))
    })
}

/// `(at_ms, id)` of the last entry of the previous page, as `<at_ms>.<id>` in base64url.
fn before(input: &Input) -> Result<Option<(u64, i64)>> {
    let Some(text) = input.str("cursor") else {
        return Ok(None);
    };
    let bad = || invalid("cursor", "not a cursor of this listing");
    let raw = String::from_utf8(base64url::decode(text).ok_or_else(bad)?).map_err(|_| bad())?;
    let (at, id) = raw.split_once('.').ok_or_else(bad)?;
    let id: i64 = id.parse().map_err(|_| bad())?;
    let at: u64 = at.parse().map_err(|_| bad())?;
    if id <= 0 {
        return Err(bad());
    }
    Ok(Some((at, id)))
}

fn cursor(at_ms: u64, id: i64) -> String {
    base64url::encode(format!("{at_ms}.{id}").as_bytes())
}

/// Key for key as `shape::LOG_ENTRY`. Values keep the stored `{redacted}` marker;
/// `Event::restore` redacted them again on the way out of storage.
fn entry_json(e: &LogEntry) -> String {
    let ev = &e.event;
    let fields: Vec<String> = ev
        .fields()
        .iter()
        .map(|(k, v)| object(&[("key", Out::Str(k.clone())), ("value", Out::Str(v.clone()))]))
        .collect();
    object(&[
        ("id", Out::Int(e.id)),
        (
            "at",
            Out::Int(i64::try_from(ev.at_ms()).unwrap_or(i64::MAX)),
        ),
        ("request_id", Out::Str(ev.request_id().as_str().to_owned())),
        ("kind", Out::Str(ev.kind().as_str().to_owned())),
        ("level", Out::Str(ev.severity().as_str().to_owned())),
        (
            "actor_id",
            ev.actor_id().map_or(Out::Null, |a| Out::Int(a.0)),
        ),
        (
            "actor_name",
            e.actor_name.clone().map_or(Out::Null, Out::Str),
        ),
        (
            "error_code",
            ev.error_code()
                .map_or(Out::Null, |c| Out::Str(c.as_str().to_owned())),
        ),
        ("subject", Out::Str(ev.subject_text().to_owned())),
        ("fields", Out::Raw(format!("[{}]", fields.join(",")))),
    ])
}

pub async fn list(ctx: &Ctx, input: &Input) -> Result<Reply> {
    let n = limit(input)?;
    let kind = input
        .str("kind")
        .map(|k| EventKind::parse(k).ok_or_else(|| invalid("kind", "not an event kind")))
        .transpose()?;
    let min_severity = input
        .str("level")
        .map(|l| Severity::parse(l).ok_or_else(|| invalid("level", "info, warning or error")))
        .transpose()?;
    let error_code = input
        .str("code")
        .map(|c| ErrorCode::parse(c).ok_or_else(|| invalid("code", "not an error code")))
        .transpose()?;
    let actor = input.int("actor").map(UserId);
    let text = input.str("q");
    if text.is_some_and(|t| !(1..=MAX_TEXT).contains(&t.chars().count())) {
        return Err(invalid("q", "1-100 characters"));
    }
    let query = LogQuery {
        kind,
        min_severity,
        error_code,
        actor,
        text,
        from_ms: time(input, "from")?,
        to_ms: time(input, "to")?,
        before: before(input)?,
        // One more than the page: its presence says another page exists.
        limit: n + 1,
    };
    let mut rows = ctx.store.query_log(query).await?;
    let more = rows.len() > n as usize;
    rows.truncate(n as usize);
    let next = match rows.last() {
        Some(last) if more => Out::Str(cursor(last.event.at_ms(), last.id)),
        _ => Out::Null,
    };
    let items: Vec<String> = rows.iter().map(entry_json).collect();
    Ok(Reply::json(
        200,
        object(&[
            ("entries", Out::Raw(format!("[{}]", items.join(",")))),
            ("next_cursor", next),
        ]),
    ))
}
