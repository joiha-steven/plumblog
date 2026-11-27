//! `/api/auth/passkey/*`: passkey registration, sign-in, listing and removal
//! (docs/ARCHITECTURE.md, Users and auth). Parsing and signature checks are
//! `plumblog_auth::webauthn`; this file owns what needs storage: single-use challenges, the
//! credential lookup, the sign counter and the rate limit.
//!
//! Options answer in the `WebAuthn` JSON shape (`PublicKeyCredential.parseCreationOptionsFromJSON`
//! / `parseRequestOptionsFromJSON` read `publicKey` as is). Verify requests carry base64url
//! fields: `client_data_json`, `attestation_object` (register) or `credential_id`,
//! `authenticator_data`, `signature` (sign-in).

use plumblog_auth::webauthn::{self, Expected, client_data, cose};
use plumblog_auth::{Crypto, base64url};
use plumblog_model::{Error, ErrorCode, Result, UserId};
use plumblog_store::{
    ChallengePurpose, ChallengeStore, NewPasskey, Passkey, PasskeyStore, User, UserStore,
};
use plumblog_syslog::EventKind;

use super::body::{Fields, Out, object};
use super::limit::{self, Keys};
use super::{Ctx, Principal, Reply, auth, fields, site};

/// `DELETE` target: `/api/auth/passkey/<id>`.
pub const ITEM_PREFIX: &str = "/api/auth/passkey/";
/// Challenges live 5 minutes, and the browser is told the same.
const CHALLENGE_TTL_MS: i64 = 5 * 60 * 1000;
const CHALLENGE_BYTES: usize = 32;
/// Base64url of the longest credential id `WebAuthn` allows.
const MAX_CREDENTIAL_ID_TEXT: usize = (webauthn::MAX_CREDENTIAL_ID_LEN * 4).div_ceil(3);
/// Fields of an assertion, at sign-in and at a passkey-only login change.
pub const ASSERTION_FIELDS: &[&str] = &[
    "credential_id",
    "client_data_json",
    "authenticator_data",
    "signature",
    "user_handle",
];
/// Re-authentication before adding or removing a passkey: `current_password`, or an assertion
/// by one of the user's own passkeys under the `reauth_` prefix (registration already uses
/// `client_data_json`).
const REAUTH_FIELDS: &[&str] = &[
    "current_password",
    "reauth_credential_id",
    "reauth_client_data_json",
    "reauth_authenticator_data",
    "reauth_signature",
    "reauth_user_handle",
    "turnstile_token",
];
/// Prefix of the assertion fields in [`REAUTH_FIELDS`].
const REAUTH: &str = "reauth_";

fn invalid(detail: impl Into<String>) -> Error {
    Error::new(ErrorCode::PasskeyInvalid, detail)
}

fn array(items: &[String]) -> String {
    format!("[{}]", items.join(","))
}

/// An empty body or `{}`: the options routes take no input.
fn no_fields(body: &[u8]) -> Result<()> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(());
    }
    Fields::parse(body, &[]).map(|_| ())
}

/// The base64url field `prefix` + `key`.
fn b64(f: &Fields, prefix: &str, key: &str) -> Result<Vec<u8>> {
    let key = format!("{prefix}{key}");
    base64url::decode(f.req(&key)?).ok_or_else(|| invalid(format!("{key} is not base64url")))
}

async fn challenge_hash(ctx: &Ctx, raw: &[u8]) -> Result<String> {
    Ok(base64url::hex(&ctx.crypto.sha256(raw).await?))
}

/// Prefix of a gated challenge's hashed input: a sign-in challenge whose options request
/// passed the site-wide cap with a Turnstile token is stored under this hash instead, so the
/// row itself records it (no schema change). A plain challenge cannot be taken as gated
/// (that needs a SHA-256 preimage); a client sending `GATED || raw` could take a gated row
/// as plain, which gains nothing: the row stays login-only and single use.
const GATED: &[u8] = b"plumblog gated sign-in challenge\0";

async fn gated_hash(ctx: &Ctx, raw: &[u8]) -> Result<String> {
    challenge_hash(ctx, &[GATED, raw].concat()).await
}

/// A fresh random challenge, stored hashed, single use, 5 minutes. Expired ones are swept
/// first (lazy cleanup, no cron). `gated` (sign-in only): stored under [`gated_hash`].
async fn new_challenge(
    ctx: &Ctx,
    purpose: ChallengePurpose,
    user: Option<UserId>,
    gated: bool,
) -> Result<String> {
    if let Err(e) = ctx.store.delete_expired_challenges(ctx.now).await {
        // Leftovers are harmless (expired ones never validate); the next call retries.
        worker::console_error!(
            "[{}] sweeping expired challenges failed: {e}",
            ctx.request_id.as_str()
        );
    }
    let mut raw = [0u8; CHALLENGE_BYTES];
    ctx.crypto.random_bytes(&mut raw)?;
    let hash = if gated {
        gated_hash(ctx, &raw).await?
    } else {
        challenge_hash(ctx, &raw).await?
    };
    ctx.store
        .put_challenge(&hash, purpose, user, ctx.now + CHALLENGE_TTL_MS)
        .await?;
    Ok(base64url::encode(&raw))
}

/// Consumes the challenge `client_data_json` names. `Some(user)` is who it was issued to. A
/// sign-in challenge may be stored gated; a row exists under one hash only, and each take is
/// atomic, so it is still single use. Registration never looks at the gated form.
async fn take_challenge(
    ctx: &Ctx,
    client_data_json: &[u8],
    purpose: ChallengePurpose,
) -> Result<(Vec<u8>, Option<UserId>)> {
    let raw = client_data::challenge(client_data_json)?;
    let hash = challenge_hash(ctx, &raw).await?;
    let mut taken = ctx.store.take_challenge(&hash, purpose, ctx.now).await?;
    if taken.is_none() && matches!(purpose, ChallengePurpose::Login) {
        let hash = gated_hash(ctx, &raw).await?;
        taken = ctx.store.take_challenge(&hash, purpose, ctx.now).await?;
    }
    match taken {
        Some(user) => Ok((raw, user)),
        None => Err(invalid(format!(
            "{} challenge unknown, used, expired or of another purpose",
            purpose.as_str()
        ))),
    }
}

fn credential_json(id: &str) -> String {
    object(&[
        ("type", Out::Str("public-key".to_owned())),
        ("id", Out::Str(id.to_owned())),
    ])
}

fn passkey_json(p: &Passkey) -> String {
    object(&[
        ("id", Out::Int(p.id)),
        ("name", Out::Str(p.name.clone())),
        ("created_at", Out::Int(p.created_at)),
        ("last_used_at", p.last_used_at.map_or(Out::Null, Out::Int)),
    ])
}

/// `POST /api/auth/passkey/register/options`: signed-in users add a passkey to themselves.
pub async fn register_options(ctx: &Ctx, p: &Principal, body: &[u8]) -> Result<Reply> {
    let user = p.user()?;
    no_fields(body)?;
    let site = site::site(ctx).await?;
    let existing = ctx.store.list_passkeys(user.id).await?;
    let challenge = new_challenge(ctx, ChallengePurpose::Register, Some(user.id), false).await?;
    let algs: Vec<String> = [cose::ALG_ES256, cose::ALG_EDDSA, cose::ALG_RS256]
        .iter()
        .map(|a| format!(r#"{{"type":"public-key","alg":{a}}}"#))
        .collect();
    let exclude: Vec<String> = existing
        .iter()
        .map(|k| credential_json(&k.credential_id))
        .collect();
    let public_key = object(&[
        ("challenge", Out::Str(challenge)),
        (
            "rp",
            Out::Raw(object(&[
                ("id", Out::Str(site.host.clone())),
                ("name", Out::Str(site.host)),
            ])),
        ),
        (
            "user",
            Out::Raw(object(&[
                // Opaque and stable: the user id, never the login (it can change).
                ("id", Out::Str(base64url::encode(&user.id.0.to_be_bytes()))),
                ("name", Out::Str(user.login.clone())),
                ("displayName", Out::Str(user.display_name.clone())),
            ])),
        ),
        ("pubKeyCredParams", Out::Raw(array(&algs))),
        ("timeout", Out::Int(CHALLENGE_TTL_MS)),
        ("excludeCredentials", Out::Raw(array(&exclude))),
        (
            "authenticatorSelection",
            Out::Raw(object(&[
                ("residentKey", Out::Str("required".to_owned())),
                ("requireResidentKey", Out::Raw("true".to_owned())),
                ("userVerification", Out::Str("required".to_owned())),
            ])),
        ),
        ("attestation", Out::Str("none".to_owned())),
    ]);
    Ok(Reply::json(
        200,
        object(&[("publicKey", Out::Raw(public_key))]),
    ))
}

/// Adding or removing a passkey needs the current password or an assertion by one of the
/// user's own passkeys, rate limited and counted like sign-in.
///
/// # Errors
///
/// `ReauthRequired` without either, `InvalidInput` with both, else as the check used.
async fn reauthenticate(ctx: &Ctx, user: &User, f: &Fields, how: &str) -> Result<()> {
    let password = f.opt("current_password")?.is_some();
    let assertion = f.opt("reauth_credential_id")?.is_some();
    match (password, assertion) {
        (true, true) => Err(Error::new(
            ErrorCode::InvalidInput,
            "current_password and a passkey assertion: send one",
        )),
        (true, false) => auth::check_current(ctx, user, f).await,
        (false, true) => verify(ctx, f, REAUTH, Some(user.id), how).await.map(drop),
        (false, false) => Err(Error::new(
            ErrorCode::ReauthRequired,
            format!("{how}: no current_password or passkey assertion"),
        )),
    }
}

/// `POST /api/auth/passkey/register/verify`.
pub async fn register_verify(ctx: &Ctx, p: &Principal, body: &[u8]) -> Result<Reply> {
    let user = p.user()?;
    // Before the challenge is consumed: a site without an origin refuses, nothing spent.
    let site = site::site(ctx).await?;
    let mut allowed = vec!["name", "client_data_json", "attestation_object"];
    allowed.extend_from_slice(REAUTH_FIELDS);
    let f = Fields::parse(body, &allowed)?;
    let name = f.req("name")?;
    fields::passkey_name(name)?;
    let client_data_json = b64(&f, "", "client_data_json")?;
    let attestation_object = b64(&f, "", "attestation_object")?;
    // Before the challenge: a stolen session cookie alone must not add a key that outlives a
    // password change.
    reauthenticate(ctx, user, &f, "passkey add").await?;
    let (challenge, issued_to) =
        take_challenge(ctx, &client_data_json, ChallengePurpose::Register).await?;
    if issued_to != Some(user.id) {
        return Err(invalid(format!(
            "register challenge issued to {issued_to:?}, used by user {}",
            user.id.0
        )));
    }
    let cred = webauthn::verify_registration(
        &ctx.crypto,
        &attestation_object,
        &client_data_json,
        &Expected {
            challenge: &challenge,
            origin: &site.origin,
            rp_id: &site.host,
        },
    )
    .await?;
    let id = ctx
        .store
        .add_passkey(NewPasskey {
            user_id: user.id,
            credential_id: cred.credential_id,
            public_key: cred.public_key,
            sign_count: i64::from(cred.sign_count),
            name: name.to_owned(),
            created_at: ctx.now,
        })
        .await?;
    ctx.syslog(
        ctx.event(EventKind::PasskeyAdded, Some(user.id))
            .subject(name),
    )
    .await;
    let added = ctx
        .store
        .list_passkeys(user.id)
        .await?
        .into_iter()
        .find(|k| k.id == id)
        .ok_or_else(|| Error::new(ErrorCode::NotFound, format!("passkey {id} gone after add")))?;
    Ok(Reply::json(
        201,
        object(&[("passkey", Out::Raw(passkey_json(&added)))]),
    ))
}

/// `POST /api/auth/passkey/login/options`: anyone. The credential is discoverable, so the
/// browser picks it; the server names no user (nothing to enumerate).
pub async fn login_options(ctx: &Ctx, body: &[u8]) -> Result<Reply> {
    let token = if body.iter().all(u8::is_ascii_whitespace) {
        None
    } else {
        Fields::parse(body, &["turnstile_token"])?
            .opt("turnstile_token")?
            .map(str::to_owned)
    };
    let site = site::site(ctx).await?;
    // Anonymous and each call writes a row: counted site-wide and per address before the
    // write. Past the cap with a token, the challenge carries that proof to the sign-in.
    let gated = limit::options(ctx, token.as_deref()).await?;
    let challenge = new_challenge(ctx, ChallengePurpose::Login, None, gated).await?;
    let public_key = object(&[
        ("challenge", Out::Str(challenge)),
        ("rpId", Out::Str(site.host)),
        ("timeout", Out::Int(CHALLENGE_TTL_MS)),
        ("userVerification", Out::Str("required".to_owned())),
        ("allowCredentials", Out::Raw("[]".to_owned())),
    ]);
    Ok(Reply::json(
        200,
        object(&[("publicKey", Out::Raw(public_key))]),
    ))
}

/// Why an assertion was refused, and whose account it touched. Every one answers
/// `auth.passkey_invalid`; the reason goes to the logs only.
struct Refused {
    actor: Option<UserId>,
    reason: &'static str,
    detail: String,
}

fn refused(actor: Option<UserId>, reason: &'static str, detail: impl Into<String>) -> Refused {
    Refused {
        actor,
        reason,
        detail: detail.into(),
    }
}

/// The checks of one assertion, in order. `Err(Ok(refused))` is the client's failure (counted);
/// `Err(Err(e))` is ours (storage, crypto, a corrupt stored key) and is not counted.
/// `challenge_taken`: the limiter already consumed the challenge as its site-wide gate proof.
async fn check_assertion(
    ctx: &Ctx,
    f: &Fields,
    prefix: &str,
    credential_id: &str,
    want_user: Option<UserId>,
    site: &site::Site,
    challenge_taken: bool,
) -> std::result::Result<User, std::result::Result<Refused, Error>> {
    let ours = |e: Error| {
        if e.code == ErrorCode::PasskeyInvalid {
            Ok(refused(want_user, "malformed or unverifiable", e.detail))
        } else {
            Err(e)
        }
    };
    let client_data_json = b64(f, prefix, "client_data_json").map_err(ours)?;
    let authenticator_data = b64(f, prefix, "authenticator_data").map_err(ours)?;
    let signature = b64(f, prefix, "signature").map_err(ours)?;
    let challenge = if challenge_taken {
        client_data::challenge(&client_data_json).map_err(ours)?
    } else {
        take_challenge(ctx, &client_data_json, ChallengePurpose::Login)
            .await
            .map_err(ours)?
            .0
    };
    let Some(key) = ctx.store.find_passkey(credential_id).await.map_err(Err)? else {
        return Err(Ok(refused(
            want_user,
            "unknown credential",
            "no such credential",
        )));
    };
    let owner = Some(key.user_id);
    if let Some(want) = want_user
        && want != key.user_id
    {
        return Err(Ok(refused(
            Some(want),
            "credential of another account",
            format!("credential of user {}", key.user_id.0),
        )));
    }
    let user = match ctx.store.get_user(key.user_id).await {
        Ok(u) => u,
        Err(e) if e.code == ErrorCode::UserNotFound => {
            return Err(Ok(refused(None, "unknown credential", "user gone")));
        }
        Err(e) => return Err(Err(e)),
    };
    if user.disabled_at.is_some() {
        return Err(Ok(refused(owner, "disabled account", "account disabled")));
    }
    let count = webauthn::verify_assertion(
        &ctx.crypto,
        &key.public_key,
        &authenticator_data,
        &client_data_json,
        &signature,
        &Expected {
            challenge: &challenge,
            origin: &site.origin,
            rp_id: &site.host,
        },
    )
    .await
    .map_err(|e| ours(e).map(|r| Refused { actor: owner, ..r }))?;
    match ctx
        .store
        .touch_passkey(key.id, i64::from(count), ctx.now)
        .await
    {
        Ok(true) => Ok(user),
        Ok(false) => Err(Ok(refused(
            owner,
            "sign counter did not increase (cloned authenticator?)",
            format!("passkey {} counter {count} <= {}", key.id, key.sign_count),
        ))),
        // Removed between the lookup and now.
        Err(e) if e.code == ErrorCode::NotFound => Err(Ok(refused(
            owner,
            "unknown credential",
            "removed meanwhile",
        ))),
        Err(e) => Err(Err(e)),
    }
}

/// Verifies a passkey assertion end to end and returns its user, rate limited per credential
/// id and per address like password sign-in. `prefix` is put before every assertion field
/// name. `want_user`: the assertion must be theirs (re-authentication). `how` names the
/// attempt in the system log.
///
/// # Errors
///
/// `auth.passkey_invalid` for every refused assertion (counted and logged), `Locked` /
/// `TurnstileRequired` from the limiter, `InvalidInput` for a body without a usable
/// credential id; storage and crypto errors pass through.
pub async fn verify(
    ctx: &Ctx,
    f: &Fields,
    prefix: &str,
    want_user: Option<UserId>,
    how: &str,
) -> Result<User> {
    let credential_id = f.req(&format!("{prefix}credential_id"))?;
    if credential_id.is_empty() || credential_id.len() > MAX_CREDENTIAL_ID_TEXT {
        return Err(Error::new(
            ErrorCode::InvalidInput,
            "credential_id: empty or too long",
        ));
    }
    // First: without a configured origin nothing is counted or consumed.
    let site = site::site(ctx).await?;
    let keys = Keys::passkey(ctx, credential_id).await?;
    // Re-auth (`want_user`) comes from a signed-in user: outside the site-wide cap.
    let token = f.opt("turnstile_token")?;
    let (attempt, challenge_taken) = if want_user.is_some() {
        (limit::check_signed_in(ctx, &keys, token).await?, false)
    } else {
        // The challenge's gated hash, if the body names one: at the cap it may stand for a
        // Turnstile token. A malformed one is refused, and counted, by check_assertion.
        let raw = b64(f, prefix, "client_data_json")
            .ok()
            .and_then(|cd| client_data::challenge(&cd).ok());
        let gated = match raw {
            Some(raw) => Some(gated_hash(ctx, &raw).await?),
            None => None,
        };
        limit::check_passkey(ctx, &keys, token, gated.as_deref()).await?
    };
    match check_assertion(
        ctx,
        f,
        prefix,
        credential_id,
        want_user,
        &site,
        challenge_taken,
    )
    .await
    {
        Ok(user) => {
            limit::succeeded(ctx, &keys).await?;
            Ok(user)
        }
        Err(Err(e)) => Err(e),
        Err(Ok(r)) => {
            limit::failed(ctx, &attempt, r.actor, how).await;
            ctx.syslog(
                ctx.event(EventKind::SignInFailed, r.actor)
                    .subject(how)
                    .field("reason", r.reason),
            )
            .await;
            Err(invalid(format!("{}: {}", r.reason, r.detail)))
        }
    }
}

/// `POST /api/auth/passkey/login/verify`: signs in like a password does (session rotated).
pub async fn login_verify(ctx: &Ctx, p: &Principal, body: &[u8]) -> Result<Reply> {
    let mut allowed = ASSERTION_FIELDS.to_vec();
    allowed.push("turnstile_token");
    let f = Fields::parse(body, &allowed)?;
    let user = verify(ctx, &f, "", None, "passkey").await?;
    auth::sign_in(ctx, p, &user, 200, "passkey").await
}

/// The user's passkeys as a JSON array (no credential ids or keys).
///
/// # Errors
///
/// Storage errors pass through.
pub async fn list_json(ctx: &Ctx, user: UserId) -> Result<String> {
    let keys: Vec<String> = ctx
        .store
        .list_passkeys(user)
        .await?
        .iter()
        .map(passkey_json)
        .collect();
    Ok(array(&keys))
}

/// `GET /api/auth/passkey`: the signed-in user's own passkeys.
pub async fn list(ctx: &Ctx, p: &Principal) -> Result<Reply> {
    let user = p.user()?;
    Ok(Reply::json(
        200,
        object(&[("passkeys", Out::Raw(list_json(ctx, user.id).await?))]),
    ))
}

/// `DELETE /api/auth/passkey/<id>`: removes one of the signed-in user's own passkeys, after
/// re-authentication (body: [`REAUTH_FIELDS`]). Another user's id answers like a missing one,
/// before anything is counted. The last passkey of an account without a password stays:
/// removing it would lock the account out.
pub async fn delete(ctx: &Ctx, p: &Principal, id: &str, body: &[u8]) -> Result<Reply> {
    let user = p.user()?;
    let not_found = || {
        Error::new(
            ErrorCode::NotFound,
            format!("passkey {id:?} of user {}", user.id.0),
        )
    };
    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
        return Err(not_found());
    }
    let id: i64 = id.parse().map_err(|_| not_found())?;
    let name = ctx
        .store
        .list_passkeys(user.id)
        .await?
        .into_iter()
        .find(|k| k.id == id)
        .map(|k| k.name)
        .ok_or_else(not_found)?;
    let f = if body.iter().all(u8::is_ascii_whitespace) {
        Fields::parse(b"{}", &[])?
    } else {
        Fields::parse(body, REAUTH_FIELDS)?
    };
    reauthenticate(ctx, user, &f, "passkey remove").await?;
    // The store refuses another user's id (NotFound) and, inside the same write, the last
    // passkey of an account without a password (Conflict).
    ctx.store.delete_own_passkey(user.id, id).await?;
    ctx.syslog(
        ctx.event(EventKind::PasskeyRemoved, Some(user.id))
            .subject(&name),
    )
    .await;
    Ok(Reply::empty())
}
