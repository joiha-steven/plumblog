//! COSE public keys (RFC 9053) for the three algorithms we accept, and the form they are stored
//! in (`passkeys.public_key`).
//!
//! Stored form, one ASCII line, base64url without padding:
//! - `es256:<x || y>` (64 bytes: P-256 affine coordinates, big-endian)
//! - `rs256:<n>:<e>` (modulus and exponent, big-endian, no leading zero bytes)
//! - `ed25519:<x>` (32-byte public key)
//!
//! Text rather than COSE bytes so a row is readable in a D1 console, and parsing it back needs no
//! CBOR. Both directions apply the same checks.

use crate::base64url;
use crate::cbor::Value;
use plumblog_model::{Error, ErrorCode, Result};

pub const ALG_ES256: i128 = -7;
pub const ALG_RS256: i128 = -257;
pub const ALG_EDDSA: i128 = -8;

const KTY_OKP: i128 = 1;
const KTY_EC2: i128 = 2;
const KTY_RSA: i128 = 3;
const CRV_P256: i128 = 1;
const CRV_ED25519: i128 = 6;

/// RSA moduli below 2048 bits are breakable; above 4096 nobody ships them and they cost CPU.
const RSA_MIN_BYTES: usize = 256;
const RSA_MAX_BYTES: usize = 512;
const RSA_MAX_EXPONENT_BYTES: usize = 4;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Es256Key {
    pub x: [u8; 32],
    pub y: [u8; 32],
}

impl Es256Key {
    /// SEC1 uncompressed point (`04 || x || y`), the `raw` format `WebCrypto` imports.
    #[must_use]
    pub fn uncompressed_point(&self) -> [u8; 65] {
        let mut out = [0u8; 65];
        out[0] = 0x04;
        out[1..33].copy_from_slice(&self.x);
        out[33..].copy_from_slice(&self.y);
        out
    }
}

/// RSASSA-PKCS1-v1_5 with SHA-256. Both fields big-endian without leading zero bytes, as a
/// JWK (`n`, `e` base64url) needs them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rs256Key {
    pub n: Vec<u8>,
    pub e: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PublicKey {
    Es256(Es256Key),
    Rs256(Rs256Key),
    Ed25519([u8; 32]),
}

fn invalid(detail: impl Into<String>) -> Error {
    Error::new(
        ErrorCode::PasskeyInvalid,
        format!("cose key: {}", detail.into()),
    )
}

fn array32(bytes: &[u8]) -> Option<[u8; 32]> {
    bytes.try_into().ok()
}

fn strip_leading_zeros(bytes: &[u8]) -> &[u8] {
    let start = bytes.iter().position(|b| *b != 0).unwrap_or(bytes.len());
    &bytes[start..]
}

fn rsa_key(n: &[u8], e: &[u8]) -> std::result::Result<Rs256Key, String> {
    let n = strip_leading_zeros(n);
    let e = strip_leading_zeros(e);
    if !(RSA_MIN_BYTES..=RSA_MAX_BYTES).contains(&n.len()) {
        return Err(format!(
            "RSA modulus is {} bits, want 2048-4096",
            n.len() * 8
        ));
    }
    // A modulus is a product of two odd primes, so it is odd.
    if n[n.len() - 1] & 1 == 0 {
        return Err("RSA modulus is even".into());
    }
    if e.is_empty() || e.len() > RSA_MAX_EXPONENT_BYTES || e[e.len() - 1] & 1 == 0 || e == [1] {
        return Err("RSA exponent must be odd, at least 3 and at most 32 bits".into());
    }
    Ok(Rs256Key {
        n: n.to_vec(),
        e: e.to_vec(),
    })
}

impl PublicKey {
    /// The COSE algorithm identifier.
    #[must_use]
    pub fn alg(&self) -> i128 {
        match self {
            Self::Es256(_) => ALG_ES256,
            Self::Rs256(_) => ALG_RS256,
            Self::Ed25519(_) => ALG_EDDSA,
        }
    }

    /// Reads a decoded `credentialPublicKey`.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::PasskeyInvalid`] for any other algorithm or curve, a missing or mistyped
    /// parameter, private key material, or a label we do not know.
    pub fn from_cose(value: &Value<'_>) -> Result<Self> {
        let map = value.as_map().ok_or_else(|| invalid("not a map"))?;
        let int = |label: i128| -> Result<i128> {
            value
                .get_int(label)
                .and_then(Value::as_int)
                .ok_or_else(|| invalid(format!("label {label} missing or not an integer")))
        };
        let bytes = |label: i128| -> Result<&[u8]> {
            value
                .get_int(label)
                .and_then(Value::as_bytes)
                .ok_or_else(|| invalid(format!("label {label} missing or not a byte string")))
        };
        let (kty, alg) = (int(1)?, int(3)?);
        // Common labels 1..=5 (kty, kid, alg, key_ops, base IV) plus the ones this key type
        // defines as public. Anything else (private parts, unknown labels) is refused.
        let public_labels: &[i128] = match (kty, alg) {
            (KTY_EC2, ALG_ES256) => &[-1, -2, -3],
            (KTY_RSA, ALG_RS256) | (KTY_OKP, ALG_EDDSA) => &[-1, -2],
            _ => return Err(invalid(format!("unsupported kty {kty} / alg {alg}"))),
        };
        for (k, _) in map {
            match k.as_int() {
                Some(1..=5) => {}
                Some(l) if public_labels.contains(&l) => {}
                _ => return Err(invalid(format!("unexpected label {k:?}"))),
            }
        }
        match alg {
            ALG_ES256 => {
                let crv = int(-1)?;
                if crv != CRV_P256 {
                    return Err(invalid(format!("ES256 curve {crv}, want P-256 (1)")));
                }
                let x = array32(bytes(-2)?).ok_or_else(|| invalid("x is not 32 bytes"))?;
                // A boolean y is point compression, which WebAuthn does not allow.
                let y = array32(bytes(-3)?).ok_or_else(|| invalid("y is not 32 bytes"))?;
                Ok(Self::Es256(Es256Key { x, y }))
            }
            ALG_RS256 => Ok(Self::Rs256(
                rsa_key(bytes(-1)?, bytes(-2)?).map_err(invalid)?,
            )),
            _ => {
                let crv = int(-1)?;
                if crv != CRV_ED25519 {
                    return Err(invalid(format!("EdDSA curve {crv}, want Ed25519 (6)")));
                }
                let x = array32(bytes(-2)?).ok_or_else(|| invalid("x is not 32 bytes"))?;
                Ok(Self::Ed25519(x))
            }
        }
    }

    /// The `passkeys.public_key` column value (format in the module docs).
    #[must_use]
    pub fn to_stored(&self) -> String {
        match self {
            Self::Es256(k) => {
                let mut xy = [0u8; 64];
                xy[..32].copy_from_slice(&k.x);
                xy[32..].copy_from_slice(&k.y);
                format!("es256:{}", base64url::encode(&xy))
            }
            Self::Rs256(k) => format!(
                "rs256:{}:{}",
                base64url::encode(&k.n),
                base64url::encode(&k.e)
            ),
            Self::Ed25519(x) => format!("ed25519:{}", base64url::encode(x)),
        }
    }

    /// Parses a stored key back.
    ///
    /// # Errors
    ///
    /// [`ErrorCode::Internal`]: a stored key we cannot read is corrupt data, not bad input.
    pub fn from_stored(stored: &str) -> Result<Self> {
        let corrupt = |why: &str| {
            Error::new(
                ErrorCode::Internal,
                format!("stored passkey public key is corrupt: {why}"),
            )
        };
        let (kind, rest) = stored.split_once(':').ok_or_else(|| corrupt("no prefix"))?;
        let decode = |s: &str| base64url::decode(s).ok_or_else(|| corrupt("bad base64url"));
        match kind {
            "es256" => {
                let xy = decode(rest)?;
                if xy.len() != 64 {
                    return Err(corrupt("es256 key is not 64 bytes"));
                }
                let x = array32(&xy[..32]).ok_or_else(|| corrupt("x"))?;
                let y = array32(&xy[32..]).ok_or_else(|| corrupt("y"))?;
                Ok(Self::Es256(Es256Key { x, y }))
            }
            "rs256" => {
                let (n, e) = rest.split_once(':').ok_or_else(|| corrupt("no exponent"))?;
                let (n, e) = (decode(n)?, decode(e)?);
                // Stored keys were stripped when written; a leading zero means someone else wrote it.
                if n.first() == Some(&0) || e.first() == Some(&0) {
                    return Err(corrupt("leading zero byte"));
                }
                rsa_key(&n, &e)
                    .map(Self::Rs256)
                    .map_err(|why| corrupt(&why))
            }
            "ed25519" => {
                let x = decode(rest)?;
                array32(&x)
                    .map(Self::Ed25519)
                    .ok_or_else(|| corrupt("ed25519 key is not 32 bytes"))
            }
            _ => Err(corrupt("unknown key type")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Es256Key, PublicKey, Rs256Key};
    use crate::cbor::decode;
    use crate::webauthn::tests::{CoseMap, cose_ed25519, cose_es256, cose_rs256};
    use plumblog_model::ErrorCode;

    fn rsa_n() -> Vec<u8> {
        let mut n = vec![0xc5; 256];
        n[255] = 0x01;
        n
    }

    #[test]
    fn parses_the_three_algorithms_and_round_trips_stored_form() {
        let es = PublicKey::from_cose(&decode(&cose_es256(&[1; 32], &[2; 32])).unwrap()).unwrap();
        assert_eq!(
            es,
            PublicKey::Es256(Es256Key {
                x: [1; 32],
                y: [2; 32]
            })
        );
        let rs = PublicKey::from_cose(&decode(&cose_rs256(&rsa_n(), &[1, 0, 1])).unwrap()).unwrap();
        assert_eq!(
            rs,
            PublicKey::Rs256(Rs256Key {
                n: rsa_n(),
                e: vec![1, 0, 1]
            })
        );
        let ed = PublicKey::from_cose(&decode(&cose_ed25519(&[3; 32])).unwrap()).unwrap();
        assert_eq!(ed, PublicKey::Ed25519([3; 32]));
        for key in [es, rs, ed] {
            let stored = key.to_stored();
            assert!(stored.is_ascii());
            assert_eq!(PublicKey::from_stored(&stored).unwrap(), key);
        }
        let mut point = [0u8; 65];
        point[0] = 4;
        point[1..33].fill(1);
        point[33..].fill(2);
        assert_eq!(
            Es256Key {
                x: [1; 32],
                y: [2; 32]
            }
            .uncompressed_point(),
            point
        );
    }

    #[test]
    fn rsa_leading_zeros_are_stripped() {
        let mut padded = vec![0];
        padded.extend(rsa_n());
        let key =
            PublicKey::from_cose(&decode(&cose_rs256(&padded, &[0, 1, 0, 1])).unwrap()).unwrap();
        assert_eq!(
            key,
            PublicKey::Rs256(Rs256Key {
                n: rsa_n(),
                e: vec![1, 0, 1]
            })
        );
    }

    #[test]
    fn refuses_other_algorithms_curves_and_shapes() {
        let es = |m: CoseMap| m.encode();
        let cases: Vec<(&str, Vec<u8>)> = vec![
            (
                "ES384 alg",
                es(CoseMap::es256(&[1; 32], &[2; 32]).set(3, -35)),
            ),
            (
                "PS256 alg",
                es(CoseMap::es256(&[1; 32], &[2; 32]).set(3, -37)),
            ),
            (
                "P-384 curve",
                es(CoseMap::es256(&[1; 32], &[2; 32]).set(-1, 2)),
            ),
            (
                "kty mismatch",
                es(CoseMap::es256(&[1; 32], &[2; 32]).set(1, 1)),
            ),
            ("short x", es(CoseMap::es256(&[1; 31], &[2; 32]))),
            ("long y", es(CoseMap::es256(&[1; 32], &[2; 33]))),
            (
                "compressed y",
                es(CoseMap::es256(&[1; 32], &[2; 32]).set_bool(-3, true)),
            ),
            (
                "private d",
                es(CoseMap::es256(&[1; 32], &[2; 32]).set_bytes(-4, &[5; 32])),
            ),
            (
                "unknown label",
                es(CoseMap::es256(&[1; 32], &[2; 32]).set(99, 0)),
            ),
            ("no alg", es(CoseMap::es256(&[1; 32], &[2; 32]).remove(3))),
            ("no x", es(CoseMap::es256(&[1; 32], &[2; 32]).remove(-2))),
            ("X25519 curve", es(CoseMap::ed25519(&[3; 32]).set(-1, 4))),
            ("Ed448 curve", es(CoseMap::ed25519(&[3; 32]).set(-1, 7))),
            (
                "ed private d",
                es(CoseMap::ed25519(&[3; 32]).set_bytes(-4, &[1; 32])),
            ),
            ("rsa 1024", cose_rs256(&[0xc5; 128], &[1, 0, 1])),
            ("rsa 8192", cose_rs256(&[0xc5; 1024], &[1, 0, 1])),
            ("rsa even n", cose_rs256(&[0xc4; 256], &[1, 0, 1])),
            ("rsa e = 1", cose_rs256(&rsa_n(), &[1])),
            ("rsa even e", cose_rs256(&rsa_n(), &[1, 0, 0])),
            ("rsa empty e", cose_rs256(&rsa_n(), &[])),
            ("rsa huge e", cose_rs256(&rsa_n(), &[1; 5])),
            (
                "rsa private d",
                es(CoseMap::rs256(&rsa_n(), &[1, 0, 1]).set_bytes(-3, &[1])),
            ),
        ];
        for (name, bytes) in cases {
            let err = PublicKey::from_cose(&decode(&bytes).unwrap()).unwrap_err();
            assert_eq!(err.code, ErrorCode::PasskeyInvalid, "{name}");
        }
        // Not a map at all.
        let err = PublicKey::from_cose(&decode(&[0x80]).unwrap()).unwrap_err();
        assert_eq!(err.code, ErrorCode::PasskeyInvalid);
    }

    #[test]
    fn corrupt_stored_keys_are_internal_errors() {
        for bad in [
            "",
            "es256",
            "es256:",
            "es256:AAAA",
            "ES256:AQ",
            "p256:AQ",
            "ed25519:AQID",
            "ed25519:====",
            "rs256:AQ",
            "rs256:AAEC:AQAB",
            &format!("es256:{}=", "A".repeat(86)),
        ] {
            let err = PublicKey::from_stored(bad).unwrap_err();
            assert_eq!(err.code, ErrorCode::Internal, "{bad:?}");
        }
    }
}
