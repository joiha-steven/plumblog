//! Narrow user writes: each touches only its columns, so writes that race never undo each
//! other, and the site always keeps an active owner. Races are run as their sequential
//! outcome: the second write lands after the first.

use plumblog_model::{ErrorCode, Role, UserId};

use super::kit::{Check, Got, T0, eq, fails, new_user, ok, user, version, version_is};
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

    ok("set_role", db.set_role(a, Role::Writer, T0 + 3).await)?;
    want.role = Role::Writer;
    want.updated_at = T0 + 3;
    eq!(
        "set_role writes only the role",
        get(db, a).await?,
        want.clone()
    )?;
    ok("set_role owner", db.set_role(a, Role::Owner, T0 + 4).await)?;
    want.role = Role::Owner;
    want.updated_at = T0 + 4;
    eq!("set_role back to owner", get(db, a).await?, want.clone())?;

    ok(
        "set_disabled",
        db.set_disabled(a, Some(T0 + 5), T0 + 6).await,
    )?;
    want.disabled_at = Some(T0 + 5);
    want.updated_at = T0 + 6;
    eq!(
        "set_disabled writes only disabled_at",
        get(db, a).await?,
        want.clone()
    )?;
    ok("set_disabled none", db.set_disabled(a, None, T0 + 7).await)?;
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
        db.set_disabled(a, Some(T0 + 3), T0 + 3).await,
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

    ok("set_role", db.set_role(a, Role::Writer, T0 + 5).await)?;
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

    ok("set_disabled none", db.set_disabled(a, None, T0 + 7).await)?;
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
            })
            .await,
        )?;
    }
    ok("set_disabled", db.set_disabled(a, Some(T0), T0).await)?;
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
    ok("set_disabled none", db.set_disabled(a, None, T0 + 1).await)?;
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
        })
        .await,
    )?;
    let before = get(db, a).await?;
    fails(
        "last owner demotes themselves",
        db.set_role(a, Role::Writer, T0 + 1).await,
        ErrorCode::Conflict,
    )?;
    fails(
        "last owner disables themselves",
        db.set_disabled(a, Some(T0 + 1), T0 + 1).await,
        ErrorCode::Conflict,
    )?;
    fails(
        "last owner deletes themselves",
        db.delete_user(a).await,
        ErrorCode::Conflict,
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
        db.set_role(a, Role::Owner, T0 + 2).await,
    )?;
    ok("set_disabled none", db.set_disabled(a, None, T0 + 2).await)
}

/// With two owners one can go; then the remaining one cannot. A disabled owner does not
/// count, and changing a disabled owner never leaves the site with fewer active owners.
pub async fn second_owner_can_be_demoted<S: Store>(db: &S) -> Check {
    let a = owner(db, "alice").await?;
    let b = owner(db, "bob").await?;
    ok(
        "demote one of two owners",
        db.set_role(b, Role::Writer, T0).await,
    )?;
    fails(
        "then demote the remaining owner",
        db.set_role(a, Role::Writer, T0).await,
        ErrorCode::Conflict,
    )?;
    ok("promote again", db.set_role(b, Role::Owner, T0).await)?;
    ok(
        "disable one of two owners",
        db.set_disabled(b, Some(T0), T0).await,
    )?;
    fails(
        "a disabled owner does not count: demote",
        db.set_role(a, Role::Writer, T0).await,
        ErrorCode::Conflict,
    )?;
    fails(
        "a disabled owner does not count: disable",
        db.set_disabled(a, Some(T0), T0).await,
        ErrorCode::Conflict,
    )?;
    fails(
        "a disabled owner does not count: delete",
        db.delete_user(a).await,
        ErrorCode::Conflict,
    )?;
    ok(
        "demote the disabled owner",
        db.set_role(b, Role::Writer, T0).await,
    )?;
    ok("set_role owner", db.set_role(b, Role::Owner, T0).await)?;
    ok("delete the disabled owner", db.delete_user(b).await)?;
    let c = owner(db, "carol").await?;
    ok("delete one of two active owners", db.delete_user(c).await)?;
    let w = user(db, "walt").await?;
    ok("delete a writer", db.delete_user(w).await)?;
    eq!("the last owner stays", get(db, a).await?.role, Role::Owner)
}

fn token(user: UserId, hash: &str, expires_at: Option<i64>) -> NewToken {
    NewToken {
        user_id: user,
        name: "ci".into(),
        token_hash: hash.into(),
        scope: Role::Writer,
        created_at: T0,
        expires_at,
    }
}

/// A disabled user's tokens and sessions do not resolve; re-enabling brings back their
/// unrevoked, unexpired tokens.
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

    ok("set_disabled", db.set_disabled(a, Some(T0), T0).await)?;
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
        })
        .await,
    )?;
    eq!(
        "disabled user's session does not resolve",
        ok("find_session", db.find_session("sa", T0).await)?.map(|s| s.id),
        None,
    )?;

    ok("set_disabled none", db.set_disabled(a, None, T0 + 1).await)?;
    eq!(
        "re-enabled user's token resolves again",
        found("ta", T0 + 1).await?,
        Some(a)
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
