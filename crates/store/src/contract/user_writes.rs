//! Narrow user writes: each touches only its columns, so writes that race never undo each
//! other, and the site always keeps an active owner. Races are run as their sequential
//! outcome: the second write lands after the first.

use plumblog_model::{ErrorCode, Role, TokenScope, UserId};

use super::kit::{ADMIN, Check, Got, T0, eq, fails, new_user, ok, that, user, version, version_is};
use crate::{NewSession, NewToken, ProfileUpdate, Store, User};

async fn owner<S: Store>(db: &S, login: &str) -> Got<UserId> {
    let mut nu = new_user(login);
    nu.role = Role::Owner;
    nu.password_hash = Some("old".into());
    ok("create_user owner", db.create_user(nu).await)
}

async fn get<S: Store>(db: &S, id: UserId) -> Got<User> {
    ok("get_user", db.get_user(id).await)
}

fn bio(u: &User, bio: &str) -> ProfileUpdate {
    ProfileUpdate {
        login: u.login.clone(),
        slug: u.slug.clone(),
        display_name: u.display_name.clone(),
        bio: bio.into(),
    }
}

/// Each setter changes its column and `updated_at`, nothing else; only the profile bumps.
pub async fn user_narrow_writes<S: Store>(db: &S) -> Check {
    let _root = owner(db, "root").await?;
    let a = owner(db, "alice").await?;
    let before = get(db, a).await?;
    let v = version(db).await?;

    ok(
        "set_password",
        db.set_password(a, Some("new".into()), T0 + 1).await,
    )?;
    let mut want = before.clone();
    want.password_hash = Some("new".into());
    want.updated_at = T0 + 1;
    eq!(
        "set_password writes only the hash",
        get(db, a).await?,
        want.clone()
    )?;
    ok("set_password none", db.set_password(a, None, T0 + 2).await)?;
    want.password_hash = None;
    want.updated_at = T0 + 2;
    eq!(
        "set_password None: passkey-only",
        get(db, a).await?,
        want.clone()
    )?;

    ok(
        "set_role",
        db.set_role(&ADMIN, a, Role::Writer, T0 + 3).await,
    )?;
    want.role = Role::Writer;
    want.updated_at = T0 + 3;
    eq!(
        "set_role writes only the role",
        get(db, a).await?,
        want.clone()
    )?;
    ok(
        "set_role owner",
        db.set_role(&ADMIN, a, Role::Owner, T0 + 4).await,
    )?;
    want.role = Role::Owner;
    want.updated_at = T0 + 4;
    eq!("set_role back to owner", get(db, a).await?, want.clone())?;

    ok(
        "set_disabled",
        db.set_disabled(&ADMIN, a, Some(T0 + 5), T0 + 6).await,
    )?;
    want.disabled_at = Some(T0 + 5);
    want.updated_at = T0 + 6;
    eq!(
        "set_disabled writes only disabled_at",
        get(db, a).await?,
        want.clone()
    )?;
    ok(
        "set_disabled none",
        db.set_disabled(&ADMIN, a, None, T0 + 7).await,
    )?;
    want.disabled_at = None;
    want.updated_at = T0 + 7;
    eq!(
        "set_disabled None re-enables",
        get(db, a).await?,
        want.clone()
    )?;
    version_is(db, "password, role and disabled writes", v).await?;

    ok(
        "update_profile",
        db.update_profile(a, bio(&want, "hi"), T0 + 8).await,
    )?;
    want.bio = "hi".into();
    want.updated_at = T0 + 8;
    eq!(
        "update_profile writes only the profile",
        get(db, a).await?,
        want
    )?;
    version_is(db, "update_profile", v + 1).await
}

/// A write never restores a column another write changed in between.
pub async fn user_writes_do_not_undo_each_other<S: Store>(db: &S) -> Check {
    let _root = owner(db, "root").await?;
    let a = owner(db, "alice").await?;
    // An admin opened the profile form before the password and disabled changes.
    let form = get(db, a).await?;

    ok(
        "set_password",
        db.set_password(a, Some("new".into()), T0 + 1).await,
    )?;
    ok(
        "update_profile",
        db.update_profile(a, bio(&form, "one"), T0 + 2).await,
    )?;
    let got = get(db, a).await?;
    eq!(
        "profile update after a password change keeps the new password",
        got.password_hash.as_deref(),
        Some("new"),
    )?;

    ok(
        "set_disabled",
        db.set_disabled(&ADMIN, a, Some(T0 + 3), T0 + 3).await,
    )?;
    ok(
        "update_profile",
        db.update_profile(a, bio(&form, "two"), T0 + 4).await,
    )?;
    let got = get(db, a).await?;
    eq!(
        "profile update after disable keeps the account disabled",
        got.disabled_at,
        Some(T0 + 3),
    )?;

    ok(
        "set_role",
        db.set_role(&ADMIN, a, Role::Writer, T0 + 5).await,
    )?;
    let got = get(db, a).await?;
    eq!(
        "set_role keeps password, disabled and profile",
        (
            got.password_hash.as_deref(),
            got.disabled_at,
            got.bio.as_str()
        ),
        (Some("new"), Some(T0 + 3), "two"),
    )?;

    ok(
        "set_password",
        db.set_password(a, Some("newer".into()), T0 + 6).await,
    )?;
    let got = get(db, a).await?;
    eq!(
        "set_password keeps role, disabled and profile",
        (got.role, got.disabled_at, got.bio.as_str()),
        (Role::Writer, Some(T0 + 3), "two"),
    )?;

    ok(
        "set_disabled none",
        db.set_disabled(&ADMIN, a, None, T0 + 7).await,
    )?;
    let got = get(db, a).await?;
    eq!(
        "set_disabled keeps role, password and profile",
        (got.role, got.password_hash.as_deref(), got.bio.as_str()),
        (Role::Writer, Some("newer"), "two"),
    )
}

/// Disabling ends every session of that user, and only theirs.
pub async fn disable_ends_sessions<S: Store>(db: &S) -> Check {
    let _root = owner(db, "root").await?;
    let a = user(db, "alice").await?;
    let b = user(db, "bob").await?;
    for (u, hash) in [(a, "a1"), (a, "a2"), (b, "b1")] {
        ok(
            "create_session",
            db.create_session(NewSession {
                token_hash: hash.into(),
                user_id: u,
                created_at: T0,
                expires_at: T0 + 100,
                proof: crate::Proof::Unchecked,
            })
            .await,
        )?;
    }
    ok(
        "set_disabled",
        db.set_disabled(&ADMIN, a, Some(T0), T0).await,
    )?;
    for hash in ["a1", "a2"] {
        eq!(
            "disabled user's session is gone",
            ok("find_session", db.find_session(hash, T0).await)?.map(|s| s.id),
            None,
        )?;
    }
    eq!(
        "other user's session stays",
        ok("find_session", db.find_session("b1", T0).await)?.map(|s| s.user_id),
        Some(b),
    )?;
    // Re-enabling does not bring sessions back; the user signs in again.
    ok(
        "set_disabled none",
        db.set_disabled(&ADMIN, a, None, T0 + 1).await,
    )?;
    eq!(
        "re-enabled user has no session",
        ok("find_session", db.find_session("a1", T0).await)?.map(|s| s.id),
        None,
    )
}

/// The last active owner cannot demote, disable or delete themselves; nothing is written.
pub async fn last_owner_is_kept<S: Store>(db: &S) -> Check {
    let a = owner(db, "alice").await?;
    ok(
        "create_session",
        db.create_session(NewSession {
            token_hash: "sa".into(),
            user_id: a,
            created_at: T0,
            expires_at: T0 + 100,
            proof: crate::Proof::Unchecked,
        })
        .await,
    )?;
    let before = get(db, a).await?;
    fails(
        "last owner demotes themselves",
        db.set_role(&ADMIN, a, Role::Writer, T0 + 1).await,
        ErrorCode::LastOwner,
    )?;
    fails(
        "last owner disables themselves",
        db.set_disabled(&ADMIN, a, Some(T0 + 1), T0 + 1).await,
        ErrorCode::LastOwner,
    )?;
    fails(
        "last owner deletes themselves",
        db.delete_user(&ADMIN, a, T0).await,
        ErrorCode::LastOwner,
    )?;
    eq!("refused writes change nothing", get(db, a).await?, before)?;
    eq!(
        "refused disable keeps the session",
        ok("find_session", db.find_session("sa", T0).await)?.map(|s| s.user_id),
        Some(a),
    )?;
    // No-op writes on the last owner are fine.
    ok(
        "set_role owner to owner",
        db.set_role(&ADMIN, a, Role::Owner, T0 + 2).await,
    )?;
    ok(
        "set_disabled none",
        db.set_disabled(&ADMIN, a, None, T0 + 2).await,
    )
}

/// With two owners one can go; then the remaining one cannot. A disabled owner does not
/// count, and changing a disabled owner never leaves the site with fewer active owners.
pub async fn second_owner_can_be_demoted<S: Store>(db: &S) -> Check {
    let a = owner(db, "alice").await?;
    let b = owner(db, "bob").await?;
    ok(
        "demote one of two owners",
        db.set_role(&ADMIN, b, Role::Writer, T0).await,
    )?;
    fails(
        "then demote the remaining owner",
        db.set_role(&ADMIN, a, Role::Writer, T0).await,
        ErrorCode::LastOwner,
    )?;
    ok(
        "promote again",
        db.set_role(&ADMIN, b, Role::Owner, T0).await,
    )?;
    ok(
        "disable one of two owners",
        db.set_disabled(&ADMIN, b, Some(T0), T0).await,
    )?;
    fails(
        "a disabled owner does not count: demote",
        db.set_role(&ADMIN, a, Role::Writer, T0).await,
        ErrorCode::LastOwner,
    )?;
    fails(
        "a disabled owner does not count: disable",
        db.set_disabled(&ADMIN, a, Some(T0), T0).await,
        ErrorCode::LastOwner,
    )?;
    fails(
        "a disabled owner does not count: delete",
        db.delete_user(&ADMIN, a, T0).await,
        ErrorCode::LastOwner,
    )?;
    ok(
        "demote the disabled owner",
        db.set_role(&ADMIN, b, Role::Writer, T0).await,
    )?;
    ok(
        "set_role owner",
        db.set_role(&ADMIN, b, Role::Owner, T0).await,
    )?;
    ok(
        "delete the disabled owner",
        db.delete_user(&ADMIN, b, T0).await,
    )?;
    let c = owner(db, "carol").await?;
    ok(
        "delete one of two active owners",
        db.delete_user(&ADMIN, c, T0).await,
    )?;
    let w = user(db, "walt").await?;
    ok("delete a writer", db.delete_user(&ADMIN, w, T0).await)?;
    eq!("the last owner stays", get(db, a).await?.role, Role::Owner)
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

/// A disabled user's tokens and sessions do not resolve, and re-enabling brings neither back.
pub async fn disabled_user_credentials_do_not_resolve<S: Store>(db: &S) -> Check {
    let _root = owner(db, "root").await?;
    let a = user(db, "alice").await?;
    let b = user(db, "bob").await?;
    ok("create_token", db.create_token(token(a, "ta", None)).await)?;
    ok(
        "create_token",
        db.create_token(token(a, "te", Some(T0 + 10))).await,
    )?;
    let revoked = ok("create_token", db.create_token(token(a, "tr", None)).await)?;
    ok("revoke_token", db.revoke_token(revoked, T0).await)?;
    ok("create_token", db.create_token(token(b, "tb", None)).await)?;
    let found = |hash: &'static str, now| async move {
        ok("find_token", db.find_token(hash, now).await).map(|t| t.map(|t| t.user_id))
    };
    eq!(
        "active user's token resolves",
        found("ta", T0).await?,
        Some(a)
    )?;

    ok(
        "set_disabled",
        db.set_disabled(&ADMIN, a, Some(T0), T0).await,
    )?;
    eq!(
        "disabled user's token does not resolve",
        found("ta", T0).await?,
        None
    )?;
    eq!(
        "other user's token still resolves",
        found("tb", T0).await?,
        Some(b)
    )?;
    ok(
        "create_session for a disabled user",
        db.create_session(NewSession {
            token_hash: "sa".into(),
            user_id: a,
            created_at: T0,
            expires_at: T0 + 100,
            proof: crate::Proof::Unchecked,
        })
        .await,
    )?;
    eq!(
        "disabled user's session does not resolve",
        ok("find_session", db.find_session("sa", T0).await)?.map(|s| s.id),
        None,
    )?;

    ok(
        "set_disabled none",
        db.set_disabled(&ADMIN, a, None, T0 + 1).await,
    )?;
    eq!(
        "disabling revoked the token: re-enabling does not bring it back",
        found("ta", T0 + 1).await?,
        None
    )?;
    eq!(
        "revoked token stays revoked",
        found("tr", T0 + 1).await?,
        None
    )?;
    eq!(
        "expired token stays expired",
        found("te", T0 + 10).await?,
        None
    )
}

fn session(user: UserId, hash: &str) -> NewSession {
    NewSession {
        token_hash: hash.into(),
        user_id: user,
        created_at: T0,
        expires_at: T0 + 1_000_000,
        proof: crate::Proof::Unchecked,
    }
}

/// A password (and login) change ends every session of the user, keeps other users' ones,
/// creates the rotated session, and on any refusal changes nothing.
pub async fn change_password_rotates_sessions<S: Store>(db: &S) -> Check {
    let root = owner(db, "root").await?;
    let a = owner(db, "alice").await?;
    ok("session a1", db.create_session(session(a, "a1")).await)?;
    ok("session a2", db.create_session(session(a, "a2")).await)?;
    ok("session root", db.create_session(session(root, "r1")).await)?;
    let before = get(db, a).await?;
    let v = version(db).await?;

    // Refusals first: each must leave the user, sessions and version as they were.
    let mut taken = bio(&before, "x");
    taken.login = "ROOT".into();
    fails(
        "change_password onto a taken login",
        db.change_password(a, Some("h".into()), Some(taken), session(a, "a3"), T0 + 1)
            .await,
        ErrorCode::Conflict,
    )?;
    fails(
        "change_password with a session of another user",
        db.change_password(a, Some("h".into()), None, session(root, "a3"), T0 + 1)
            .await,
        ErrorCode::InvalidInput,
    )?;
    fails(
        "change_password reusing another user's session hash",
        db.change_password(a, Some("h".into()), None, session(a, "r1"), T0 + 1)
            .await,
        ErrorCode::Conflict,
    )?;
    fails(
        "change_password of a missing user",
        db.change_password(
            UserId(999),
            Some("h".into()),
            None,
            session(UserId(999), "z"),
            T0 + 1,
        )
        .await,
        ErrorCode::UserNotFound,
    )?;
    eq!(
        "refused changes leave the user",
        get(db, a).await?,
        before.clone()
    )?;
    for h in ["a1", "a2", "r1"] {
        let found = ok("find_session", db.find_session(h, T0).await)?;
        eq!("refused changes keep sessions", found.is_some(), true)?;
    }
    version_is(db, "refused changes", v).await?;

    // Password only: no version bump; a's sessions replaced by the new one; root's kept.
    let sid = ok(
        "change_password",
        db.change_password(a, Some("p2".into()), None, session(a, "a3"), T0 + 2)
            .await,
    )?;
    let mut want = before.clone();
    want.password_hash = Some("p2".into());
    want.updated_at = T0 + 2;
    eq!("password written", get(db, a).await?, want.clone())?;
    version_is(db, "password-only change", v).await?;
    for (h, alive) in [("a1", false), ("a2", false), ("a3", true), ("r1", true)] {
        let found = ok("find_session", db.find_session(h, T0).await)?;
        eq!(
            &format!("session {h} alive after change"),
            found.is_some(),
            alive
        )?;
    }
    let s = ok("find_session a3", db.find_session("a3", T0).await)?;
    eq!("returned id is the new session", s.map(|s| s.id), Some(sid))?;

    // With a profile: login and password land together and the version bumps.
    let mut p = bio(&want, "b");
    p.login = "alicia".into();
    ok(
        "change_password with profile",
        db.change_password(a, Some("p3".into()), Some(p), session(a, "a4"), T0 + 3)
            .await,
    )?;
    want.login = "alicia".into();
    want.bio = "b".into();
    want.password_hash = Some("p3".into());
    want.updated_at = T0 + 3;
    eq!("login and password written", get(db, a).await?, want)?;
    let old = ok("find_session a3", db.find_session("a3", T0).await)?;
    eq!("previous session ended", old.is_some(), false)?;
    version_is(db, "login change", v + 1).await
}

/// A passkey-only account changes its login with `hash = None`: sessions rotate, no password
/// appears. Once the account has a password, the same call is refused and changes nothing.
pub async fn change_login_passkey_only<S: Store>(db: &S) -> Check {
    let a = owner(db, "pk").await?;
    ok("passkey-only", db.set_password(a, None, T0).await)?;
    ok("session p1", db.create_session(session(a, "p1")).await)?;
    let before = get(db, a).await?;
    let mut p = bio(&before, "");
    p.login = "pk2".into();
    ok(
        "passkey-only login change",
        db.change_password(a, None, Some(p.clone()), session(a, "p2"), T0 + 1)
            .await,
    )?;
    let after = get(db, a).await?;
    eq!("login written", after.login.as_str(), "pk2")?;
    eq!("still no password", after.password_hash, None)?;
    let old = ok("find_session p1", db.find_session("p1", T0).await)?;
    eq!("previous session ended", old.is_some(), false)?;

    ok(
        "password set",
        db.set_password(a, Some("h".into()), T0 + 2).await,
    )?;
    let before = get(db, a).await?;
    p.login = "pk3".into();
    fails(
        "passkey-only change of an account with a password",
        db.change_password(a, None, Some(p), session(a, "p3"), T0 + 3)
            .await,
        ErrorCode::Conflict,
    )?;
    eq!("refused change leaves the user", get(db, a).await?, before)?;
    let kept = ok("find_session p2", db.find_session("p2", T0).await)?;
    eq!("refused change keeps the session", kept.is_some(), true)?;
    fails(
        "passkey-only change of a missing user",
        db.change_password(UserId(999), None, None, session(UserId(999), "z"), T0)
            .await,
        ErrorCode::UserNotFound,
    )
}

/// Disabling revokes every unrevoked token of that user at the disable time, in the same
/// write; an earlier revocation keeps its time, other users' tokens stay, and a refused
/// disable (the last owner) revokes nothing.
pub async fn disable_revokes_tokens<S: Store>(db: &S) -> Check {
    let root = owner(db, "root").await?;
    let a = user(db, "alice").await?;
    let b = user(db, "bob").await?;
    let ta = ok("create_token", db.create_token(token(a, "ta", None)).await)?;
    let tr = ok("create_token", db.create_token(token(a, "tr", None)).await)?;
    ok("revoke_token", db.revoke_token(tr, T0).await)?;
    let tb = ok("create_token", db.create_token(token(b, "tb", None)).await)?;
    let troot = ok(
        "create_token",
        db.create_token(token(root, "tr0", None)).await,
    )?;
    ok(
        "set_disabled",
        db.set_disabled(&ADMIN, a, Some(T0 + 5), T0 + 5).await,
    )?;
    let revoked = |list: Vec<crate::ApiToken>, id: i64| {
        list.into_iter().find(|t| t.id == id).map(|t| t.revoked_at)
    };
    let mine = ok("list_tokens", db.list_tokens(a).await)?;
    eq!(
        "active token revoked at the disable time",
        revoked(mine.clone(), ta),
        Some(Some(T0 + 5))
    )?;
    eq!(
        "earlier revocation keeps its time",
        revoked(mine, tr),
        Some(Some(T0))
    )?;
    let theirs = ok("list_tokens", db.list_tokens(b).await)?;
    eq!("other user's token stays", revoked(theirs, tb), Some(None))?;
    fails(
        "last owner cannot be disabled",
        db.set_disabled(&ADMIN, root, Some(T0 + 6), T0 + 6).await,
        ErrorCode::LastOwner,
    )?;
    let kept = ok("list_tokens", db.list_tokens(root).await)?;
    eq!(
        "refused disable revokes nothing",
        revoked(kept, troot),
        Some(None)
    )
}

fn session_of(user: UserId, hash: &str) -> NewSession {
    NewSession {
        token_hash: hash.into(),
        user_id: user,
        created_at: T0,
        expires_at: T0 + 1000,
        proof: crate::Proof::Unchecked,
    }
}

fn invite_by(by: &crate::Admin, hash: &str) -> crate::NewInvite {
    crate::NewInvite {
        code_hash: hash.into(),
        role: Role::Owner,
        created_by: by.id,
        created_at: T0,
        expires_at: T0 + 100,
        proof: by.proof.clone(),
    }
}

/// A user-admin write checks its actor inside the write: an owner demoted or disabled after
/// their request was authorized writes nothing (403), and one whose credential is gone is 401.
pub async fn stale_admin_writes_nothing<S: Store>(db: &S) -> Check {
    use crate::{Admin, Proof};
    let [_, b, c, _, w] = admins(db).await?;
    let by_b = Admin {
        id: b,
        proof: Proof::Session("sb".into()),
    };
    ok(
        "an active owner's write works",
        db.set_role(&by_b, w, Role::Writer, T0).await,
    )?;
    ok("demote bea", db.set_role(&ADMIN, b, Role::Writer, T0).await)?;
    let users = ok("list_users", db.list_users().await)?;
    fails(
        "demoted: role change",
        db.set_role(&by_b, c, Role::Writer, T0).await,
        ErrorCode::Forbidden,
    )?;
    fails(
        "demoted: promote themself back",
        db.set_role(&by_b, b, Role::Owner, T0).await,
        ErrorCode::Forbidden,
    )?;
    fails(
        "demoted: disable",
        db.set_disabled(&by_b, c, Some(T0), T0).await,
        ErrorCode::Forbidden,
    )?;
    fails(
        "demoted: enable",
        db.set_disabled(&by_b, c, None, T0).await,
        ErrorCode::Forbidden,
    )?;
    fails(
        "demoted: delete",
        db.delete_user(&by_b, w, T0).await,
        ErrorCode::Forbidden,
    )?;
    fails(
        "demoted: invite",
        db.create_invite(invite_by(&by_b, "ib")).await,
        ErrorCode::Forbidden,
    )?;
    let theirs = ok(
        "cid's invite",
        db.create_invite(invite_by(
            &Admin {
                id: c,
                proof: Proof::Session("sc".into()),
            },
            "ic",
        ))
        .await,
    )?;
    fails(
        "demoted: revoke",
        db.delete_invite(&by_b, theirs, T0).await,
        ErrorCode::Forbidden,
    )?;
    eq!(
        "the refused revoke kept the invite",
        ok("list_invites", db.list_invites().await)?
            .iter()
            .map(|i| i.id)
            .collect::<Vec<_>>(),
        vec![theirs]
    )?;
    eq!(
        "a demoted owner's writes changed nothing",
        ok("list_users", db.list_users().await)?,
        users
    )?;
    Ok(())
}

/// Owners root, bea, cid, dov (sessions `sr`, `sb`, `sc`) and writer wes.
async fn admins<S: Store>(db: &S) -> Got<[UserId; 5]> {
    let root = owner(db, "root").await?;
    let b = owner(db, "bea").await?;
    let c = owner(db, "cid").await?;
    let d = owner(db, "dov").await?;
    let w = user(db, "wes").await?;
    for (u, h) in [(root, "sr"), (b, "sb"), (c, "sc")] {
        ok("create_session", db.create_session(session_of(u, h)).await)?;
    }
    Ok([root, b, c, d, w])
}

/// Like [`stale_admin_writes_nothing`] for a disabled owner (403) and for an owner whose
/// credential is gone (401); a live active owner still writes.
pub async fn disabled_admin_writes_nothing<S: Store>(db: &S) -> Check {
    use crate::{Admin, Proof};
    let [root, _, c, d, w] = admins(db).await?;
    // A disabled owner whose password still verifies (sessions went with the disable).
    ok(
        "dov's password",
        db.set_password(d, Some("pw-d".into()), T0).await,
    )?;
    ok(
        "disable dov",
        db.set_disabled(&ADMIN, d, Some(T0), T0).await,
    )?;
    let by_d = Admin {
        id: d,
        proof: Proof::Password("pw-d".into()),
    };
    // The baseline now includes dov's own (allowed) password change and disable.
    let before: Vec<User> = ok("list_users", db.list_users().await)?;
    fails(
        "disabled: role change",
        db.set_role(&by_d, c, Role::Writer, T0).await,
        ErrorCode::Forbidden,
    )?;
    fails(
        "disabled: enable themself",
        db.set_disabled(&by_d, d, None, T0).await,
        ErrorCode::Forbidden,
    )?;
    fails(
        "disabled: invite",
        db.create_invite(invite_by(&by_d, "id")).await,
        ErrorCode::Forbidden,
    )?;
    // Still an owner, but the session the request came with is gone.
    ok("end root's sessions", db.delete_user_sessions(root).await)?;
    let by_root = Admin {
        id: root,
        proof: Proof::Session("sr".into()),
    };
    fails(
        "credential gone: role change",
        db.set_role(&by_root, c, Role::Writer, T0).await,
        ErrorCode::Unauthenticated,
    )?;
    fails(
        "credential gone: invite",
        db.create_invite(invite_by(&by_root, "ir")).await,
        ErrorCode::Unauthenticated,
    )?;
    eq!(
        "refused writes changed no user",
        ok("list_users", db.list_users().await)?,
        before
    )?;
    eq!(
        "refused writes made no invite",
        ok("list_invites", db.list_invites().await)?.len(),
        0
    )?;
    let by_c = Admin {
        id: c,
        proof: Proof::Session("sc".into()),
    };
    ok(
        "a live active owner still writes",
        db.create_invite(invite_by(&by_c, "ic")).await,
    )?;
    ok(
        "and disables",
        db.set_disabled(&by_c, w, Some(T0 + 1), T0 + 1).await,
    )
}

/// Revoking deletes only an unused invite; a used one answers `NotFound` and stays, as the
/// record of how its user joined.
pub async fn revoke_keeps_used_invites<S: Store>(db: &S) -> Check {
    let o = owner(db, "root").await?;
    let by = crate::Admin {
        id: o,
        proof: crate::Proof::Unchecked,
    };
    let used = ok("create_invite", db.create_invite(invite_by(&by, "u")).await)?;
    let unused = ok("create_invite", db.create_invite(invite_by(&by, "n")).await)?;
    ok(
        "accept_invite",
        db.accept_invite("u", new_user("joe"), T0 + 1).await,
    )?;
    fails(
        "revoke a used invite",
        db.delete_invite(&ADMIN, used, T0).await,
        ErrorCode::NotFound,
    )?;
    ok(
        "revoke an unused invite",
        db.delete_invite(&ADMIN, unused, T0).await,
    )?;
    fails(
        "revoke it again",
        db.delete_invite(&ADMIN, unused, T0).await,
        ErrorCode::NotFound,
    )?;
    let left = ok("list_invites", db.list_invites().await)?;
    eq!(
        "the used invite stays",
        left.iter().map(|i| (i.id, i.used_at)).collect::<Vec<_>>(),
        vec![(used, Some(T0 + 1))]
    )
}

/// An owner's unused invites die when that owner is disabled or demoted (the used ones stay,
/// as records), and accepting requires the creator to still be an active owner, whatever
/// path changed them. An active owner's invite still works.
pub async fn invites_die_with_their_owner<S: Store>(db: &S) -> Check {
    let _root = owner(db, "root").await?;
    let a = owner(db, "ada").await?;
    let d = owner(db, "dan").await?;
    let c = owner(db, "cy").await?;
    let w = user(db, "wil").await?;
    let unchecked = |id| crate::Admin {
        id,
        proof: crate::Proof::Unchecked,
    };
    for (by, code) in [(a, "ia"), (a, "ia-used"), (d, "id"), (c, "ic"), (w, "iw")] {
        ok(
            "create_invite",
            db.create_invite(invite_by(&unchecked(by), code)).await,
        )?;
    }
    ok(
        "accept one of ada's",
        db.accept_invite("ia-used", new_user("early"), T0 + 1).await,
    )?;
    let users = ok("list_users", db.list_users().await)?.len();
    ok(
        "disable ada",
        db.set_disabled(&ADMIN, a, Some(T0 + 2), T0 + 2).await,
    )?;
    fails(
        "disabled creator's invite",
        db.accept_invite("ia", new_user("thief"), T0 + 3).await,
        ErrorCode::InviteInvalid,
    )?;
    ok(
        "demote dan",
        db.set_role(&ADMIN, d, Role::Writer, T0 + 2).await,
    )?;
    fails(
        "demoted creator's invite",
        db.accept_invite("id", new_user("thief2"), T0 + 3).await,
        ErrorCode::InviteInvalid,
    )?;
    // Not through a disable or demote: the accept itself checks the creator.
    fails(
        "a writer's invite",
        db.accept_invite("iw", new_user("thief3"), T0 + 3).await,
        ErrorCode::InviteInvalid,
    )?;
    eq!(
        "refused accepts add no user",
        ok("list_users", db.list_users().await)?.len(),
        users
    )?;
    let left: Vec<(UserId, bool)> = ok("list_invites", db.list_invites().await)?
        .iter()
        .map(|i| (i.created_by, i.used_at.is_some()))
        .collect();
    that(
        "ada's and dan's unused invites are gone, the used one stays",
        !left.contains(&(a, false)) && !left.contains(&(d, false)) && left.contains(&(a, true)),
    )?;
    ok(
        "an active owner's invite works",
        db.accept_invite("ic", new_user("guest"), T0 + 3).await,
    )
    .map(drop)
}

/// Demoting an owner revokes their owner-scope tokens in the same write, so promoting them
/// again does not bring those back; their read and writer tokens, and others', stay.
pub async fn demote_revokes_owner_tokens<S: Store>(db: &S) -> Check {
    let root = owner(db, "root").await?;
    let b = owner(db, "bea").await?;
    let scoped = |user, hash: &str, scope| NewToken {
        scope,
        ..token(user, hash, None)
    };
    let tbo = ok(
        "create_token",
        db.create_token(scoped(b, "tbo", TokenScope::Owner)).await,
    )?;
    let tbw = ok(
        "create_token",
        db.create_token(scoped(b, "tbw", TokenScope::Writer)).await,
    )?;
    let tbr = ok(
        "create_token",
        db.create_token(scoped(b, "tbr", TokenScope::Read)).await,
    )?;
    let troot = ok(
        "create_token",
        db.create_token(scoped(root, "tro", TokenScope::Owner))
            .await,
    )?;
    ok("demote", db.set_role(&ADMIN, b, Role::Writer, T0 + 5).await)?;
    ok(
        "promote again",
        db.set_role(&ADMIN, b, Role::Owner, T0 + 6).await,
    )?;
    let revoked =
        |list: &[crate::ApiToken], id: i64| list.iter().find(|t| t.id == id).map(|t| t.revoked_at);
    let mine = ok("list_tokens", db.list_tokens(b).await)?;
    eq!(
        "owner token revoked at the demote time",
        revoked(&mine, tbo),
        Some(Some(T0 + 5))
    )?;
    eq!("writer token stays", revoked(&mine, tbw), Some(None))?;
    eq!("read token stays", revoked(&mine, tbr), Some(None))?;
    eq!(
        "revoked owner token does not resolve after re-promotion",
        ok("find_token", db.find_token("tbo", T0 + 7).await)?.map(|t| t.id),
        None
    )?;
    let theirs = ok("list_tokens", db.list_tokens(root).await)?;
    eq!(
        "the acting owner's token stays",
        revoked(&theirs, troot),
        Some(None)
    )?;
    // Promoting (to owner) revokes nothing.
    let tb2 = ok(
        "create_token",
        db.create_token(scoped(b, "tb2", TokenScope::Owner)).await,
    )?;
    ok(
        "set_role owner to owner",
        db.set_role(&ADMIN, b, Role::Owner, T0 + 8).await,
    )?;
    let after = ok("list_tokens", db.list_tokens(b).await)?;
    eq!(
        "promote keeps owner tokens",
        revoked(&after, tb2),
        Some(None)
    )
}

/// Disabling an owner deletes a pending owner reset bound to them in the same write, so a
/// disable then enable within the hour cannot reuse that code; one bound to another owner
/// stays.
pub async fn disable_drops_owner_reset<S: Store>(db: &S) -> Check {
    let root = owner(db, "root").await?;
    let b = owner(db, "bea").await?;
    let later = T0 + 3_600_000;
    ok(
        "put_owner_reset for root",
        db.put_owner_reset("rc-root", root, later, false, T0).await,
    )?;
    ok(
        "disable bea",
        db.set_disabled(&ADMIN, b, Some(T0 + 1), T0 + 1).await,
    )?;
    eq!(
        "another owner's reset stays",
        ok(
            "take_owner_reset",
            db.take_owner_reset("rc-root", T0 + 2).await
        )?,
        Some(root)
    )?;
    ok("enable bea", db.set_disabled(&ADMIN, b, None, T0 + 2).await)?;
    ok(
        "put_owner_reset for bea",
        db.put_owner_reset("rc-bea", b, later, false, T0 + 3).await,
    )?;
    ok(
        "disable bea",
        db.set_disabled(&ADMIN, b, Some(T0 + 4), T0 + 4).await,
    )?;
    ok("enable bea", db.set_disabled(&ADMIN, b, None, T0 + 5).await)?;
    fails(
        "redeem after disable and enable",
        db.redeem_owner_reset(
            b,
            "new".into(),
            None,
            NewSession {
                token_hash: "sr".into(),
                user_id: b,
                created_at: T0 + 6,
                expires_at: T0 + 1000,
                proof: crate::Proof::ResetCode("rc-bea".into()),
            },
            T0 + 6,
        )
        .await,
        ErrorCode::SetupInvalid,
    )?;
    eq!(
        "the code is gone",
        ok(
            "take_owner_reset",
            db.take_owner_reset("rc-bea", T0 + 6).await
        )?,
        None
    )?;
    eq!(
        "password unchanged",
        get(db, b).await?.password_hash,
        Some("old".into())
    )
}

/// Demoting an owner deletes a pending owner reset bound to them in the same write, so a
/// demote then re-promote within the hour cannot reuse that code; promoting deletes nothing.
pub async fn demote_drops_owner_reset<S: Store>(db: &S) -> Check {
    let root = owner(db, "root").await?;
    let b = owner(db, "bea").await?;
    let later = T0 + 3_600_000;
    ok(
        "put_owner_reset for root",
        db.put_owner_reset("rc-root", root, later, false, T0).await,
    )?;
    ok(
        "demote bea",
        db.set_role(&ADMIN, b, Role::Writer, T0 + 1).await,
    )?;
    eq!(
        "another owner's reset stays",
        ok(
            "take_owner_reset",
            db.take_owner_reset("rc-root", T0 + 2).await
        )?,
        Some(root)
    )?;
    ok(
        "put_owner_reset for bea",
        db.put_owner_reset("rc-bea", b, later, false, T0 + 3).await,
    )?;
    ok(
        "promote bea",
        db.set_role(&ADMIN, b, Role::Owner, T0 + 3).await,
    )?;
    ok(
        "demote bea",
        db.set_role(&ADMIN, b, Role::Writer, T0 + 4).await,
    )?;
    ok(
        "promote bea",
        db.set_role(&ADMIN, b, Role::Owner, T0 + 5).await,
    )?;
    fails(
        "redeem after demote and re-promote",
        db.redeem_owner_reset(
            b,
            "new".into(),
            None,
            NewSession {
                token_hash: "sr".into(),
                user_id: b,
                created_at: T0 + 6,
                expires_at: T0 + 1000,
                proof: crate::Proof::ResetCode("rc-bea".into()),
            },
            T0 + 6,
        )
        .await,
        ErrorCode::SetupInvalid,
    )?;
    eq!(
        "the code is gone",
        ok(
            "take_owner_reset",
            db.take_owner_reset("rc-bea", T0 + 6).await
        )?,
        None
    )?;
    eq!(
        "password unchanged",
        get(db, b).await?.password_hash,
        Some("old".into())
    )
}
