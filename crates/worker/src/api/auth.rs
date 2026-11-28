//! `/api/auth/*`: setup, owner reset, password sign-in, sign-out, invites, own password and profile.
//! Request bodies carry the browser-stretched password (`web/admin/src/auth/stretch.ts`), never the
//! password itself.

use plumblog_auth::password::{
    self, STRETCH_ITERATIONS, STRETCHED_LEN, dummy_verify, hash_password, verify_password,
};
use plumblog_auth::{Crypto, base64url, constant_time_eq, session};
use plumblog_model::{Error, ErrorCode, Result, Role, UserId};
use plumblog_store::{
    InviteStore, NewSession, NewUser, OWNER_RESET_HASH_KEY, OWNER_RESET_USER_KEY, ProfileUpdate,
    SETUP_CODE_HASH_KEY, SITE_ID_KEY, SessionStore, SettingsStore, SetupStore, User, UserStore,
    setup_hash_json,
};
use plumblog_syslog::EventKind;

use super::body::{Fields, Out, json_string, object, quote};
use super::limit::{self, Keys};
use super::{Ctx, Principal, Reply, fields, passkey};

/// Random bytes in a new site id (base64url, 22 characters).
const SITE_ID_BYTES: usize = 16;

fn user_json(u: &User) -> String {
    object(&[
        ("id", Out::Int(u.id.0)),
        ("login", Out::Str(u.login.clone())),
        ("slug", Out::Str(u.slug.clone())),
        ("display_name", Out::Str(u.display_name.clone())),
        ("bio", Out::Str(u.bio.clone())),
        ("role", Out::Str(u.role.as_str().to_owned())),
    ])
}

fn user_reply(status: u16, u: &User) -> Reply {
    Reply::json(status, object(&[("user", Out::Raw(user_json(u)))]))
}

fn wrong_credentials(detail: &str) -> Error {
    Error::new(ErrorCode::InvalidCredentials, detail)
}

/// SHA-256 hex of a one-time code (setup, invite): only hashes are stored.
async fn code_hash(ctx: &Ctx, code: &str) -> Result<String> {
    if code.is_empty() {
        return Err(Error::new(ErrorCode::InvalidInput, "code is required"));
    }
    Ok(base64url::hex(&ctx.crypto.sha256(code.as_bytes()).await?))
}

/// The site id, created on first use. Public: the browser needs it to stretch a password
/// before anyone is signed in.
async fn site_id(ctx: &Ctx) -> Result<String> {
    let stored = if let Some(v) = ctx.store.get_setting(SITE_ID_KEY).await? {
        v
    } else {
        let mut b = [0u8; SITE_ID_BYTES];
        ctx.crypto.random_bytes(&mut b)?;
        let fresh = quote(&base64url::encode(&b));
        ctx.store.ensure_site_id(&fresh, ctx.now).await?
    };
    json_string(&stored).ok_or_else(|| {
        Error::new(
            ErrorCode::Internal,
            format!("setting {SITE_ID_KEY} is not a JSON string"),
        )
    })
}

/// `GET /api/auth/params`: what the sign-in page needs before anyone is signed in.
pub async fn params(ctx: &Ctx) -> Result<Reply> {
    let site_id = site_id(ctx).await?;
    let turnstile = limit::turnstile_site_key(ctx)
        .await?
        .map_or(Out::Null, Out::Str);
    Ok(Reply::json(
        200,
        object(&[
            ("site_id", Out::Str(site_id)),
            (
                "stretch_iterations",
                Out::Int(i64::from(STRETCH_ITERATIONS)),
            ),
            ("turnstile_site_key", turnstile),
        ]),
    ))
}

/// `GET /api/auth/me`: the signed-in user and their passkeys (when added and last used).
pub async fn me(ctx: &Ctx, p: &Principal) -> Result<Reply> {
    let user = p.user()?;
    Ok(Reply::json(
        200,
        object(&[
            ("user", Out::Raw(user_json(user))),
            (
                "passkeys",
                Out::Raw(passkey::list_json(ctx, user.id).await?),
            ),
        ]),
    ))
}

async fn new_session(ctx: &Ctx, user: UserId) -> Result<(NewSession, String)> {
    let (token, hash) = session::new_token(&ctx.crypto).await?;
    let expires = session::expires_at(ctx.now);
    Ok((
        NewSession {
            token_hash: hash,
            user_id: user,
            created_at: ctx.now,
            expires_at: expires,
        },
        session::set_cookie(&token, expires, ctx.now),
    ))
}

/// Starts a session for `user` and ends the one this request came with (rotation on
/// sign-in), then answers with the user.
pub(super) async fn sign_in(
    ctx: &Ctx,
    p: &Principal,
    user: &User,
    status: u16,
    how: &str,
) -> Result<Reply> {
    let (new, cookie) = new_session(ctx, user.id).await?;
    ctx.store.create_session(new).await?;
    if let Some(old) = p.session_hash()
        && let Err(e) = ctx.store.delete_session(old).await
    {
        // The new session works; the old one ends at its expiry. Worth a log, not a failure.
        worker::console_error!(
            "[{}] rotation: deleting the previous session failed: {e}",
            ctx.request_id.as_str()
        );
    }
    ctx.syslog(ctx.event(EventKind::SignIn, Some(user.id)).subject(how))
        .await;
    Ok(user_reply(status, user).with_cookie(cookie))
}

/// Same work whether or not the account exists or has a password, so neither the answer nor
/// its timing tells them apart.
async fn password_matches(
    ctx: &Ctx,
    user: Option<&User>,
    stretched: &[u8; STRETCHED_LEN],
) -> Result<bool> {
    match user.and_then(|u| u.password_hash.as_deref()) {
        // An unreadable stored hash is an error (500, logged), never a silent mismatch.
        Some(stored) => verify_password(&ctx.crypto, stored, stretched).await,
        None => dummy_verify(&ctx.crypto, stretched).await,
    }
}

fn new_user(f: &Fields, now: i64, hash: String) -> Result<NewUser> {
    let login = f.req("login")?;
    let slug = f.req("slug")?;
    let display_name = f.req("display_name")?;
    fields::login(login)?;
    fields::slug(slug)?;
    fields::display_name(display_name)?;
    Ok(NewUser {
        login: login.to_owned(),
        slug: slug.to_owned(),
        display_name: display_name.to_owned(),
        bio: String::new(),
        // Ignored by both stores (setup: owner; invite: the invite's role).
        role: Role::Writer,
        password_hash: Some(hash),
        created_at: now,
    })
}

const NEW_ACCOUNT_FIELDS: &[&str] = &[
    "code",
    "login",
    "slug",
    "display_name",
    "password",
    "turnstile_token",
];

/// One-time codes are guessable only by trying: wrong codes count per address like failed
/// sign-ins (same thresholds, Turnstile, lock), and are logged.
async fn code_failed(ctx: &Ctx, attempt: &limit::Attempt, how: &str) -> Result<()> {
    limit::failed(ctx, attempt, None, how).await;
    ctx.syslog(ctx.event(EventKind::SignInFailed, None).subject(how))
        .await;
    Ok(())
}

/// `POST /api/auth/setup`: the first owner, with the one-time code `blog setup` printed.
pub async fn setup(ctx: &Ctx, p: &Principal, body: &[u8]) -> Result<Reply> {
    let f = Fields::parse(body, NEW_ACCOUNT_FIELDS)?;
    let stretched = password::parse_stretched(f.req("password")?)?;
    let hash = code_hash(ctx, f.req("code")?).await?;
    let user = new_user(&f, ctx.now, String::new())?;
    let keys = Keys::address(ctx).await?;
    let attempt = limit::check(ctx, &keys, f.opt("turnstile_token")?).await?;
    // The store compares with SQL `=`; compare in constant time here first, as its docs ask.
    let stored = ctx.store.get_setting(SETUP_CODE_HASH_KEY).await?;
    let matches =
        stored.is_some_and(|s| constant_time_eq(s.as_bytes(), setup_hash_json(&hash).as_bytes()));
    if !matches {
        code_failed(ctx, &attempt, "setup code").await?;
        return Err(Error::new(
            ErrorCode::SetupInvalid,
            "setup code does not match, or none is pending",
        ));
    }
    let user = NewUser {
        password_hash: Some(hash_password(&ctx.crypto, &stretched).await?),
        ..user
    };
    // Expired, or another setup won the race: the store refuses with SetupInvalid too.
    let id = match ctx.store.create_first_owner(&hash, user, ctx.now).await {
        Err(e) if e.code == ErrorCode::SetupInvalid => {
            code_failed(ctx, &attempt, "setup code").await?;
            return Err(e);
        }
        other => other?,
    };
    // The code was right: this attempt was not a guess.
    limit::succeeded(ctx, &keys).await?;
    // The page normally created the id already (params); make sure it exists.
    site_id(ctx).await?;
    ctx.syslog(
        ctx.event(EventKind::UserCreated, Some(id))
            .subject("setup: first owner"),
    )
    .await;
    let user = ctx.store.get_user(id).await?;
    sign_in(ctx, p, &user, 201, "setup").await
}

/// `POST /api/auth/reset`: an owner's new password (and maybe login), with the one-time code
/// `blog admin reset` printed. Limited like setup codes. The code is taken (single use) before
/// the write; a write that then fails leaves no code, and the owner runs the reset again.
pub async fn redeem_reset(ctx: &Ctx, p: &Principal, body: &[u8]) -> Result<Reply> {
    let f = Fields::parse(body, &["code", "login", "password", "turnstile_token"])?;
    let stretched = password::parse_stretched(f.req("password")?)?;
    let hash = code_hash(ctx, f.req("code")?).await?;
    let login = f.req("login")?;
    fields::login(login)?;
    let keys = Keys::address(ctx).await?;
    let attempt = limit::check(ctx, &keys, f.opt("turnstile_token")?).await?;
    let stored = ctx.store.get_setting(OWNER_RESET_HASH_KEY).await?;
    let matches =
        stored.is_some_and(|s| constant_time_eq(s.as_bytes(), setup_hash_json(&hash).as_bytes()));
    let taken = if matches {
        // A login another user holds would fail the write after the code is spent: refuse
        // it first (the take below stays the check that counts).
        if let Some(other) = ctx.store.find_user_by_login(login).await?
            && ctx
                .store
                .get_setting(OWNER_RESET_USER_KEY)
                .await?
                .as_deref()
                != Some(other.id.0.to_string().as_str())
        {
            return Err(Error::new(ErrorCode::Conflict, "login is taken").at("login"));
        }
        ctx.store.take_owner_reset(&hash, ctx.now).await?
    } else {
        None
    };
    let Some(id) = taken else {
        code_failed(ctx, &attempt, "reset code").await?;
        return Err(Error::new(
            ErrorCode::SetupInvalid,
            "reset code does not match, expired or was used",
        ));
    };
    // The code was right: this attempt was not a guess.
    limit::succeeded(ctx, &keys).await?;
    let user = ctx.store.get_user(id).await?;
    if user.role != Role::Owner || user.disabled_at.is_some() {
        // Demoted or disabled since the reset: the code is spent and gives nothing.
        return Err(Error::new(
            ErrorCode::SetupInvalid,
            format!(
                "reset code for user {} who is no longer an active owner",
                id.0
            ),
        ));
    }
    let profile = (login != user.login).then(|| ProfileUpdate {
        login: login.to_owned(),
        slug: user.slug.clone(),
        display_name: user.display_name.clone(),
        bio: user.bio.clone(),
    });
    let new_hash = Some(hash_password(&ctx.crypto, &stretched).await?);
    let (session, cookie) = new_session(ctx, user.id).await?;
    // The password write that ends every session of the user and creates this one.
    ctx.store
        .change_password(user.id, new_hash, profile, session, ctx.now)
        .await?;
    if let Some(old) = p.session_hash()
        && let Err(e) = ctx.store.delete_session(old).await
    {
        worker::console_error!(
            "[{}] reset: deleting the request's previous session failed: {e}",
            ctx.request_id.as_str()
        );
    }
    ctx.syslog(
        ctx.event(EventKind::OwnerResetRedeemed, Some(user.id))
            .subject("owner reset redeemed"),
    )
    .await;
    Ok(user_reply(200, &ctx.store.get_user(user.id).await?).with_cookie(cookie))
}

/// `POST /api/auth/invite/accept`: a new account with the invite's role.
pub async fn accept_invite(ctx: &Ctx, p: &Principal, body: &[u8]) -> Result<Reply> {
    let f = Fields::parse(body, NEW_ACCOUNT_FIELDS)?;
    let stretched = password::parse_stretched(f.req("password")?)?;
    let hash = code_hash(ctx, f.req("code")?).await?;
    new_user(&f, ctx.now, String::new())?;
    let keys = Keys::address(ctx).await?;
    let attempt = limit::check(ctx, &keys, f.opt("turnstile_token")?).await?;
    let user = new_user(&f, ctx.now, hash_password(&ctx.crypto, &stretched).await?)?;
    let id = match ctx.store.accept_invite(&hash, user, ctx.now).await {
        Err(e) if e.code == ErrorCode::InviteInvalid => {
            code_failed(ctx, &attempt, "invite code").await?;
            return Err(e);
        }
        other => other?,
    };
    // The code was right: this attempt was not a guess.
    limit::succeeded(ctx, &keys).await?;
    ctx.syslog(
        ctx.event(EventKind::UserCreated, Some(id))
            .subject("invite accepted"),
    )
    .await;
    let user = ctx.store.get_user(id).await?;
    sign_in(ctx, p, &user, 201, "invite").await
}

/// `POST /api/auth/login`.
pub async fn login(ctx: &Ctx, p: &Principal, body: &[u8]) -> Result<Reply> {
    let f = Fields::parse(body, &["login", "password", "turnstile_token"])?;
    let login = f.req("login")?;
    let stretched = password::parse_stretched(f.req("password")?)?;
    let keys = Keys::new(ctx, login).await?;
    let attempt = limit::check(ctx, &keys, f.opt("turnstile_token")?).await?;
    let user = ctx.store.find_user_by_login(login).await?;
    let matched = password_matches(ctx, user.as_ref(), &stretched).await?;
    let user = match user {
        Some(u) if matched && u.disabled_at.is_none() => u,
        other => {
            let actor = other.as_ref().map(|u| u.id);
            limit::failed(ctx, &attempt, actor, "password").await;
            ctx.syslog(
                ctx.event(EventKind::SignInFailed, actor)
                    .subject("password"),
            )
            .await;
            // One answer for unknown login, wrong password and disabled account.
            return Err(wrong_credentials(match other {
                None => "unknown login",
                Some(u) if u.disabled_at.is_some() => "account disabled",
                Some(_) => "wrong password",
            }));
        }
    };
    limit::succeeded(ctx, &keys).await?;
    sign_in(ctx, p, &user, 200, "password").await
}

/// `POST /api/auth/logout`: ends this session. Harmless without one.
pub async fn logout(ctx: &Ctx, p: &Principal) -> Result<Reply> {
    if let Some(hash) = p.session_hash() {
        ctx.store.delete_session(hash).await?;
    }
    // Only a live session is a sign-out; a stale cookie is just cleared.
    if let Ok(user) = p.user() {
        ctx.syslog(
            ctx.event(EventKind::SignOut, Some(user.id))
                .subject("sign-out"),
        )
        .await;
    }
    Ok(Reply::empty().with_cookie(session::clear_cookie()))
}

/// Checks the signed-in user's current password, rate limited like sign-in (a stolen session
/// must not become a password oracle), but outside the site-wide cap.
pub(super) async fn check_current(ctx: &Ctx, user: &User, f: &Fields) -> Result<()> {
    let current = password::parse_stretched(f.req("current_password")?)?;
    let keys = Keys::new(ctx, &user.login).await?;
    let attempt = limit::check_signed_in(ctx, &keys, f.opt("turnstile_token")?).await?;
    if password_matches(ctx, Some(user), &current).await? {
        limit::succeeded(ctx, &keys).await
    } else {
        limit::failed(ctx, &attempt, Some(user.id), "current password").await;
        ctx.syslog(
            ctx.event(EventKind::SignInFailed, Some(user.id))
                .subject("current password"),
        )
        .await;
        Err(wrong_credentials("current password does not match"))
    }
}

/// Writes the new password (and profile, for a login change), ends every other session of
/// the user and rotates this one, in one store write.
async fn write_password(
    ctx: &Ctx,
    user: &User,
    f: &Fields,
    profile: Option<ProfileUpdate>,
    detail: &str,
) -> Result<String> {
    let new = password::parse_stretched(f.req("new_password")?)?;
    let hash = Some(hash_password(&ctx.crypto, &new).await?);
    let (session, cookie) = new_session(ctx, user.id).await?;
    ctx.store
        .change_password(user.id, hash, profile, session, ctx.now)
        .await?;
    ctx.syslog(
        ctx.event(EventKind::PasswordChanged, Some(user.id))
            .subject(detail),
    )
    .await;
    Ok(cookie)
}

/// `POST /api/auth/password`: change own password.
pub async fn change_password(ctx: &Ctx, p: &Principal, body: &[u8]) -> Result<Reply> {
    let user = p.user()?;
    let f = Fields::parse(
        body,
        &["current_password", "new_password", "turnstile_token"],
    )?;
    // Shape errors before the (rate-limited) password check.
    password::parse_stretched(f.req("new_password")?)?;
    check_current(ctx, user, &f).await?;
    let cookie = write_password(ctx, user, &f, None, "password").await?;
    Ok(Reply::empty().with_cookie(cookie))
}

/// `PATCH /api/auth/me`: own profile. A login change also needs `current_password` and
/// `new_password`, both stretched by the browser: the stretch salt holds the login, so the
/// password must be re-stretched with the new one. An account without a password (passkey
/// only) re-authenticates with a passkey assertion instead (a challenge from
/// `/api/auth/passkey/login/options`); the write is the same atomic one, sessions rotate.
pub async fn update_me(ctx: &Ctx, p: &Principal, body: &[u8]) -> Result<Reply> {
    let user = p.user()?;
    let mut allowed = vec![
        "login",
        "slug",
        "display_name",
        "bio",
        "current_password",
        "new_password",
        "turnstile_token",
    ];
    allowed.extend_from_slice(passkey::ASSERTION_FIELDS);
    let f = Fields::parse(body, &allowed)?;
    let pick =
        |key: &str, now: &str| -> Result<String> { Ok(f.opt(key)?.unwrap_or(now).to_owned()) };
    let profile = ProfileUpdate {
        login: pick("login", &user.login)?,
        slug: pick("slug", &user.slug)?,
        display_name: pick("display_name", &user.display_name)?,
        bio: pick("bio", &user.bio)?,
    };
    fields::login(&profile.login)?;
    fields::slug(&profile.slug)?;
    fields::display_name(&profile.display_name)?;
    fields::bio(&profile.bio)?;
    let has_passwords = f.opt("current_password")?.is_some() || f.opt("new_password")?.is_some();
    let has_assertion = f.opt("credential_id")?.is_some();
    if profile.login == user.login {
        if has_passwords || has_assertion {
            return Err(Error::new(
                ErrorCode::InvalidInput,
                "password and passkey fields belong to a login change; use /api/auth/password",
            ));
        }
        ctx.store.update_profile(user.id, profile, ctx.now).await?;
        return Ok(user_reply(200, &ctx.store.get_user(user.id).await?));
    }
    if user.password_hash.is_none() {
        if has_passwords {
            return Err(Error::new(
                ErrorCode::InvalidInput,
                "passkey-only account: re-authenticate with a passkey, not a password",
            ));
        }
        passkey::verify(ctx, &f, "", Some(user.id), "passkey re-auth").await?;
        let (session, cookie) = new_session(ctx, user.id).await?;
        // Conflict if a password was set meanwhile (its stretch salt holds the old login).
        ctx.store
            .change_password(user.id, None, Some(profile), session, ctx.now)
            .await?;
        ctx.syslog(
            ctx.event(EventKind::PasswordChanged, Some(user.id))
                .subject("login (passkey)"),
        )
        .await;
        return Ok(user_reply(200, &ctx.store.get_user(user.id).await?).with_cookie(cookie));
    }
    if has_assertion {
        return Err(Error::new(
            ErrorCode::InvalidInput,
            "an account with a password changes its login with current_password",
        ));
    }
    password::parse_stretched(f.req("new_password")?)?;
    check_current(ctx, user, &f).await?;
    let cookie = write_password(ctx, user, &f, Some(profile), "login").await?;
    Ok(user_reply(200, &ctx.store.get_user(user.id).await?).with_cookie(cookie))
}
