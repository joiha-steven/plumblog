//! Owner reset across requests in flight: a credential write that authenticated before the
//! reset creates nothing after it, and the redeem replaces every credential in one write.

use plumblog_model::{ErrorCode, Role, TokenScope, UserId};

use super::kit::{Check, Got, T0, eq, fails, new_user, ok, that};
use crate::{
    NewInvite, NewPasskey, NewSession, NewToken, OWNER_RESET_HASH_KEY, OWNER_RESET_KEYS,
    OWNER_RESET_SINCE_MARGIN_MS, ProfileUpdate, Proof, Store,
};

const LATER: i64 = T0 + 1_000_000;

fn named(v: &[(&str, bool)]) -> Vec<(String, bool)> {
    v.iter().map(|&(n, r)| (n.to_string(), r)).collect()
}
/// The reset happens past the margin after the old credentials, so they count as before it.
const RESET: i64 = T0 + OWNER_RESET_SINCE_MARGIN_MS + 10;

async fn owner<S: Store>(db: &S, login: &str, role: Role) -> Got<UserId> {
    let mut nu = new_user(login);
    nu.role = role;
    nu.password_hash = Some(format!("pw-{login}"));
    ok("create_user", db.create_user(nu).await)
}

fn session(user: UserId, hash: &str, at: i64, proof: Proof) -> NewSession {
    NewSession {
        token_hash: hash.into(),
        user_id: user,
        created_at: at,
        expires_at: LATER,
        proof,
    }
}

fn passkey(user: UserId, cred: &str, at: i64, proof: Proof) -> NewPasskey {
    NewPasskey {
        user_id: user,
        credential_id: cred.into(),
        public_key: "pk".into(),
        sign_count: 0,
        name: cred.into(),
        created_at: at,
        proof,
    }
}

fn token(user: UserId, hash: &str, at: i64, proof: Proof) -> NewToken {
    NewToken {
        user_id: user,
        name: hash.into(),
        token_hash: hash.into(),
        scope: TokenScope::Owner,
        created_at: at,
        expires_at: None,
        proof,
    }
}

fn invite(by: UserId, code: &str, at: i64) -> NewInvite {
    NewInvite {
        code_hash: code.into(),
        role: Role::Owner,
        created_by: by,
        created_at: at,
        expires_at: LATER,
    }
}

async fn live<S: Store>(db: &S, hash: &str) -> Got<bool> {
    Ok(ok("find_session", db.find_session(hash, T0).await)?.is_some())
}

/// Owner `a` (password `pw-a`, session `sa`, a passkey) and writer `b` (session `sb`), and
/// the three proofs a request of `a` may hold.
async fn signed_in<S: Store>(db: &S) -> Got<(UserId, UserId, i64, [Proof; 3])> {
    let a = owner(db, "a", Role::Owner).await?;
    let b = owner(db, "b", Role::Writer).await?;
    for (u, h) in [(a, "sa"), (b, "sb")] {
        ok(
            "session",
            db.create_session(session(u, h, T0, Proof::Unchecked)).await,
        )?;
    }
    let key = ok(
        "a's passkey",
        db.add_passkey(passkey(a, "ka", T0, Proof::Unchecked)).await,
    )?;
    let proofs = [
        Proof::Session("sa".into()),
        Proof::Password("pw-a".into()),
        Proof::Passkey(key),
    ];
    Ok((a, b, key, proofs))
}

/// A proof of one user never creates a credential for another.
pub async fn proof_is_for_its_own_user<S: Store>(db: &S) -> Check {
    let (a, b, _, proofs) = signed_in(db).await?;
    ok(
        "a live proof works",
        db.create_session(session(a, "s1", T0, proofs[0].clone()))
            .await,
    )?;
    fails(
        "b's session does not prove a",
        db.create_session(session(a, "s2", T0, Proof::Session("sb".into())))
            .await,
        ErrorCode::Unauthenticated,
    )?;
    for (i, p) in proofs.iter().enumerate() {
        let n = format!("x{i}");
        fails(
            &format!("{p:?} of a: no session for b"),
            db.create_session(session(b, &n, T0, p.clone())).await,
            ErrorCode::Unauthenticated,
        )?;
        fails(
            &format!("{p:?} of a: no passkey for b"),
            db.add_passkey(passkey(b, &n, T0, p.clone())).await,
            ErrorCode::Unauthenticated,
        )?;
        fails(
            &format!("{p:?} of a: no token for b"),
            db.create_token(token(b, &n, T0, p.clone())).await,
            ErrorCode::Unauthenticated,
        )?;
    }
    eq!(
        "b has no passkey",
        ok("list", db.list_passkeys(b).await)?.len(),
        0
    )?;
    eq!(
        "b has no token",
        ok("list", db.list_tokens(b).await)?.len(),
        0
    )
}

/// What a `blog admin reset` leaves (crates/cli/src/admin.rs), done with store writes: no
/// sessions, no passkeys, another password. The proofs read before it then hold nothing.
pub async fn stale_proof_creates_nothing<S: Store>(db: &S) -> Check {
    let (a, _, key, [by_session, by_password, by_passkey]) = signed_in(db).await?;
    // The reset, as the CLI writes it, ends what those proofs stand on.
    ok("passkeys gone", db.delete_passkey(key).await)?;
    ok(
        "password changed",
        db.change_password(
            a,
            Some("pw-new".into()),
            None,
            session(a, "s4", T0, Proof::Unchecked),
            T0,
        )
        .await,
    )?;
    ok("sessions gone", db.delete_user_sessions(a).await)?;
    for (what, p) in [
        ("session", &by_session),
        ("password", &by_password),
        ("passkey", &by_passkey),
    ] {
        fails(
            &format!("session by a stale {what}"),
            db.create_session(session(a, &format!("late-{what}"), T0 + 1, p.clone()))
                .await,
            ErrorCode::Unauthenticated,
        )?;
        fails(
            &format!("passkey by a stale {what}"),
            db.add_passkey(passkey(a, &format!("late-{what}"), T0 + 1, p.clone()))
                .await,
            ErrorCode::Unauthenticated,
        )?;
        fails(
            &format!("token by a stale {what}"),
            db.create_token(token(a, &format!("late-{what}"), T0 + 1, p.clone()))
                .await,
            ErrorCode::Unauthenticated,
        )?;
        fails(
            &format!("password change by a stale {what}"),
            db.change_password(
                a,
                Some("pw-attacker".into()),
                None,
                session(a, &format!("pc-{what}"), T0 + 1, p.clone()),
                T0 + 1,
            )
            .await,
            ErrorCode::Unauthenticated,
        )?;
    }
    eq!("no session written", live(db, "late-session").await?, false)?;
    eq!(
        "no passkey written",
        ok("list_passkeys", db.list_passkeys(a).await)?.len(),
        0
    )?;
    eq!(
        "no token written",
        ok("list_tokens", db.list_tokens(a).await)?.len(),
        0
    )?;
    let pw = ok("get_user", db.get_user(a).await)?.password_hash;
    eq!("password untouched", pw.as_deref(), Some("pw-new"))?;
    that("b keeps its session", live(db, "sb").await?)?;
    // The new password proves a new session.
    ok(
        "fresh password proof",
        db.create_session(session(a, "s5", T0 + 2, Proof::Password("pw-new".into())))
            .await,
    )?;
    Ok(())
}

/// A site with owner `o` (password, session, passkey, an old token, an old owner invite, a
/// reset code) and writer `w`; then what an attacker in flight left after the reset: a
/// passkey, a token and an owner invite. Returns `(o, w)`.
async fn after_reset<S: Store>(db: &S, keep_tokens: bool) -> Got<(UserId, UserId)> {
    let o = owner(db, "o", Role::Owner).await?;
    let w = owner(db, "w", Role::Writer).await?;
    for (u, h) in [(o, "so"), (w, "sw")] {
        ok(
            "session",
            db.create_session(session(u, h, T0, Proof::Unchecked)).await,
        )?;
        ok(
            "token",
            db.create_token(token(u, &format!("old-{h}"), T0, Proof::Unchecked))
                .await,
        )?;
        ok(
            "passkey",
            db.add_passkey(passkey(u, &format!("k-{h}"), T0, Proof::Unchecked))
                .await,
        )?;
    }
    ok(
        "old invite",
        db.create_invite(invite(o, "old-invite", T0)).await,
    )?;
    ok(
        "w's invite",
        db.create_invite(invite(w, "w-invite", RESET + 10)).await,
    )?;
    // Minted by a request that began just before the reset (or under a CLI clock running ahead).
    ok(
        "straddling token",
        db.create_token(token(o, "straddle", RESET - 1_000, Proof::Unchecked))
            .await,
    )?;
    ok(
        "reset",
        db.put_owner_reset("code", o, LATER, keep_tokens, RESET)
            .await,
    )?;
    ok(
        "late passkey",
        db.add_passkey(passkey(o, "late", RESET + 10, Proof::Unchecked))
            .await,
    )?;
    ok(
        "late token",
        db.create_token(token(o, "late", RESET + 10, Proof::Unchecked))
            .await,
    )?;
    ok(
        "late invite",
        db.create_invite(invite(o, "late-invite", RESET + 10)).await,
    )?;
    Ok((o, w))
}

fn by_code(o: UserId, hash: &str) -> NewSession {
    session(o, "redeemed", RESET + 20, Proof::ResetCode(hash.into()))
}

async fn revoked<S: Store>(db: &S, u: UserId) -> Got<Vec<(String, bool)>> {
    Ok(ok("list_tokens", db.list_tokens(u).await)?
        .into_iter()
        .map(|t| (t.token_hash, t.revoked_at.is_some()))
        .collect())
}

async fn invites<S: Store>(db: &S) -> Got<Vec<String>> {
    let mut v: Vec<String> = ok("list_invites", db.list_invites().await)?
        .into_iter()
        .map(|i| i.code_hash)
        .collect();
    v.sort();
    Ok(v)
}

/// The redeem: a wrong code or a login another user holds changes nothing and keeps the code;
/// the right one replaces every credential of the owner, theirs only, and works once.
pub async fn owner_reset_redeem_replaces_credentials<S: Store>(db: &S) -> Check {
    let (o, w) = after_reset(db, true).await?;
    let profile = |login: &str| ProfileUpdate {
        login: login.into(),
        slug: "o".into(),
        display_name: "o".into(),
        bio: String::new(),
    };
    fails(
        "wrong code",
        db.redeem_owner_reset(o, "pw-x".into(), None, by_code(o, "nope"), RESET + 20)
            .await,
        ErrorCode::SetupInvalid,
    )?;
    fails(
        "the code of another user",
        db.redeem_owner_reset(w, "pw-x".into(), None, by_code(w, "code"), RESET + 20)
            .await,
        ErrorCode::SetupInvalid,
    )?;
    fails(
        "expired",
        db.redeem_owner_reset(o, "pw-x".into(), None, by_code(o, "code"), LATER)
            .await,
        ErrorCode::SetupInvalid,
    )?;
    fails(
        "login another user holds",
        db.redeem_owner_reset(
            o,
            "pw-x".into(),
            Some(profile("w")),
            by_code(o, "code"),
            RESET + 20,
        )
        .await,
        ErrorCode::Conflict,
    )?;
    that(
        "the code still works",
        ok("hash", db.get_setting(OWNER_RESET_HASH_KEY).await)?.is_some(),
    )?;
    eq!(
        "passkeys kept on refusal",
        ok("list", db.list_passkeys(o).await)?.len(),
        2
    )?;
    ok(
        "redeem",
        db.redeem_owner_reset(
            o,
            "pw-o2".into(),
            Some(profile("o2")),
            by_code(o, "code"),
            RESET + 20,
        )
        .await,
    )?;
    let u = ok("get_user", db.get_user(o).await)?;
    eq!("password", u.password_hash.as_deref(), Some("pw-o2"))?;
    eq!("login", u.login.as_str(), "o2")?;
    eq!("old session gone", live(db, "so").await?, false)?;
    that("new session", live(db, "redeemed").await?)?;
    eq!(
        "every passkey gone",
        ok("list", db.list_passkeys(o).await)?.len(),
        0
    )?;
    eq!(
        "kept tokens stay, those minted since the margin before the reset are revoked",
        revoked(db, o).await?,
        named(&[("old-so", false), ("straddle", true), ("late", true)]),
    )?;
    eq!(
        "the owner invite made after the reset is gone",
        invites(db).await?,
        vec!["old-invite".to_string(), "w-invite".to_string()],
    )?;
    for &k in OWNER_RESET_KEYS {
        eq!(k, ok("get", db.get_setting(k).await)?, None)?;
    }
    // The writer is untouched.
    that("w's session", live(db, "sw").await?)?;
    eq!(
        "w's passkey",
        ok("list", db.list_passkeys(w).await)?.len(),
        1
    )?;
    eq!(
        "w's token",
        revoked(db, w).await?,
        vec![("old-sw".to_string(), false)]
    )?;
    fails(
        "single use",
        db.redeem_owner_reset(o, "pw-o3".into(), None, by_code(o, "code"), RESET + 21)
            .await,
        ErrorCode::SetupInvalid,
    )?;
    Ok(())
}

/// Without `--keep-tokens` the redeem revokes every token of the owner.
pub async fn owner_reset_redeem_revokes_all_tokens<S: Store>(db: &S) -> Check {
    let (o, _) = after_reset(db, false).await?;
    ok(
        "redeem",
        db.redeem_owner_reset(o, "pw-o2".into(), None, by_code(o, "code"), RESET + 20)
            .await,
    )?;
    eq!(
        "all revoked",
        revoked(db, o).await?,
        named(&[("old-so", true), ("straddle", true), ("late", true)]),
    )
}

/// A code whose owner was demoted or disabled after the reset gives nothing and changes
/// nothing; taking it then spends it.
pub async fn owner_reset_needs_an_active_owner<S: Store>(db: &S) -> Check {
    let (o, _) = after_reset(db, true).await?;
    // Another owner, so `o` may be demoted or disabled.
    owner(db, "root", Role::Owner).await?;
    for (what, disable) in [("demoted", false), ("disabled", true)] {
        ok(
            "code",
            db.put_owner_reset("code", o, LATER, true, RESET).await,
        )?;
        if disable {
            ok("set_role", db.set_role(o, Role::Owner, T0).await)?;
            ok("disable", db.set_disabled(o, Some(T0), T0).await)?;
        } else {
            ok("demote", db.set_role(o, Role::Writer, T0).await)?;
        }
        fails(
            what,
            db.redeem_owner_reset(o, "pw-o2".into(), None, by_code(o, "code"), RESET + 20)
                .await,
            ErrorCode::SetupInvalid,
        )?;
        let pw = ok("get_user", db.get_user(o).await)?.password_hash;
        eq!(
            &format!("{what}: password untouched"),
            pw.as_deref(),
            Some("pw-o")
        )?;
        eq!(
            &format!("{what}: no session"),
            live(db, "redeemed").await?,
            false
        )?;
        eq!(
            &format!("{what}: the take spends it and names the user"),
            ok("take", db.take_owner_reset("code", RESET + 20).await)?,
            Some(o),
        )?;
        if disable {
            ok("enable", db.set_disabled(o, None, T0).await)?;
        }
    }
    Ok(())
}
