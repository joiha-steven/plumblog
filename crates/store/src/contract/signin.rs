//! Sign-in rate limits, passkey challenges and first-owner setup.

use plumblog_model::{ErrorCode, Role};

use super::kit::{Check, T0, eq, fails, new_user, ok, user, version_is};
use crate::{
    ChallengePurpose, MAX_FAILURE_KEYS, SETUP_CODE_EXPIRES_KEY, SETUP_CODE_HASH_KEY, Store,
};

const WINDOW: i64 = 900_000;

pub async fn login_failures_count_and_restart<S: Store>(db: &S) -> Check {
    eq!(
        "first failure",
        ok("record_failure", db.record_failure("k", T0, WINDOW).await)?,
        (1, T0),
    )?;
    eq!(
        "second failure",
        ok(
            "record_failure",
            db.record_failure("k", T0 + 10, WINDOW).await
        )?,
        (2, T0),
    )?;
    eq!(
        "failure 1 ms before the window ends counts in it",
        ok(
            "record_failure",
            db.record_failure("k", T0 + WINDOW - 1, WINDOW).await
        )?,
        (3, T0),
    )?;
    eq!(
        "failure at exactly the window end restarts it",
        ok(
            "record_failure",
            db.record_failure("k", T0 + WINDOW, WINDOW).await
        )?,
        (1, T0 + WINDOW),
    )?;
    eq!(
        "restarted window counts on",
        ok(
            "record_failure",
            db.record_failure("k", T0 + WINDOW + 1, WINDOW).await
        )?,
        (2, T0 + WINDOW),
    )?;
    ok("clear_failures", db.clear_failures("k").await)?;
    ok("clear_failures absent", db.clear_failures("k").await)?;
    eq!(
        "after clearing a failure starts at 1",
        ok(
            "record_failure",
            db.record_failure("k", T0 + WINDOW + 2, WINDOW).await
        )?,
        (1, T0 + WINDOW + 2),
    )?;
    version_is(db, "failures", 1).await
}

pub async fn login_failures_multi_key_read<S: Store>(db: &S) -> Check {
    for _ in 0..3 {
        ok("record_failure a", db.record_failure("a", T0, WINDOW).await)?;
    }
    ok(
        "record_failure b",
        db.record_failure("b", T0 + 5, WINDOW).await,
    )?;
    eq!(
        "in the order given; missing reads as 0",
        ok(
            "failures",
            db.failures(&["b", "missing", "a", "b"], T0 + 5, WINDOW)
                .await
        )?,
        vec![(1, T0 + 5), (0, 0), (3, T0), (1, T0 + 5)],
    )?;
    eq!(
        "a window that ended reads as 0, even at exactly its end",
        ok(
            "failures",
            db.failures(&["a", "b"], T0 + WINDOW, WINDOW).await
        )?,
        vec![(0, 0), (1, T0 + 5)],
    )?;
    eq!(
        "no keys",
        ok("failures none", db.failures(&[], T0, WINDOW).await)?,
        Vec::<(i64, i64)>::new(),
    )?;
    let many = vec!["k"; MAX_FAILURE_KEYS + 1];
    fails(
        "failures over the key cap",
        db.failures(&many, T0, WINDOW).await,
        ErrorCode::InvalidInput,
    )?;
    let full = vec!["a"; MAX_FAILURE_KEYS];
    eq!(
        "failures at the key cap",
        ok("failures cap", db.failures(&full, T0, WINDOW).await)?.len(),
        MAX_FAILURE_KEYS,
    )?;
    eq!(
        "reading never resets an ended window",
        ok(
            "record_failure a",
            db.record_failure("a", T0 + 1, WINDOW).await
        )?,
        (4, T0),
    )
}

pub async fn challenges_are_single_use<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    ok(
        "put register",
        db.put_challenge("r", ChallengePurpose::Register, Some(u), T0 + 100)
            .await,
    )?;
    ok(
        "put login",
        db.put_challenge("l", ChallengePurpose::Login, None, T0 + 100)
            .await,
    )?;
    fails(
        "put duplicate hash",
        db.put_challenge("r", ChallengePurpose::Login, None, T0 + 100)
            .await,
        ErrorCode::Conflict,
    )?;
    let ghost = user(db, "ghost").await?;
    ok("delete_user ghost", db.delete_user(ghost).await)?;
    fails(
        "put for a missing user",
        db.put_challenge("x", ChallengePurpose::Register, Some(ghost), T0 + 100)
            .await,
        ErrorCode::InvalidInput,
    )?;

    eq!(
        "take with the wrong purpose",
        ok(
            "take",
            db.take_challenge("r", ChallengePurpose::Login, T0).await
        )?,
        None,
    )?;
    eq!(
        "take register (wrong purpose left it alone)",
        ok(
            "take",
            db.take_challenge("r", ChallengePurpose::Register, T0).await
        )?,
        Some(Some(u)),
    )?;
    eq!(
        "second take",
        ok(
            "take",
            db.take_challenge("r", ChallengePurpose::Register, T0).await
        )?,
        None,
    )?;
    eq!(
        "take login challenge 1 ms before expiry",
        ok(
            "take",
            db.take_challenge("l", ChallengePurpose::Login, T0 + 99)
                .await
        )?,
        Some(None),
    )?;
    eq!(
        "take unknown",
        ok(
            "take",
            db.take_challenge("nope", ChallengePurpose::Login, T0).await
        )?,
        None,
    )?;
    version_is(db, "challenges", 1).await
}

pub async fn challenges_expire<S: Store>(db: &S) -> Check {
    for (hash, at) in [("e1", T0), ("e2", T0 + 1), ("e3", T0 + 2)] {
        ok(
            "put",
            db.put_challenge(hash, ChallengePurpose::Login, None, at)
                .await,
        )?;
    }
    eq!(
        "take at exactly expires_at",
        ok(
            "take",
            db.take_challenge("e1", ChallengePurpose::Login, T0).await
        )?,
        None,
    )?;
    eq!(
        "the expired take deleted it",
        ok(
            "take",
            db.take_challenge("e1", ChallengePurpose::Login, T0 - 1)
                .await
        )?,
        None,
    )?;
    eq!(
        "delete_expired removes expires_at <= now",
        ok(
            "delete_expired_challenges",
            db.delete_expired_challenges(T0 + 1).await
        )?,
        1,
    )?;
    eq!(
        "an unexpired one is kept",
        ok(
            "take",
            db.take_challenge("e3", ChallengePurpose::Login, T0 + 1)
                .await
        )?,
        Some(None),
    )?;
    eq!(
        "nothing left to delete",
        ok(
            "delete_expired_challenges",
            db.delete_expired_challenges(T0 + 100).await
        )?,
        0,
    )
}

pub async fn challenges_cascade_on_user_delete<S: Store>(db: &S) -> Check {
    let mut o = new_user("owner");
    o.role = Role::Owner;
    ok("create_user owner", db.create_user(o).await)?;
    let w = user(db, "writer").await?;
    ok(
        "put",
        db.put_challenge("w", ChallengePurpose::Register, Some(w), T0 + 100)
            .await,
    )?;
    ok(
        "put",
        db.put_challenge("l", ChallengePurpose::Login, None, T0 + 100)
            .await,
    )?;
    ok("delete_user", db.delete_user(w).await)?;
    eq!(
        "the deleted user's challenge is gone",
        ok(
            "take",
            db.take_challenge("w", ChallengePurpose::Register, T0).await
        )?,
        None,
    )?;
    eq!(
        "other challenges stay",
        ok(
            "take",
            db.take_challenge("l", ChallengePurpose::Login, T0).await
        )?,
        Some(None),
    )
}

async fn setup_keys<S: Store>(db: &S) -> super::kit::Got<(Option<String>, Option<String>)> {
    Ok((
        ok("get hash", db.get_setting(SETUP_CODE_HASH_KEY).await)?,
        ok("get expiry", db.get_setting(SETUP_CODE_EXPIRES_KEY).await)?,
    ))
}

pub async fn setup_refuses_bad_codes<S: Store>(db: &S) -> Check {
    fails(
        "setup without a code",
        db.create_first_owner("c", new_user("a"), T0).await,
        ErrorCode::SetupInvalid,
    )?;
    ok("put_setup_code", db.put_setup_code("c", T0 + 100, T0).await)?;
    eq!(
        "setup keys as JSON",
        setup_keys(db).await?,
        (Some("\"c\"".to_owned()), Some((T0 + 100).to_string())),
    )?;
    fails(
        "wrong code",
        db.create_first_owner("d", new_user("a"), T0).await,
        ErrorCode::SetupInvalid,
    )?;
    fails(
        "code at exactly its expiry",
        db.create_first_owner("c", new_user("a"), T0 + 100).await,
        ErrorCode::SetupInvalid,
    )?;
    ok(
        "garble the expiry",
        db.put_setting(SETUP_CODE_EXPIRES_KEY, "\"9999999999999\"", T0)
            .await,
    )?;
    fails(
        "expiry that is not a JSON integer",
        db.create_first_owner("c", new_user("a"), T0).await,
        ErrorCode::SetupInvalid,
    )?;
    eq!(
        "refused setups create no user",
        ok("list_users", db.list_users().await)?.len(),
        0,
    )?;
    eq!(
        "refused setups keep the code",
        setup_keys(db).await?.0,
        Some("\"c\"".to_owned()),
    )
}

pub async fn setup_creates_first_owner_once<S: Store>(db: &S) -> Check {
    ok(
        "put_setup_code",
        db.put_setup_code("old", T0 + 50, T0).await,
    )?;
    ok(
        "put_setup_code replaces",
        db.put_setup_code("c", T0 + 100, T0).await,
    )?;
    fails(
        "replaced code",
        db.create_first_owner("old", new_user("a"), T0).await,
        ErrorCode::SetupInvalid,
    )?;
    let mut nu = new_user("first");
    nu.role = Role::Writer;
    let id = ok(
        "create_first_owner",
        db.create_first_owner("c", nu, T0 + 99).await,
    )?;
    let u = ok("get_user", db.get_user(id).await)?;
    eq!(
        "first user is an owner whatever role was asked",
        (u.login.as_str(), u.role),
        ("first", Role::Owner),
    )?;
    eq!(
        "success consumes the code",
        setup_keys(db).await?,
        (None, None),
    )?;
    fails(
        "same code again",
        db.create_first_owner("c", new_user("b"), T0 + 99).await,
        ErrorCode::SetupInvalid,
    )?;
    ok(
        "put_setup_code again",
        db.put_setup_code("n", T0 + 500, T0).await,
    )?;
    fails(
        "valid code once a user exists",
        db.create_first_owner("n", new_user("b"), T0 + 99).await,
        ErrorCode::SetupInvalid,
    )?;
    eq!(
        "still one user",
        ok("list_users", db.list_users().await)?.len(),
        1,
    )?;
    // Readers never see the setup keys or need the first user's pages yet.
    version_is(db, "setup", 1).await
}

/// The first stored site id wins; later calls get it back and write nothing visible.
pub async fn site_id_is_set_once<S: Store>(db: &S) -> Check {
    let first = ok("ensure_site_id", db.ensure_site_id("\"one\"", T0).await)?;
    eq!("first call stores its value", first.as_str(), "\"one\"")?;
    let again = ok("ensure_site_id", db.ensure_site_id("\"two\"", T0 + 1).await)?;
    eq!("second call keeps the first", again.as_str(), "\"one\"")?;
    let stored = ok("get_setting", db.get_setting(crate::SITE_ID_KEY).await)?;
    eq!("stored value", stored.as_deref(), Some("\"one\""))?;
    version_is(db, "ensure_site_id", 1).await
}

/// Renewal moves the expiry of a live session only; it never revives an expired one.
pub async fn session_renewal<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let new = |hash: &str, expires_at: i64| crate::NewSession {
        token_hash: hash.into(),
        user_id: u,
        created_at: T0,
        expires_at,
    };
    ok("live", db.create_session(new("live", T0 + 100)).await)?;
    ok("dead", db.create_session(new("dead", T0 + 10)).await)?;
    let renewed = ok(
        "renew live",
        db.renew_session("live", T0 + 500, T0 + 50).await,
    )?;
    eq!("live session renewed", renewed, true)?;
    let s = ok("find live", db.find_session("live", T0 + 400).await)?;
    eq!("new expiry holds", s.map(|s| s.expires_at), Some(T0 + 500))?;
    let revived = ok(
        "renew dead",
        db.renew_session("dead", T0 + 500, T0 + 50).await,
    )?;
    eq!("expired session not revived", revived, false)?;
    let absent = ok("renew absent", db.renew_session("nope", T0 + 500, T0).await)?;
    eq!("absent session", absent, false)?;
    let s = ok("find dead", db.find_session("dead", T0).await)?;
    eq!(
        "expired session keeps its expiry",
        s.map(|s| s.expires_at),
        Some(T0 + 10)
    )
}
