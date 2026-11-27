//! Passkey (`WebAuthn` level 2) verification for "none" attestation and ES256, RS256 and Ed25519
//! keys. Pure parsing and checks; hashing and signature verification go through [`Crypto`].
//!
//! What the caller still owns (needs storage): issuing a random single-use challenge and
//! consuming it atomically, looking up the credential id, and refusing a sign count that does
//! not move forward (`touch_passkey`).

pub mod client_data;
pub mod cose;
pub mod der;

pub use cose::{Es256Key, PublicKey, Rs256Key};

use crate::{Crypto, base64url, cbor, constant_time_eq};
use plumblog_model::{Error, ErrorCode, Result};

pub const FLAG_UP: u8 = 0x01;
pub const FLAG_UV: u8 = 0x04;
pub const FLAG_BE: u8 = 0x08;
pub const FLAG_BS: u8 = 0x10;
pub const FLAG_AT: u8 = 0x40;
pub const FLAG_ED: u8 = 0x80;

/// `WebAuthn` caps credential ids at 1023 bytes.
pub const MAX_CREDENTIAL_ID_LEN: usize = 1023;
/// Bounds work on hostile input; a "none" attestation with a 4096-bit RSA key is under 1 KiB.
pub const MAX_ATTESTATION_OBJECT_LEN: usize = 16 * 1024;
pub const MAX_AUTHENTICATOR_DATA_LEN: usize = 16 * 1024;
/// RSA-4096 signatures are 512 bytes; DER ES256 at most 72; Ed25519 64.
pub const MAX_SIGNATURE_LEN: usize = 512;

/// What a ceremony must match, all from the server's own state, never from the request.
#[derive(Clone, Copy, Debug)]
pub struct Expected<'a> {
    /// The raw challenge bytes the server issued for this ceremony.
    pub challenge: &'a [u8],
    /// Site origin, e.g. `https://blog.example` (scheme, host, port if not default; no slash).
    pub origin: &'a str,
    /// Relying party id: the site host, e.g. `blog.example`.
    pub rp_id: &'a str,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttestedCredential<'a> {
    pub aaguid: [u8; 16],
    pub credential_id: &'a [u8],
    pub public_key: PublicKey,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthenticatorData<'a> {
    pub rp_id_hash: [u8; 32],
    pub flags: u8,
    pub sign_count: u32,
    pub attested: Option<AttestedCredential<'a>>,
}

/// A verified registration, ready for `NewPasskey`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewCredential {
    /// Base64url (no padding) of the raw credential id: the same text the browser reports as
    /// `PublicKeyCredential.id`, and what `passkeys.credential_id` holds.
    pub credential_id: String,
    /// [`PublicKey::to_stored`] form.
    pub public_key: String,
    pub sign_count: u32,
}

fn invalid(detail: impl Into<String>) -> Error {
    Error::new(ErrorCode::PasskeyInvalid, detail.into())
}

fn split(input: &[u8], n: usize) -> Result<(&[u8], &[u8])> {
    if input.len() < n {
        return Err(invalid("authenticator data truncated"));
    }
    Ok(input.split_at(n))
}

/// Parses authenticator data: fixed header, attested credential data when `AT` is set,
/// extensions (a CBOR map, skipped) when `ED` is set, and nothing after.
///
/// # Errors
///
/// [`ErrorCode::PasskeyInvalid`] when truncated, over-long, inconsistent with its flags or
/// carrying an unsupported key.
pub fn parse_authenticator_data(data: &[u8]) -> Result<AuthenticatorData<'_>> {
    if data.len() > MAX_AUTHENTICATOR_DATA_LEN {
        return Err(invalid("authenticator data too long"));
    }
    let (rp_id_hash, rest) = split(data, 32)?;
    let (flags, rest) = split(rest, 1)?;
    let (count, mut rest) = split(rest, 4)?;
    let flags = flags[0];
    // Backed up without being backup eligible is impossible per the spec.
    if flags & FLAG_BS != 0 && flags & FLAG_BE == 0 {
        return Err(invalid("BS flag without BE"));
    }
    let attested = if flags & FLAG_AT == 0 {
        None
    } else {
        let (aaguid, r) = split(rest, 16)?;
        let (len, r) = split(r, 2)?;
        let len = usize::from(u16::from_be_bytes([len[0], len[1]]));
        if len == 0 || len > MAX_CREDENTIAL_ID_LEN {
            return Err(invalid(format!("credential id length {len}")));
        }
        let (credential_id, r) = split(r, len)?;
        let (key, used) = cbor::decode_prefix(r)
            .map_err(|e| invalid(format!("credential public key: {}", e.detail)))?;
        rest = &r[used..];
        Some(AttestedCredential {
            aaguid: aaguid.try_into().map_err(|_| invalid("aaguid"))?,
            credential_id,
            public_key: PublicKey::from_cose(&key)?,
        })
    };
    if flags & FLAG_ED != 0 {
        let (ext, used) =
            cbor::decode_prefix(rest).map_err(|e| invalid(format!("extensions: {}", e.detail)))?;
        if ext.as_map().is_none() {
            return Err(invalid("extensions are not a map"));
        }
        rest = &rest[used..];
    }
    if !rest.is_empty() {
        return Err(invalid(format!(
            "{} trailing bytes in authenticator data",
            rest.len()
        )));
    }
    Ok(AuthenticatorData {
        rp_id_hash: rp_id_hash.try_into().map_err(|_| invalid("rpIdHash"))?,
        flags,
        sign_count: u32::from_be_bytes([count[0], count[1], count[2], count[3]]),
        attested,
    })
}

/// rpIdHash and the user present + verified flags, common to both ceremonies.
async fn check_rp_and_flags(
    crypto: &impl Crypto,
    auth: &AuthenticatorData<'_>,
    rp_id: &str,
) -> Result<()> {
    let expected = crypto.sha256(rp_id.as_bytes()).await?;
    if !constant_time_eq(&auth.rp_id_hash, &expected) {
        return Err(invalid("rpIdHash does not match the rp id"));
    }
    if auth.flags & FLAG_UP == 0 {
        return Err(invalid("user present flag not set"));
    }
    if auth.flags & FLAG_UV == 0 {
        return Err(invalid("user verified flag not set"));
    }
    Ok(())
}

/// Verifies a registration (`navigator.credentials.create`) with "none" attestation.
///
/// # Errors
///
/// [`ErrorCode::PasskeyInvalid`] for any failed check or malformed input; a [`Crypto`] error
/// (platform failure) passes through.
pub async fn verify_registration(
    crypto: &impl Crypto,
    attestation_object: &[u8],
    client_data_json: &[u8],
    expected: &Expected<'_>,
) -> Result<NewCredential> {
    client_data::check(
        client_data_json,
        client_data::TYPE_CREATE,
        expected.challenge,
        expected.origin,
    )?;
    if attestation_object.len() > MAX_ATTESTATION_OBJECT_LEN {
        return Err(invalid("attestation object too long"));
    }
    let att = cbor::decode(attestation_object)
        .map_err(|e| invalid(format!("attestation object: {}", e.detail)))?;
    if att.as_map().is_none() {
        return Err(invalid("attestation object is not a map"));
    }
    match att.get_text("fmt").and_then(cbor::Value::as_text) {
        Some("none") => {}
        other => return Err(invalid(format!("attestation format {other:?}, want none"))),
    }
    if att.get_text("attStmt").and_then(cbor::Value::as_map) != Some(&[]) {
        return Err(invalid("attStmt must be an empty map for none"));
    }
    let auth_data = att
        .get_text("authData")
        .and_then(cbor::Value::as_bytes)
        .ok_or_else(|| invalid("authData missing or not bytes"))?;
    let auth = parse_authenticator_data(auth_data)?;
    check_rp_and_flags(crypto, &auth, expected.rp_id).await?;
    let cred = auth
        .attested
        .ok_or_else(|| invalid("no attested credential data"))?;
    Ok(NewCredential {
        credential_id: base64url::encode(cred.credential_id),
        public_key: cred.public_key.to_stored(),
        sign_count: auth.sign_count,
    })
}

/// Verifies a sign-in (`navigator.credentials.get`) against the stored key of the credential
/// the browser named. Returns the authenticator's new sign count for `touch_passkey`.
///
/// # Errors
///
/// [`ErrorCode::PasskeyInvalid`] for any failed check, malformed input or a signature that does
/// not verify; [`ErrorCode::Internal`] when the stored key is corrupt; a [`Crypto`] error passes
/// through.
pub async fn verify_assertion(
    crypto: &impl Crypto,
    stored_public_key: &str,
    authenticator_data: &[u8],
    client_data_json: &[u8],
    signature: &[u8],
    expected: &Expected<'_>,
) -> Result<u32> {
    let key = PublicKey::from_stored(stored_public_key)?;
    client_data::check(
        client_data_json,
        client_data::TYPE_GET,
        expected.challenge,
        expected.origin,
    )?;
    let auth = parse_authenticator_data(authenticator_data)?;
    if auth.attested.is_some() {
        return Err(invalid("assertion carries attested credential data"));
    }
    check_rp_and_flags(crypto, &auth, expected.rp_id).await?;
    if signature.len() > MAX_SIGNATURE_LEN {
        return Err(invalid("signature too long"));
    }
    let client_hash = crypto.sha256(client_data_json).await?;
    let mut message = Vec::with_capacity(authenticator_data.len() + 32);
    message.extend_from_slice(authenticator_data);
    message.extend_from_slice(&client_hash);
    let ok = match &key {
        PublicKey::Es256(k) => {
            let raw = der::ecdsa_der_to_raw(signature)?;
            crypto.verify_es256(k, &message, &raw).await?
        }
        PublicKey::Rs256(k) => {
            if signature.len() != k.n.len() {
                return Err(invalid(format!(
                    "RS256 signature is {} bytes, modulus {}",
                    signature.len(),
                    k.n.len()
                )));
            }
            crypto.verify_rs256(k, &message, signature).await?
        }
        PublicKey::Ed25519(k) => {
            let sig: &[u8; 64] = signature
                .try_into()
                .map_err(|_| invalid("Ed25519 signature is not 64 bytes"))?;
            crypto.verify_ed25519(k, &message, sig).await?
        }
    };
    if !ok {
        return Err(invalid("signature does not verify"));
    }
    Ok(auth.sign_count)
}

#[cfg(test)]
pub(crate) mod tests;
