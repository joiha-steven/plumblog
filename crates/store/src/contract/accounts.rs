//! Users, sessions, passkeys, API tokens and invites.

use plumblog_model::{ErrorCode, Role, TokenScope, UserId};
use plumblog_syslog::EventKind;

use super::kit::{
    Check, T0, eq, event, fails, media, new_user, ok, post, save, that, update, user,
};
use crate::{
    LogQuery, NewInvite, NewPasskey, NewRevision, NewSession, NewToken, PostStatus, ProfileUpdate,
    Store,
};

pub async fn users_round_trip<S: Store>(db: &S) -> Check {
    let mut nu = new_user("Alice");
    nu.role = Role::Owner;
    nu.password_hash = Some("hash".into());
    nu.bio = "bio".into();
    let a = ok("create_user", db.create_user(nu).await)?;
    let b = user(db, "bob").await?;
    let got = ok("get_user", db.get_user(a).await)?;
    eq!("login", got.login.as_str(), "Alice")?;
    eq!("slug", got.slug.as_str(), "alice")?;
    eq!("role", got.role, Role::Owner)?;
    eq!("password_hash", got.password_hash.as_deref(), Some("hash"))?;
    eq!("disabled_at", got.disabled_at, None)?;
    eq!(
        "updated_at = created_at",
        (got.created_at, got.updated_at),
        (T0, T0),
    )?;
    let by_login = ok("find_user_by_login", db.find_user_by_login("aLiCe").await)?;
    eq!(
        "find_user_by_login ignores case",
        by_login.map(|u| u.id),
        Some(a),
    )?;
    let by_slug = ok("find_user_by_slug", db.find_user_by_slug("bob").await)?;
    eq!("find_user_by_slug", by_slug.map(|u| u.id), Some(b))?;
    let none = ok("find_user_by_login", db.find_user_by_login("carol").await)?;
    eq!("unknown login", none, None)?;
    let none = ok("find_user_by_slug", db.find_user_by_slug("carol").await)?;
    eq!("unknown slug", none, None)?;
    let ids: Vec<UserId> = ok("list_users", db.list_users().await)?
        .iter()
        .map(|u| u.id)
        .collect();
    eq!("list_users by id", ids, super::kit::asc(&[a, b]))?;

    ok(
        "update_profile",
        db.update_profile(a, profile("alice2", "alice-2", "new"), T0 + 5)
            .await,
    )?;
    let after = ok("get_user", db.get_user(a).await)?;
    let mut want = got.clone();
    "alice2".clone_into(&mut want.login);
    "alice-2".clone_into(&mut want.slug);
    "alice2 shown".clone_into(&mut want.display_name);
    "new".clone_into(&mut want.bio);
    want.updated_at = T0 + 5;
    eq!(
        "update_profile writes only the profile and updated_at",
        after,
        want,
    )?;
    // Same login with other case, held by the same user, is not a conflict.
    ok(
        "update_profile own login",
        db.update_profile(a, profile("ALICE2", "alice-2", "new"), T0 + 6)
            .await,
    )
}

fn profile(login: &str, slug: &str, bio: &str) -> ProfileUpdate {
    ProfileUpdate {
        login: login.into(),
        slug: slug.into(),
        display_name: format!("{login} shown"),
        bio: bio.into(),
    }
}

pub async fn user_errors<S: Store>(db: &S) -> Check {
    let a = user(db, "alice").await?;
    let b = user(db, "bob").await?;
    fails(
        "create_user same login other case",
        db.create_user(new_user("ALICE")).await,
        ErrorCode::Conflict,
    )?;
    let mut dup_slug = new_user("carol");
    dup_slug.slug = "alice".into();
    fails(
        "create_user taken slug",
        db.create_user(dup_slug).await,
        ErrorCode::Conflict,
    )?;
    let missing = UserId(a.0.max(b.0) + 1000);
    fails(
        "get_user missing",
        db.get_user(missing).await,
        ErrorCode::UserNotFound,
    )?;
    fails(
        "update_profile missing",
        db.update_profile(missing, profile("ghost", "ghost", ""), T0)
            .await,
        ErrorCode::UserNotFound,
    )?;
    fails(
        "set_password missing",
        db.set_password(missing, None, T0).await,
        ErrorCode::UserNotFound,
    )?;
    fails(
        "set_role missing",
        db.set_role(missing, Role::Writer, T0).await,
        ErrorCode::UserNotFound,
    )?;
    fails(
        "set_role owner missing",
        db.set_role(missing, Role::Owner, T0).await,
        ErrorCode::UserNotFound,
    )?;
    fails(
        "set_disabled missing",
        db.set_disabled(missing, Some(T0), T0).await,
        ErrorCode::UserNotFound,
    )?;
    fails(
        "set_disabled none missing",
        db.set_disabled(missing, None, T0).await,
        ErrorCode::UserNotFound,
    )?;
    fails(
        "delete_user missing",
        db.delete_user(missing).await,
        ErrorCode::UserNotFound,
    )?;
    fails(
        "update_profile to taken login",
        db.update_profile(b, profile("Alice", "bob", ""), T0).await,
        ErrorCode::Conflict,
    )?;
    fails(
        "update_profile to taken slug",
        db.update_profile(b, profile("bob", "alice", ""), T0).await,
        ErrorCode::Conflict,
    )?;
    eq!(
        "failed update_profile keeps the row",
        ok("get_user", db.get_user(b).await)?.login.as_str(),
        "bob",
    )?;
    let users = ok("list_users", db.list_users().await)?;
    eq!("failed writes add no user", users.len(), 2)
}

/// RESTRICT on posts, CASCADE on sign-in rows and own invites, SET NULL elsewhere.
pub async fn user_delete_effects<S: Store>(db: &S) -> Check {
    let owner = user(db, "owner").await?;
    let invite_for_v = ok(
        "create_invite",
        db.create_invite(NewInvite {
            code_hash: "code-v".into(),
            role: Role::Writer,
            created_by: owner,
            created_at: T0,
            expires_at: T0 + 100,
            proof: crate::Proof::Unchecked,
        })
        .await,
    )?;
    let v = ok(
        "accept_invite",
        db.accept_invite("code-v", new_user("victim"), T0 + 1).await,
    )?;
    ok(
        "create_session",
        db.create_session(NewSession {
            token_hash: "sess-v".into(),
            user_id: v,
            created_at: T0,
            expires_at: T0 + 1000,
            proof: crate::Proof::Unchecked,
        })
        .await,
    )?;
    ok("add_passkey", db.add_passkey(passkey(v, "cred-v")).await)?;
    ok(
        "create_token",
        db.create_token(token(v, "tok-v", None)).await,
    )?;
    let own_invite = ok(
        "create_invite by victim",
        db.create_invite(NewInvite {
            code_hash: "code-by-v".into(),
            role: Role::Writer,
            created_by: v,
            created_at: T0,
            expires_at: T0 + 100,
            proof: crate::Proof::Unchecked,
        })
        .await,
    )?;
    let mut m = media("v.png", T0);
    m.uploaded_by = Some(v);
    let mid = ok("create_media", db.create_media(m).await)?;
    let mut p = post(v, "vi", "by-victim", PostStatus::Draft);
    p.revision = Some(NewRevision {
        author_id: Some(v),
        created_at: T0,
    });
    let pid = save(db, p.clone()).await?;
    let log_id = ok(
        "append",
        db.append(event(5, EventKind::SignIn, Some(v))?).await,
    )?;

    fails(
        "delete_user who authors a post",
        db.delete_user(v).await,
        ErrorCode::Conflict,
    )?;
    eq!(
        "failed delete keeps the session",
        ok("find_session", db.find_session("sess-v", T0).await)?.is_some(),
        true,
    )?;
    p.author_id = owner;
    p.revision = None;
    save(db, update(db, pid, p).await?).await?;
    ok("delete_user", db.delete_user(v).await)?;

    sign_in_rows_gone(db, v).await?;
    let invites = ok("list_invites", db.list_invites().await)?;
    that(
        "invites the user created cascade",
        invites.iter().all(|i| i.id != own_invite),
    )?;
    let used = invites
        .iter()
        .find(|i| i.id == invite_for_v)
        .ok_or("invite the user accepted must stay")?;
    eq!("invites.used_by set NULL", used.used_by, None)?;
    eq!("invites.used_at kept", used.used_at, Some(T0 + 1))?;
    let m = ok("get_media", db.get_media(mid).await)?;
    eq!("media.uploaded_by set NULL", m.uploaded_by, None)?;
    let revs = ok("list_revisions", db.list_revisions(pid).await)?;
    eq!(
        "revisions.author_id set NULL",
        revs.iter().map(|r| r.author_id).collect::<Vec<_>>(),
        vec![None],
    )?;
    let log = ok("query_log", db.query_log(all_log()).await)?;
    let entry = log
        .iter()
        .find(|e| e.id == log_id)
        .ok_or("log entry must stay")?;
    eq!("system_log.actor_id set NULL", entry.event.actor_id(), None)
}

/// The deleted user and every sign-in row of theirs (ON DELETE CASCADE).
async fn sign_in_rows_gone<S: Store>(db: &S, v: UserId) -> Check {
    fails(
        "get_user deleted",
        db.get_user(v).await,
        ErrorCode::UserNotFound,
    )?;
    eq!(
        "sessions cascade",
        ok("find_session", db.find_session("sess-v", T0).await)?,
        None,
    )?;
    eq!(
        "passkeys cascade",
        ok("find_passkey", db.find_passkey("cred-v").await)?,
        None,
    )?;
    eq!(
        "tokens cascade",
        ok("find_token", db.find_token("tok-v", T0).await)?,
        None,
    )
}

fn all_log() -> LogQuery {
    LogQuery {
        kind: None,
        actor: None,
        from_ms: None,
        to_ms: None,
        before: None,
        limit: 100,
    }
}

fn passkey(user: UserId, cred: &str) -> NewPasskey {
    NewPasskey {
        user_id: user,
        credential_id: cred.into(),
        public_key: "pk".into(),
        sign_count: 0,
        name: "laptop".into(),
        created_at: T0,
        proof: crate::Proof::Unchecked,
    }
}

fn token(user: UserId, hash: &str, expires_at: Option<i64>) -> NewToken {
    NewToken {
        user_id: user,
        name: "ci".into(),
        token_hash: hash.into(),
        scope: TokenScope::Writer,
        created_at: T0,
        expires_at,
        proof: crate::Proof::Unchecked,
    }
}

fn session(user: UserId, hash: &str, expires_at: i64) -> NewSession {
    NewSession {
        token_hash: hash.into(),
        user_id: user,
        created_at: T0,
        expires_at,
        proof: crate::Proof::Unchecked,
    }
}

pub async fn sessions<S: Store>(db: &S) -> Check {
    let a = user(db, "alice").await?;
    let b = user(db, "bob").await?;
    let missing = UserId(a.0.max(b.0) + 1000);
    ok(
        "create_session",
        db.create_session(session(a, "s1", 2000)).await,
    )?;
    ok(
        "create_session",
        db.create_session(session(a, "s2", 3000)).await,
    )?;
    ok(
        "create_session",
        db.create_session(session(b, "s3", 2000)).await,
    )?;
    fails(
        "create_session duplicate hash",
        db.create_session(session(b, "s1", 5000)).await,
        ErrorCode::Conflict,
    )?;
    fails(
        "create_session missing user",
        db.create_session(session(missing, "s9", 5000)).await,
        ErrorCode::InvalidInput,
    )?;
    let found = ok("find_session", db.find_session("s1", 1999).await)?
        .ok_or("find_session before expiry: expected Some, got None")?;
    eq!("session user", found.user_id, a)?;
    eq!("session expires_at", found.expires_at, 2000)?;
    eq!(
        "find_session at exactly expires_at",
        ok("find_session", db.find_session("s1", 2000).await)?,
        None,
    )?;
    eq!(
        "find_session unknown",
        ok("find_session", db.find_session("nope", 0).await)?,
        None,
    )?;
    eq!(
        "delete_expired_sessions at 2000 (expires_at <= now)",
        ok(
            "delete_expired_sessions",
            db.delete_expired_sessions(2000).await,
        )?,
        2,
    )?;
    that(
        "unexpired session survives",
        ok("find_session", db.find_session("s2", 2000).await)?.is_some(),
    )?;
    ok(
        "create_session",
        db.create_session(session(a, "s4", 9000)).await,
    )?;
    ok("delete_session", db.delete_session("s4").await)?;
    eq!(
        "deleted session",
        ok("find_session", db.find_session("s4", 0).await)?,
        None,
    )?;
    ok("delete_session absent", db.delete_session("s4").await)?;
    ok(
        "create_session",
        db.create_session(session(b, "s5", 9000)).await,
    )?;
    eq!(
        "delete_user_sessions count",
        ok("delete_user_sessions", db.delete_user_sessions(a).await)?,
        1,
    )?;
    eq!(
        "other user'db session kept",
        ok("find_session", db.find_session("s5", 0).await)?.map(|x| x.user_id),
        Some(b),
    )?;
    eq!(
        "delete_user_sessions none left",
        ok("delete_user_sessions", db.delete_user_sessions(a).await)?,
        0,
    )
}

pub async fn passkeys<S: Store>(db: &S) -> Check {
    let a = user(db, "alice").await?;
    let missing = UserId(a.0 + 1000);
    let k1 = ok("add_passkey", db.add_passkey(passkey(a, "c1")).await)?;
    let k2 = ok("add_passkey", db.add_passkey(passkey(a, "c2")).await)?;
    fails(
        "add_passkey duplicate credential",
        db.add_passkey(passkey(a, "c1")).await,
        ErrorCode::Conflict,
    )?;
    fails(
        "add_passkey missing user",
        db.add_passkey(passkey(missing, "c3")).await,
        ErrorCode::InvalidInput,
    )?;
    let found = ok("find_passkey", db.find_passkey("c1").await)?.ok_or("find_passkey c1: None")?;
    eq!("passkey id", found.id, k1)?;
    eq!("passkey last_used_at", found.last_used_at, None)?;
    eq!(
        "find_passkey unknown",
        ok("find_passkey", db.find_passkey("zz").await)?,
        None,
    )?;
    let ids: Vec<i64> = ok("list_passkeys", db.list_passkeys(a).await)?
        .iter()
        .map(|p| p.id)
        .collect();
    eq!("list_passkeys by id", ids, super::kit::asc(&[k1, k2]))?;
    eq!(
        "touch_passkey accepts a higher counter",
        ok("touch_passkey", db.touch_passkey(k1, 7, T0 + 1).await)?,
        true,
    )?;
    let t = ok("find_passkey", db.find_passkey("c1").await)?.ok_or("c1 gone")?;
    eq!("touched", (t.sign_count, t.last_used_at), (7, Some(T0 + 1)))?;
    let gone = k1.max(k2) + 1000;
    fails(
        "touch_passkey missing",
        db.touch_passkey(gone, 1, 1).await,
        ErrorCode::NotFound,
    )?;
    ok("delete_passkey", db.delete_passkey(k1).await)?;
    fails(
        "delete_passkey again",
        db.delete_passkey(k1).await,
        ErrorCode::NotFound,
    )?;
    eq!(
        "list after delete",
        ok("list_passkeys", db.list_passkeys(a).await)?.len(),
        1,
    )
}

/// Only the owner of a passkey removes it with `delete_own_passkey`, and a passwordless
/// account keeps its last one; an account with a password may remove all of them.
pub async fn delete_own_passkey<S: Store>(db: &S) -> Check {
    let a = user(db, "pia").await?;
    let b = user(db, "ben").await?;
    let a1 = ok("add_passkey", db.add_passkey(passkey(a, "pa1")).await)?;
    let a2 = ok("add_passkey", db.add_passkey(passkey(a, "pa2")).await)?;
    let b1 = ok("add_passkey", db.add_passkey(passkey(b, "pb1")).await)?;
    fails(
        "delete_own_passkey of another user",
        db.delete_own_passkey(a, b1).await,
        ErrorCode::NotFound,
    )?;
    fails(
        "delete_own_passkey missing",
        db.delete_own_passkey(a, a1.max(a2).max(b1) + 1000).await,
        ErrorCode::NotFound,
    )?;
    ok("delete_own_passkey a1", db.delete_own_passkey(a, a1).await)?;
    fails(
        "delete_own_passkey of the last passkey, no password",
        db.delete_own_passkey(a, a2).await,
        ErrorCode::Conflict,
    )?;
    fails(
        "delete_own_passkey of b's last passkey by a",
        db.delete_own_passkey(a, b1).await,
        ErrorCode::NotFound,
    )?;
    let left: Vec<i64> = ok("list_passkeys", db.list_passkeys(a).await)?
        .iter()
        .map(|p| p.id)
        .collect();
    eq!("refusals keep the passkeys", left, vec![a2])?;
    eq!(
        "b's passkey kept",
        ok("find_passkey", db.find_passkey("pb1").await)?.map(|p| p.id),
        Some(b1),
    )?;
    ok(
        "password set",
        db.set_password(a, Some("h".into()), T0).await,
    )?;
    ok(
        "delete_own_passkey last, with a password",
        db.delete_own_passkey(a, a2).await,
    )?;
    eq!(
        "all gone",
        ok("list_passkeys", db.list_passkeys(a).await)?.len(),
        0,
    )
}

/// The counter never goes back; only a higher value, or 0 after 0, is accepted, and only an
/// accepted sign-in is recorded.
pub async fn passkey_sign_count<S: Store>(db: &S) -> Check {
    let a = user(db, "alice").await?;
    let k = ok("add_passkey", db.add_passkey(passkey(a, "c")).await)?;
    let z = ok("add_passkey", db.add_passkey(passkey(a, "z")).await)?;
    let stored = |cred: &'static str| async move {
        ok("find_passkey", db.find_passkey(cred).await)?
            .map(|p| (p.sign_count, p.last_used_at))
            .ok_or_else(|| format!("passkey {cred} gone"))
    };
    let touch = |id, n, at| async move { ok("touch_passkey", db.touch_passkey(id, n, at).await) };
    eq!("increase is accepted", touch(k, 5, T0 + 1).await?, true)?;
    eq!("increase is stored", stored("c").await?, (5, Some(T0 + 1)))?;
    eq!("equal is refused", touch(k, 5, T0 + 2).await?, false)?;
    eq!(
        "refused equal writes nothing",
        stored("c").await?,
        (5, Some(T0 + 1))
    )?;
    eq!("decrease is refused", touch(k, 3, T0 + 3).await?, false)?;
    eq!(
        "refused decrease writes nothing",
        stored("c").await?,
        (5, Some(T0 + 1))
    )?;
    eq!(
        "zero after a counter is refused",
        touch(k, 0, T0 + 4).await?,
        false
    )?;
    eq!(
        "refused zero writes nothing",
        stored("c").await?,
        (5, Some(T0 + 1))
    )?;
    eq!("zero-zero is accepted", touch(z, 0, T0 + 5).await?, true)?;
    eq!(
        "zero-zero again is accepted",
        touch(z, 0, T0 + 6).await?,
        true
    )?;
    eq!("zero-zero is stored", stored("z").await?, (0, Some(T0 + 6)))?;
    fails(
        "touch_passkey missing",
        db.touch_passkey(k.max(z) + 1000, 9, T0).await,
        ErrorCode::NotFound,
    )
}

pub async fn tokens<S: Store>(db: &S) -> Check {
    let a = user(db, "alice").await?;
    let missing = UserId(a.0 + 1000);
    let t1 = ok("create_token", db.create_token(token(a, "h1", None)).await)?;
    let t2 = ok(
        "create_token",
        db.create_token(token(a, "h2", Some(2000))).await,
    )?;
    fails(
        "create_token duplicate hash",
        db.create_token(token(a, "h1", None)).await,
        ErrorCode::Conflict,
    )?;
    fails(
        "create_token missing user",
        db.create_token(token(missing, "h3", None)).await,
        ErrorCode::InvalidInput,
    )?;
    let never = ok("find_token", db.find_token("h1", i64::MAX / 2).await)?;
    eq!("token without expiry", never.map(|t| t.id), Some(t1))?;
    let before = ok("find_token", db.find_token("h2", 1999).await)?;
    eq!(
        "token before expiry",
        before.map(|t| t.scope),
        Some(TokenScope::Writer),
    )?;
    eq!(
        "token at exactly expires_at",
        ok("find_token", db.find_token("h2", 2000).await)?,
        None,
    )?;
    ok("revoke_token", db.revoke_token(t1, T0 + 1).await)?;
    eq!(
        "revoked token",
        ok("find_token", db.find_token("h1", T0).await)?,
        None,
    )?;
    ok("revoke_token twice", db.revoke_token(t1, T0 + 9).await)?;
    let list = ok("list_tokens", db.list_tokens(a).await)?;
    eq!(
        "list_tokens: all, by id",
        list.iter().map(|t| t.id).collect::<Vec<_>>(),
        super::kit::asc(&[t1, t2]),
    )?;
    let r = list.iter().find(|t| t.id == t1).ok_or("t1 missing")?;
    eq!(
        "revoking twice keeps the first time",
        r.revoked_at,
        Some(T0 + 1),
    )?;
    fails(
        "revoke_token missing",
        db.revoke_token(t1.max(t2) + 1000, 1).await,
        ErrorCode::NotFound,
    )
}

/// Every scope (read included, allowed by migration 0005) is stored and read back as
/// itself, through create, find and list; the user's role does not change the stored scope.
pub async fn token_scopes_round_trip<S: Store>(db: &S) -> Check {
    let w = user(db, "wanda").await?;
    let mut ids = Vec::new();
    for scope in TokenScope::ALL {
        let hash = format!("scope-{}", scope.as_str());
        let mut t = token(w, &hash, None);
        t.scope = *scope;
        ids.push(ok("create_token", db.create_token(t).await)?);
        let found = ok("find_token", db.find_token(&hash, T0).await)?;
        eq!("find_token scope", found.map(|t| t.scope), Some(*scope))?;
    }
    let list = ok("list_tokens", db.list_tokens(w).await)?;
    eq!(
        "list_tokens scopes",
        list.iter().map(|t| (t.id, t.scope)).collect::<Vec<_>>(),
        ids.iter()
            .copied()
            .zip(TokenScope::ALL.iter().copied())
            .collect::<Vec<_>>(),
    )
}

fn invite(by: UserId, code: &str, role: Role, created_at: i64, expires_at: i64) -> NewInvite {
    NewInvite {
        code_hash: code.into(),
        role,
        created_by: by,
        created_at,
        expires_at,
        proof: crate::Proof::Unchecked,
    }
}

pub async fn invites<S: Store>(db: &S) -> Check {
    let o = user(db, "owner").await?;
    let missing = UserId(o.0 + 1000);
    let i1 = ok(
        "create_invite",
        db.create_invite(invite(o, "c1", Role::Owner, T0, 2000))
            .await,
    )?;
    let i2 = ok(
        "create_invite",
        db.create_invite(invite(o, "c2", Role::Writer, T0 + 1, 2000))
            .await,
    )?;
    let i3 = ok(
        "create_invite",
        db.create_invite(invite(o, "c3", Role::Writer, T0 + 1, 2000))
            .await,
    )?;
    fails(
        "create_invite duplicate hash",
        db.create_invite(invite(o, "c1", Role::Writer, T0, 2000))
            .await,
        ErrorCode::Conflict,
    )?;
    fails(
        "create_invite missing creator",
        db.create_invite(invite(missing, "c9", Role::Writer, T0, 2000))
            .await,
        ErrorCode::InvalidInput,
    )?;
    let order: Vec<i64> = ok("list_invites", db.list_invites().await)?
        .iter()
        .map(|i| i.id)
        .collect();
    let mut want = super::kit::desc(&[i2, i3]);
    want.push(i1);
    eq!("list_invites newest first (created_at, id)", order, want)?;

    let mut nu = new_user("newbie");
    nu.role = Role::Writer;
    let u = ok("accept_invite", db.accept_invite("c1", nu, 1500).await)?;
    let got = ok("get_user", db.get_user(u).await)?;
    eq!(
        "accepted user gets the invite'db role",
        got.role,
        Role::Owner,
    )?;
    let used = ok("list_invites", db.list_invites().await)?;
    let i = used
        .iter()
        .find(|i| i.id == i1)
        .ok_or("invite c1 missing")?;
    eq!(
        "invite marked used",
        (i.used_at, i.used_by),
        (Some(1500), Some(u)),
    )?;

    fails(
        "accept_invite used",
        db.accept_invite("c1", new_user("again"), 1500).await,
        ErrorCode::InviteInvalid,
    )?;
    fails(
        "accept_invite unknown",
        db.accept_invite("nope", new_user("x1"), 1500).await,
        ErrorCode::InviteInvalid,
    )?;
    fails(
        "accept_invite at exactly expires_at",
        db.accept_invite("c2", new_user("x2"), 2000).await,
        ErrorCode::InviteInvalid,
    )?;
    eq!(
        "invalid invites create no user",
        ok("list_users", db.list_users().await)?.len(),
        2,
    )?;

    ok("delete_invite", db.delete_invite(i3).await)?;
    fails(
        "delete_invite again",
        db.delete_invite(i3).await,
        ErrorCode::NotFound,
    )?;
    fails(
        "accept deleted invite",
        db.accept_invite("c3", new_user("x3"), 1500).await,
        ErrorCode::InviteInvalid,
    )
}

/// A login or slug conflict leaves the invite unused and adds no user.
pub async fn accept_invite_conflict_is_atomic<S: Store>(db: &S) -> Check {
    let o = user(db, "owner").await?;
    let id = ok(
        "create_invite",
        db.create_invite(invite(o, "c", Role::Writer, T0, 5000))
            .await,
    )?;
    fails(
        "accept_invite with taken login (other case)",
        db.accept_invite("c", new_user("OWNER"), 2000).await,
        ErrorCode::Conflict,
    )?;
    let mut slug_taken = new_user("fresh");
    slug_taken.slug = "owner".into();
    fails(
        "accept_invite with taken slug",
        db.accept_invite("c", slug_taken, 2000).await,
        ErrorCode::Conflict,
    )?;
    let i = ok("list_invites", db.list_invites().await)?;
    let i = i.iter().find(|x| x.id == id).ok_or("invite missing")?;
    eq!("invite still unused", (i.used_at, i.used_by), (None, None))?;
    eq!(
        "no user added",
        ok("list_users", db.list_users().await)?.len(),
        1,
    )?;
    ok(
        "accept_invite after fixing the login",
        db.accept_invite("c", new_user("fresh"), 2000).await,
    )
    .map(drop)
}
