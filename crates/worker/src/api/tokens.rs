//! `/api/auth/tokens`: a signed-in user's own API tokens (docs/ARCHITECTURE.md, AI surface).
//! Session only (the gate refuses tokens on `/api/auth/`, and each handler asks for the
//! session again). The token text is in the create answer and nowhere else: not in the list,
//! the system log or the Worker log.

use plumblog_auth::api_token;
use plumblog_model::{Error, ErrorCode, Permission, Result, TokenScope};
use plumblog_store::{ApiToken, NewToken, TokenStore};
use plumblog_syslog::EventKind;

use super::body::{Fields, Out, object};
use super::{Ctx, Principal, Reply, fields};

pub const COLLECTION: &str = "/api/auth/tokens";
/// `DELETE` target: `/api/auth/tokens/<id>`.
pub const ITEM_PREFIX: &str = "/api/auth/tokens/";

fn invalid(detail: impl Into<String>) -> Error {
    Error::new(ErrorCode::InvalidInput, detail)
}

fn token_json(t: &ApiToken, secret: Option<&str>) -> String {
    let mut out = vec![
        ("id", Out::Int(t.id)),
        ("name", Out::Str(t.name.clone())),
        ("scope", Out::Str(t.scope.as_str().to_owned())),
        ("created_at", Out::Int(t.created_at)),
        ("expires_at", t.expires_at.map_or(Out::Null, Out::Int)),
        ("revoked_at", t.revoked_at.map_or(Out::Null, Out::Int)),
    ];
    if let Some(s) = secret {
        out.push(("token", Out::Str(s.to_owned())));
    }
    object(&out)
}

/// `expires_at`: UTC milliseconds as a decimal string (request bodies are flat strings), in
/// the future. Absent = never expires.
fn expires_at(value: Option<&str>, now: i64) -> Result<Option<i64>> {
    let Some(v) = value else { return Ok(None) };
    if v.is_empty() || v.len() > 16 || !v.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid("expires_at: UTC milliseconds as a decimal string"));
    }
    let at: i64 = v.parse().map_err(|_| invalid("expires_at: out of range"))?;
    if at <= now {
        return Err(invalid("expires_at: in the past"));
    }
    Ok(Some(at))
}

/// `POST /api/auth/tokens` `{name, scope, expires_at?}`: 201 with the token, shown once.
/// The scope cannot exceed the caller's role (a writer cannot create an owner token).
pub async fn create(ctx: &Ctx, p: &Principal, body: &[u8]) -> Result<Reply> {
    let user = p.session_user()?;
    p.require(Permission::CreateOwnToken)?;
    let f = Fields::parse(body, &["name", "scope", "expires_at"])?;
    let name = f.req("name")?;
    fields::token_name(name)?;
    let scope_text = f.req("scope")?;
    let scope = TokenScope::parse(scope_text)
        .ok_or_else(|| invalid(format!("scope: unknown {scope_text:?}")))?;
    let expires_at = expires_at(f.opt("expires_at")?, ctx.now)?;
    if scope > TokenScope::from(user.role) {
        return Err(Error::new(
            ErrorCode::Forbidden,
            format!("{:?} cannot create a {scope:?} token", user.role),
        ));
    }
    let (secret, hash) = api_token::new_token(&ctx.crypto).await?;
    let new = NewToken {
        user_id: user.id,
        name: name.to_owned(),
        token_hash: hash.clone(),
        scope,
        created_at: ctx.now,
        expires_at,
    };
    let id = ctx.store.create_token(new).await?;
    let made = ApiToken {
        id,
        user_id: user.id,
        name: name.to_owned(),
        token_hash: hash,
        scope,
        created_at: ctx.now,
        expires_at,
        revoked_at: None,
    };
    ctx.syslog(
        p.event(ctx, EventKind::TokenCreated)
            .subject(name)
            .field("scope", scope.as_str()),
    )
    .await;
    Ok(Reply::json(201, token_json(&made, Some(secret.as_str()))))
}

/// `GET /api/auth/tokens`: the caller's own tokens, revoked and expired included, without
/// the token text or its hash.
pub async fn list(ctx: &Ctx, p: &Principal) -> Result<Reply> {
    let user = p.session_user()?;
    let items: Vec<String> = ctx
        .store
        .list_tokens(user.id)
        .await?
        .iter()
        .map(|t| token_json(t, None))
        .collect();
    Ok(Reply::json(
        200,
        object(&[("tokens", Out::Raw(format!("[{}]", items.join(","))))]),
    ))
}

/// `DELETE /api/auth/tokens/<id>`: revokes one of the caller's own tokens, at once. Another
/// user's id answers like a missing one. Revoking twice is 204 and keeps the first time.
pub async fn revoke(ctx: &Ctx, p: &Principal, id: &str) -> Result<Reply> {
    let user = p.session_user()?;
    let not_found = || {
        Error::new(
            ErrorCode::NotFound,
            format!("api token {id:?} of user {}", user.id.0),
        )
    };
    if id.is_empty() || id.len() > 18 || !id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(not_found());
    }
    let id: i64 = id.parse().map_err(|_| not_found())?;
    // Ownership never changes, so checking it before the write cannot race.
    let token = ctx
        .store
        .list_tokens(user.id)
        .await?
        .into_iter()
        .find(|t| t.id == id)
        .ok_or_else(not_found)?;
    ctx.store.revoke_token(id, ctx.now).await?;
    if token.revoked_at.is_none() {
        ctx.syslog(
            p.event(ctx, EventKind::TokenRevoked)
                .subject(&token.name)
                .field("scope", token.scope.as_str()),
        )
        .await;
    }
    Ok(Reply::empty())
}
