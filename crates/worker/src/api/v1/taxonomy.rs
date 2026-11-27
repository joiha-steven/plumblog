//! `/api/v1/tags` and `/api/v1/series`. Lists are whole (a blog has tens of tags, and the
//! store returns them in one query); items are addressed by id.

use plumblog_model::{Error, ErrorCode, Permission, Result, SeriesId, TagId};
use plumblog_store::{NewSeries, Series, Tag, TaxonomyStore};
use plumblog_syslog::EventKind;

use super::{Input, Principal};
use crate::api::body::{Out, object};
use crate::api::{Ctx, Reply, fields};

const MAX_NAME: usize = 100;
const MAX_DESCRIPTION: usize = 2000;

fn invalid(field: &str, why: &str) -> Error {
    Error::new(ErrorCode::InvalidInput, format!("{field}: {why}")).at(field)
}

/// Not blank, no control characters (newlines only where `multiline`).
fn text(field: &str, v: &str, max: usize, multiline: bool) -> Result<()> {
    if !multiline && v.trim().is_empty() {
        return Err(invalid(field, "empty"));
    }
    if v.chars().count() > max {
        return Err(invalid(field, "too long"));
    }
    let bad = |c: char| c.is_control() && !(multiline && (c == '\n' || c == '\t'));
    if v.chars().any(bad) {
        return Err(invalid(field, "control character"));
    }
    Ok(())
}

fn slug(v: &str) -> Result<()> {
    fields::slug(v).map_err(|e| e.at("slug"))
}

/// A slug another row holds is `Conflict` on `slug`.
fn slug_conflict(e: Error) -> Error {
    if e.code == ErrorCode::Conflict {
        e.at("slug")
    } else {
        e
    }
}

fn not_found(what: &str, id: i64) -> Error {
    Error::new(ErrorCode::NotFound, format!("{what} {id}"))
}

async fn log(ctx: &Ctx, p: &Principal, action: &str, subject: &str, id: i64) {
    ctx.syslog(
        p.event(ctx, EventKind::ContentChanged)
            .subject(subject)
            .field("action", action)
            .field("id", &id.to_string()),
    )
    .await;
}

fn tag_json(t: &Tag) -> String {
    object(&[
        ("id", Out::Int(t.id.0)),
        ("slug", Out::Str(t.slug.clone())),
        ("name", Out::Str(t.name.clone())),
    ])
}

fn series_json(s: &Series) -> String {
    object(&[
        ("id", Out::Int(s.id.0)),
        ("slug", Out::Str(s.slug.clone())),
        ("title", Out::Str(s.title.clone())),
        ("description", Out::Str(s.description.clone())),
    ])
}

fn list_json(key: &str, items: &[String]) -> String {
    object(&[(key, Out::Raw(format!("[{}]", items.join(","))))])
}

async fn tag(ctx: &Ctx, id: i64) -> Result<Tag> {
    ctx.store
        .list_tags()
        .await?
        .into_iter()
        .find(|t| t.id.0 == id)
        .ok_or_else(|| not_found("tag", id))
}

async fn series(ctx: &Ctx, id: i64) -> Result<Series> {
    ctx.store
        .list_series()
        .await?
        .into_iter()
        .find(|s| s.id.0 == id)
        .ok_or_else(|| not_found("series", id))
}

pub async fn tag_list(ctx: &Ctx) -> Result<Reply> {
    let items: Vec<String> = ctx.store.list_tags().await?.iter().map(tag_json).collect();
    Ok(Reply::json(200, list_json("tags", &items)))
}

pub async fn tag_get(ctx: &Ctx, input: &Input) -> Result<Reply> {
    Ok(Reply::json(200, tag_json(&tag(ctx, input.id()).await?)))
}

/// `POST /api/v1/tags`: 201; a slug in use is 409 on `slug` (two creates of the same slug at
/// once may both see it free and both get the one tag, which is the same outcome).
pub async fn tag_create(ctx: &Ctx, p: &Principal, input: &Input) -> Result<Reply> {
    p.require(Permission::WritePost)?;
    let s = input.str("slug").unwrap_or_default();
    let name = input.str("name").unwrap_or_default();
    slug(s)?;
    text("name", name, MAX_NAME, false)?;
    if ctx.store.find_tag(s).await?.is_some() {
        return Err(Error::new(ErrorCode::Conflict, format!("tag {s:?} exists")).at("slug"));
    }
    let id = ctx.store.ensure_tag(s, name).await?;
    let t = tag(ctx, id.0).await?;
    log(ctx, p, "tag.create", &t.name, id.0).await;
    Ok(Reply::json(201, tag_json(&t)))
}

pub async fn tag_update(ctx: &Ctx, p: &Principal, input: &Input) -> Result<Reply> {
    p.require(Permission::WritePost)?;
    let mut t = tag(ctx, input.id()).await?;
    if let Some(s) = input.str("slug") {
        slug(s)?;
        t.slug = s.to_owned();
    }
    if let Some(n) = input.str("name") {
        text("name", n, MAX_NAME, false)?;
        t.name = n.to_owned();
    }
    ctx.store.update_tag(&t).await.map_err(slug_conflict)?;
    log(ctx, p, "tag.update", &t.name, t.id.0).await;
    Ok(Reply::json(200, tag_json(&t)))
}

/// Removes the tag from every post (their versions go up).
pub async fn tag_delete(ctx: &Ctx, p: &Principal, input: &Input) -> Result<Reply> {
    p.require(Permission::WritePost)?;
    let t = tag(ctx, input.id()).await?;
    ctx.store.delete_tag(TagId(t.id.0)).await?;
    log(ctx, p, "tag.delete", &t.name, t.id.0).await;
    Ok(Reply::empty())
}

pub async fn series_list(ctx: &Ctx) -> Result<Reply> {
    let items: Vec<String> = ctx
        .store
        .list_series()
        .await?
        .iter()
        .map(series_json)
        .collect();
    Ok(Reply::json(200, list_json("series", &items)))
}

pub async fn series_get(ctx: &Ctx, input: &Input) -> Result<Reply> {
    Ok(Reply::json(
        200,
        series_json(&series(ctx, input.id()).await?),
    ))
}

pub async fn series_create(ctx: &Ctx, p: &Principal, input: &Input) -> Result<Reply> {
    p.require(Permission::WritePost)?;
    let s = input.str("slug").unwrap_or_default();
    let title = input.str("title").unwrap_or_default();
    let description = input.str("description").unwrap_or_default();
    slug(s)?;
    text("title", title, MAX_NAME, false)?;
    text("description", description, MAX_DESCRIPTION, true)?;
    let id = ctx
        .store
        .create_series(NewSeries {
            slug: s.to_owned(),
            title: title.to_owned(),
            description: description.to_owned(),
        })
        .await
        .map_err(slug_conflict)?;
    let made = series(ctx, id.0).await?;
    log(ctx, p, "series.create", &made.title, id.0).await;
    Ok(Reply::json(201, series_json(&made)))
}

pub async fn series_update(ctx: &Ctx, p: &Principal, input: &Input) -> Result<Reply> {
    p.require(Permission::WritePost)?;
    let mut s = series(ctx, input.id()).await?;
    if let Some(v) = input.str("slug") {
        slug(v)?;
        s.slug = v.to_owned();
    }
    if let Some(v) = input.str("title") {
        text("title", v, MAX_NAME, false)?;
        s.title = v.to_owned();
    }
    if let Some(v) = input.str("description") {
        text("description", v, MAX_DESCRIPTION, true)?;
        s.description = v.to_owned();
    }
    ctx.store.update_series(&s).await.map_err(slug_conflict)?;
    log(ctx, p, "series.update", &s.title, s.id.0).await;
    Ok(Reply::json(200, series_json(&s)))
}

/// Its posts leave the series (their versions go up).
pub async fn series_delete(ctx: &Ctx, p: &Principal, input: &Input) -> Result<Reply> {
    p.require(Permission::WritePost)?;
    let s = series(ctx, input.id()).await?;
    ctx.store.delete_series(SeriesId(s.id.0)).await?;
    log(ctx, p, "series.delete", &s.title, s.id.0).await;
    Ok(Reply::empty())
}
