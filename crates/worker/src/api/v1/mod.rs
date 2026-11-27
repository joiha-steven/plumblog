//! `/api/v1`: the content API for the admin, the CLI and AI clients (docs/ARCHITECTURE.md,
//! AI surface). Routes come from [`plumblog_model::api::ROUTES`]: this module finds the
//! route, checks its access and input against the table, then calls the handler. Session
//! cookie or API token alike.

mod input;
mod post_body;
mod posts;
mod taxonomy;

use plumblog_model::api::{self, Access, Field, Method as M, ROUTES, Route, RouteId};
use plumblog_model::{Error, ErrorCode, Result, TokenScope};
use worker::Method;

use super::body::{Out, object, quote};
use super::{Ctx, Principal, Reply};
pub use input::{Input, Nullable};

pub const PREFIX: &str = "/api/v1/";

fn method(m: &Method) -> Option<M> {
    match m {
        Method::Get => Some(M::Get),
        Method::Post => Some(M::Post),
        Method::Patch => Some(M::Patch),
        Method::Delete => Some(M::Delete),
        _ => None,
    }
}

/// The body cap for this request: its route's, else the default.
pub fn max_body(m: &Method, path: &str) -> usize {
    method(m)
        .and_then(|m| api::find(m, path).ok())
        .map_or(api::MAX_BODY, |(r, _)| r.max_body)
}

/// Finds the route, checks access then input, and runs the handler.
///
/// # Errors
///
/// `RouteNotFound` for a path or method outside the table; the access and input errors of
/// [`Input`]; whatever the handler returns.
pub async fn dispatch(
    ctx: &Ctx,
    p: &Principal,
    m: &Method,
    path: &str,
    query: &[(String, String)],
    body: &[u8],
) -> Result<Reply> {
    let not_found = || {
        Error::new(
            ErrorCode::RouteNotFound,
            format!("no route {} {path}", m.as_ref()),
        )
    };
    let (route, path_values) = method(m)
        .ok_or_else(not_found)
        .and_then(|m| api::find(m, path).map_err(|_| not_found()))?;
    // Access first: an anonymous or read-only caller learns nothing from input checks.
    match route.access {
        Access::Read => p.require_read()?,
        Access::Require(perm) => p.require(perm)?,
    };
    let input = Input::parse(route, &path_values, query, body)?;
    match route.id {
        RouteId::Reference => Ok(Reply::json(200, reference())),
        RouteId::Me => me(p),
        RouteId::PostList => posts::list(ctx, &input).await,
        RouteId::PostGet => posts::get(ctx, &input).await,
        RouteId::PostCreate => posts::create(ctx, p, &input).await,
        RouteId::PostUpdate => posts::update(ctx, p, &input).await,
        RouteId::PostTrash => posts::trash(ctx, p, &input, true).await,
        RouteId::PostRestore => posts::trash(ctx, p, &input, false).await,
        RouteId::PostDelete => posts::delete(ctx, p, &input).await,
        RouteId::TagList => taxonomy::tag_list(ctx).await,
        RouteId::TagGet => taxonomy::tag_get(ctx, &input).await,
        RouteId::TagCreate => taxonomy::tag_create(ctx, p, &input).await,
        RouteId::TagUpdate => taxonomy::tag_update(ctx, p, &input).await,
        RouteId::TagDelete => taxonomy::tag_delete(ctx, p, &input).await,
        RouteId::SeriesList => taxonomy::series_list(ctx).await,
        RouteId::SeriesGet => taxonomy::series_get(ctx, &input).await,
        RouteId::SeriesCreate => taxonomy::series_create(ctx, p, &input).await,
        RouteId::SeriesUpdate => taxonomy::series_update(ctx, p, &input).await,
        RouteId::SeriesDelete => taxonomy::series_delete(ctx, p, &input).await,
    }
}

fn access_text(a: Access) -> &'static str {
    match a {
        Access::Read => "read",
        Access::Require(p) => p.as_str(),
    }
}

fn field_json(f: &Field) -> String {
    object(&[
        ("name", Out::Str(f.name.to_owned())),
        ("type", Out::Str(f.ty.as_str().to_owned())),
        ("in", Out::Str(f.at.as_str().to_owned())),
        ("required", Out::Bool(f.required)),
    ])
}

fn route_json(r: &Route) -> String {
    let list = |items: Vec<String>| Out::Raw(format!("[{}]", items.join(",")));
    object(&[
        ("method", Out::Str(r.method.as_str().to_owned())),
        ("path", Out::Str(r.path.to_owned())),
        ("access", Out::Str(access_text(r.access).to_owned())),
        (
            "also",
            list(r.also.iter().map(|p| quote(p.as_str())).collect()),
        ),
        ("fields", list(r.fields.iter().map(field_json).collect())),
        (
            "max_body",
            Out::Int(i64::try_from(r.max_body).unwrap_or(i64::MAX)),
        ),
        ("status", Out::Int(i64::from(r.status))),
        ("response", Out::Str(r.response.to_owned())),
    ])
}

/// `GET /api/v1/reference`: the route table as JSON.
#[must_use]
pub fn reference() -> String {
    let routes: Vec<String> = ROUTES.iter().map(route_json).collect();
    object(&[
        ("version", Out::Str("v1".to_owned())),
        ("max_limit", Out::Int(i64::from(api::MAX_LIMIT))),
        ("routes", Out::Raw(format!("[{}]", routes.join(",")))),
    ])
}

/// `GET /api/v1/me`: the caller and what it may do right now.
fn me(p: &Principal) -> Result<Reply> {
    let user = p.require_read()?;
    let (via, scope) = match p {
        Principal::Token { scope, .. } => ("token", scope.effective(user.role)),
        _ => ("session", TokenScope::from(user.role)),
    };
    let permissions: Vec<String> = plumblog_model::Permission::ALL
        .iter()
        .filter(|perm| scope.can(**perm))
        .map(|perm| quote(perm.as_str()))
        .collect();
    let u = object(&[
        ("id", Out::Int(user.id.0)),
        ("login", Out::Str(user.login.clone())),
        ("slug", Out::Str(user.slug.clone())),
        ("display_name", Out::Str(user.display_name.clone())),
        ("role", Out::Str(user.role.as_str().to_owned())),
    ]);
    Ok(Reply::json(
        200,
        object(&[
            ("user", Out::Raw(u)),
            ("via", Out::Str(via.to_owned())),
            ("scope", Out::Str(scope.as_str().to_owned())),
            (
                "permissions",
                Out::Raw(format!("[{}]", permissions.join(","))),
            ),
        ]),
    ))
}

#[cfg(test)]
mod tests {
    use super::reference;
    use plumblog_model::api::ROUTES;

    /// The served reference lists every route of the table, and only those.
    #[test]
    fn reference_lists_every_route() {
        let r = reference();
        for route in ROUTES {
            let entry = format!(
                "\"method\":\"{}\",\"path\":\"{}\"",
                route.method.as_str(),
                route.path
            );
            assert_eq!(r.matches(&entry).count(), 1, "{entry}");
        }
        assert_eq!(r.matches("\"method\":").count(), ROUTES.len());
    }
}
