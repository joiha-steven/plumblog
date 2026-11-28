//! User admin (docs/ARCHITECTURE.md, Users and auth): role changes, disabling, deleting and
//! invites. `user.manage` (owner) and session only, both checked by the route table before
//! these run, and again by the store inside each write ([`Admin`]), so an owner demoted or
//! disabled while their request runs writes nothing. The last active owner is kept by the store inside the same write
//! (`user.last_owner`), so two owners demoting each other at once cannot both succeed. Log
//! entries carry ids and roles only, never an invite code.

use plumblog_auth::{Crypto, base64url};
use plumblog_model::api::{INVITE_MAX_TTL_MS, INVITE_TTL_MS};
use plumblog_model::{Error, ErrorCode, Result, Role, UserId};
use plumblog_store::{Admin, Invite, InviteStore, NewInvite, UserStore};
use plumblog_syslog::EventKind;

use super::Input;
use super::export::user_json;
use crate::api::body::{Out, object};
use crate::api::{Ctx, Principal, Reply};

/// Random bytes in an invite code: as many as a session or API token.
const INVITE_CODE_BYTES: usize = 32;

fn invalid(field: &str, why: &str) -> Error {
    Error::new(ErrorCode::InvalidInput, format!("{field}: {why}")).at(field)
}

/// The caller as the acting owner: the store re-checks, inside the write, that this session
/// still exists and its user is still an active owner.
fn admin(p: &Principal) -> Result<Admin> {
    Ok(Admin {
        id: p.session_user()?.id,
        proof: p.session_proof()?,
    })
}

fn role(input: &Input) -> Result<Role> {
    let text = input.str("role").unwrap_or_default();
    Role::parse(text).ok_or_else(|| invalid("role", "owner or writer"))
}

async fn log(ctx: &Ctx, p: &Principal, subject: &str, action: &str, extra: &[(&str, &str)]) {
    let mut e = p
        .event(ctx, EventKind::UserChanged)
        .subject(subject)
        .field("action", action);
    for (k, v) in extra {
        e = e.field(k, v);
    }
    ctx.syslog(e).await;
}

/// `PUT /api/v1/users/{id}/role` `{role}`: 200 with the user. The caller may change their
/// own role; only an owner gets here, so it can only ever go down.
pub async fn set_role(ctx: &Ctx, p: &Principal, input: &Input) -> Result<Reply> {
    let id = UserId(input.id());
    let role = role(input)?;
    ctx.store.set_role(&admin(p)?, id, role, ctx.now).await?;
    log(
        ctx,
        p,
        &format!("user {}", id.0),
        "role",
        &[("role", role.as_str())],
    )
    .await;
    Ok(Reply::json(200, user_json(&ctx.store.get_user(id).await?)))
}

/// `PUT /api/v1/users/{id}/disabled` `{disabled}`: 200 with the user. Disabling ends their
/// sessions and revokes their API tokens in the same write; enabling brings neither back.
pub async fn set_disabled(ctx: &Ctx, p: &Principal, input: &Input) -> Result<Reply> {
    let id = UserId(input.id());
    let disabled = input.bool("disabled").unwrap_or(false);
    ctx.store
        .set_disabled(&admin(p)?, id, disabled.then_some(ctx.now), ctx.now)
        .await?;
    let action = if disabled { "disable" } else { "enable" };
    log(ctx, p, &format!("user {}", id.0), action, &[]).await;
    Ok(Reply::json(200, user_json(&ctx.store.get_user(id).await?)))
}

/// `DELETE /api/v1/users/{id}`: 204. Refused (`store.conflict`) while they author any post;
/// their sessions, passkeys, tokens and the invites they made (used ones included) go with
/// them. Logged only after the delete succeeded.
pub async fn delete(ctx: &Ctx, p: &Principal, input: &Input) -> Result<Reply> {
    let id = UserId(input.id());
    let by = admin(p)?;
    ctx.store.delete_user(&by, id, ctx.now).await?;
    let mut e = p
        .event(ctx, EventKind::UserChanged)
        .subject(&format!("user {}", id.0))
        .field("action", "delete");
    if by.id == id {
        // A self-delete: `system_log.actor_id` must name an existing user, so the entry
        // would be refused; the actor's id goes in a field instead.
        e = e.actor(None).field("actor", &id.0.to_string());
    }
    ctx.syslog(e).await;
    Ok(Reply::empty())
}

/// Key for key as `shape::INVITE` (then `code`, only when it was just made). Named one by one
/// so the code hash never reaches an answer.
fn invite_json(i: &Invite, code: Option<&str>) -> String {
    let mut out = vec![
        ("id", Out::Int(i.id)),
        ("role", Out::Str(i.role.as_str().to_owned())),
        ("created_by", Out::Int(i.created_by.0)),
        ("created_at", Out::Int(i.created_at)),
        ("expires_at", Out::Int(i.expires_at)),
    ];
    if let Some(c) = code {
        out.push(("code", Out::Str(c.to_owned())));
    }
    object(&out)
}

/// `expires_at`: in the future and at most [`INVITE_MAX_TTL_MS`] away; absent is
/// [`INVITE_TTL_MS`] from now.
fn invite_expiry(input: &Input, now: i64) -> Result<i64> {
    let Some(at) = input.int("expires_at") else {
        return Ok(now.saturating_add(INVITE_TTL_MS));
    };
    if at <= now {
        return Err(invalid("expires_at", "in the past"));
    }
    if at > now.saturating_add(INVITE_MAX_TTL_MS) {
        return Err(invalid("expires_at", "at most 30 days ahead"));
    }
    Ok(at)
}

/// `POST /api/v1/invites` `{role, expires_at?}`: 201 with the invite and its code, shown in
/// this answer only; the store keeps its SHA-256 (what `POST /api/auth/invite/accept`
/// looks up). Written only if, at that moment, the caller's session still exists and they
/// are still an active owner.
pub async fn invite_create(ctx: &Ctx, p: &Principal, input: &Input) -> Result<Reply> {
    let user = p.session_user()?;
    let role = role(input)?;
    let expires_at = invite_expiry(input, ctx.now)?;
    let mut bytes = [0u8; INVITE_CODE_BYTES];
    ctx.crypto.random_bytes(&mut bytes)?;
    let code = base64url::encode(&bytes);
    let code_hash = base64url::hex(&ctx.crypto.sha256(code.as_bytes()).await?);
    let new = NewInvite {
        code_hash: code_hash.clone(),
        role,
        created_by: user.id,
        created_at: ctx.now,
        expires_at,
        proof: p.session_proof()?,
    };
    let id = ctx.store.create_invite(new).await?;
    let made = Invite {
        id,
        code_hash,
        role,
        created_by: user.id,
        created_at: ctx.now,
        expires_at,
        used_at: None,
        used_by: None,
    };
    log(
        ctx,
        p,
        &format!("invite {id}"),
        "invite_create",
        &[("role", role.as_str())],
    )
    .await;
    Ok(Reply::json(201, invite_json(&made, Some(&code))))
}

/// `GET /api/v1/invites`: unused invites, newest first, expired ones included (the admin
/// shows them as expired until revoked). Never the code or its hash.
pub async fn invite_list(ctx: &Ctx) -> Result<Reply> {
    let items: Vec<String> = ctx
        .store
        .list_invites()
        .await?
        .iter()
        .filter(|i| i.used_at.is_none())
        .map(|i| invite_json(i, None))
        .collect();
    Ok(Reply::json(
        200,
        object(&[("invites", Out::Raw(format!("[{}]", items.join(","))))]),
    ))
}

/// `DELETE /api/v1/invites/{id}`: 204; the code stops working at once. A used invite answers
/// like a missing one (one conditional delete in the store): it is the record of how that
/// user joined.
pub async fn invite_revoke(ctx: &Ctx, p: &Principal, input: &Input) -> Result<Reply> {
    let id = input.id();
    ctx.store.delete_invite(&admin(p)?, id, ctx.now).await?;
    log(ctx, p, &format!("invite {id}"), "invite_revoke", &[]).await;
    Ok(Reply::empty())
}

#[cfg(test)]
mod tests {
    use super::invite_json;
    use plumblog_model::{Role, UserId};
    use plumblog_store::Invite;

    #[test]
    fn invite_json_holds_the_code_only_when_given() {
        let i = Invite {
            id: 3,
            code_hash: "secret-hash".into(),
            role: Role::Writer,
            created_by: UserId(1),
            created_at: 10,
            expires_at: 20,
            used_at: None,
            used_by: None,
        };
        let listed = invite_json(&i, None);
        assert!(!listed.contains("secret-hash"), "{listed}");
        assert_eq!(
            listed,
            r#"{"id":3,"role":"writer","created_by":1,"created_at":10,"expires_at":20}"#
        );
        let made = invite_json(&i, Some("the-code"));
        assert!(!made.contains("secret-hash"), "{made}");
        assert!(made.ends_with(r#","code":"the-code"}"#), "{made}");
    }
}
