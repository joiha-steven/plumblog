//! minisign keys and signatures, legacy algorithm `Ed` only (pure Ed25519 over the whole
//! file), so releases verify with the real tool: `minisign -V -m SHA256SUMS -p minisign.pub`.
//!
//! Formats (minisign 0.12, `src/minisign.c` and the README "Signature format" section):
//! - public key: `untrusted comment: ...`, then base64(`Ed` || key id (8) || public key (32));
//! - signature: `untrusted comment: ...`, base64(`Ed` || key id || signature (64)),
//!   `trusted comment: <text>`, base64(global signature (64)) where the global signature signs
//!   signature || text, so the trusted comment cannot be swapped.
//!
//! The secret is our own compact form for one CI secret: base64(`Ed` || key id || seed (32) ||
//! public key (32)), unencrypted.

use super::reason::Reason;
use ring::signature::{ED25519, Ed25519KeyPair, KeyPair, UnparsedPublicKey};

/// The marker `release/minisign.pub` holds until the owner generates the real key.
pub const PLACEHOLDER: &str = "PLACEHOLDER";
const ALG_LEGACY: &[u8; 2] = b"Ed";
/// minisign's default (`BLAKE2b-512` prehash); not supported: no `BLAKE2b` in the tree.
const ALG_PREHASHED: &[u8; 2] = b"ED";
const UNTRUSTED: &str = "untrusted comment: ";
const TRUSTED: &str = "trusted comment: ";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicKey {
    pub key_id: [u8; 8],
    pub key: [u8; 32],
}

#[derive(Debug, PartialEq, Eq)]
pub enum KeyError {
    /// The file is the placeholder: no release key yet.
    Placeholder,
    Malformed(&'static str),
}

#[derive(Debug, PartialEq, Eq)]
pub enum SigError {
    Malformed(Reason),
    /// A prehashed (`ED`) signature; releases are signed with the legacy algorithm.
    Prehashed,
    /// Signed by another key: hex key ids, minisign's way (`want` is ours).
    KeyMismatch {
        want: String,
        got: String,
    },
    /// The file signature or the trusted comment signature does not verify.
    Invalid,
}

/// Standard base64 with padding, via the strict base64url decoder.
fn b64_decode(s: &str) -> Option<Vec<u8>> {
    let s = s.trim();
    if s.is_empty() || !s.len().is_multiple_of(4) {
        return None;
    }
    let body = s.trim_end_matches('=');
    if s.len() - body.len() > 2 || body.contains(['-', '_']) {
        return None;
    }
    plumblog_auth::base64url::decode(&body.replace('+', "-").replace('/', "_"))
}

fn b64_encode(bytes: &[u8]) -> String {
    crate::setup::base64_std(bytes)
}

/// minisign prints key ids as the little-endian u64 in uppercase hex.
#[must_use]
pub fn key_id_hex(id: &[u8; 8]) -> String {
    format!("{:016X}", u64::from_le_bytes(*id))
}

/// The meaningful lines: comments and blank lines are not part of a public key.
fn payload_line(text: &str) -> Option<&str> {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with(UNTRUSTED))
}

impl PublicKey {
    /// A minisign public key file (or its base64 line alone).
    ///
    /// # Errors
    /// [`KeyError::Placeholder`] for the placeholder file, else `Malformed`.
    pub fn parse(text: &str) -> Result<Self, KeyError> {
        let line = payload_line(text).ok_or(KeyError::Malformed("empty"))?;
        if line == PLACEHOLDER {
            return Err(KeyError::Placeholder);
        }
        let raw = b64_decode(line).ok_or(KeyError::Malformed("not base64"))?;
        if raw.len() != 42 {
            return Err(KeyError::Malformed("wrong length"));
        }
        if &raw[..2] != ALG_LEGACY {
            return Err(KeyError::Malformed("not an Ed25519 key"));
        }
        let mut key_id = [0; 8];
        key_id.copy_from_slice(&raw[2..10]);
        let mut key = [0; 32];
        key.copy_from_slice(&raw[10..]);
        Ok(Self { key_id, key })
    }

    /// The public key file, as `minisign -G` writes it.
    #[must_use]
    pub fn to_file(&self) -> String {
        let mut raw = ALG_LEGACY.to_vec();
        raw.extend_from_slice(&self.key_id);
        raw.extend_from_slice(&self.key);
        format!(
            "{UNTRUSTED}minisign public key {}\n{}\n",
            key_id_hex(&self.key_id),
            b64_encode(&raw)
        )
    }

    /// Verifies a `.minisig` over `data`; returns the trusted comment.
    ///
    /// # Errors
    /// [`SigError`].
    pub fn verify(&self, minisig: &str, data: &[u8]) -> Result<String, SigError> {
        let mut lines = minisig.lines();
        let first = lines.next().ok_or(SigError::Malformed(Reason::SigEmpty))?;
        if !first.starts_with(UNTRUSTED) {
            return Err(SigError::Malformed(Reason::SigNoCommentLine));
        }
        let sig = lines
            .next()
            .and_then(b64_decode)
            .ok_or(SigError::Malformed(Reason::SigNotBase64))?;
        if sig.len() != 74 {
            return Err(SigError::Malformed(Reason::SigWrongLength));
        }
        let comment = lines
            .next()
            .and_then(|l| l.strip_prefix(TRUSTED))
            .ok_or(SigError::Malformed(Reason::SigNoTrustedComment))?;
        let global = lines
            .next()
            .and_then(b64_decode)
            .ok_or(SigError::Malformed(Reason::SigGlobalNotBase64))?;
        if global.len() != 64 {
            return Err(SigError::Malformed(Reason::SigGlobalWrongLength));
        }
        if &sig[..2] == ALG_PREHASHED {
            return Err(SigError::Prehashed);
        }
        if &sig[..2] != ALG_LEGACY {
            return Err(SigError::Malformed(Reason::SigUnknownAlgorithm));
        }
        if sig[2..10] != self.key_id {
            let mut got = [0; 8];
            got.copy_from_slice(&sig[2..10]);
            return Err(SigError::KeyMismatch {
                want: key_id_hex(&self.key_id),
                got: key_id_hex(&got),
            });
        }
        let pk = UnparsedPublicKey::new(&ED25519, &self.key);
        pk.verify(data, &sig[10..]).map_err(|_| SigError::Invalid)?;
        let mut signed = sig[10..].to_vec();
        signed.extend_from_slice(comment.as_bytes());
        pk.verify(&signed, &global).map_err(|_| SigError::Invalid)?;
        Ok(comment.to_string())
    }
}

/// A signing key. Debug never shows the seed.
pub struct SecretKey {
    key_id: [u8; 8],
    pair: Ed25519KeyPair,
    seed: [u8; 32],
}

impl std::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SecretKey({})", key_id_hex(&self.key_id))
    }
}

impl SecretKey {
    /// A new key from the OS random source.
    ///
    /// # Errors
    /// The random source failed.
    pub fn generate() -> Result<Self, String> {
        let mut seed = [0; 32];
        let mut key_id = [0; 8];
        getrandom::getrandom(&mut seed).map_err(|e| e.to_string())?;
        getrandom::getrandom(&mut key_id).map_err(|e| e.to_string())?;
        Self::from_parts(key_id, seed)
    }

    fn from_parts(key_id: [u8; 8], seed: [u8; 32]) -> Result<Self, String> {
        let pair = Ed25519KeyPair::from_seed_unchecked(&seed).map_err(|e| e.to_string())?;
        Ok(Self { key_id, pair, seed })
    }

    /// The secret string (`PLUMBLOG_RELEASE_KEY`).
    ///
    /// # Errors
    /// A fixed reason; the value itself is never echoed.
    pub fn parse(secret: &str) -> Result<Self, &'static str> {
        let raw = b64_decode(secret).ok_or("not base64")?;
        if raw.len() != 74 || &raw[..2] != ALG_LEGACY {
            return Err("not a blog-release key");
        }
        let mut key_id = [0; 8];
        key_id.copy_from_slice(&raw[2..10]);
        let mut seed = [0; 32];
        seed.copy_from_slice(&raw[10..42]);
        let key = Self::from_parts(key_id, seed).map_err(|_| "not an Ed25519 seed")?;
        // The public half is stored too, so a damaged secret is caught here, not at verify.
        if key.pair.public_key().as_ref() != &raw[42..] {
            return Err("its public key does not match its seed");
        }
        Ok(key)
    }

    #[must_use]
    pub fn to_secret(&self) -> String {
        let mut raw = ALG_LEGACY.to_vec();
        raw.extend_from_slice(&self.key_id);
        raw.extend_from_slice(&self.seed);
        raw.extend_from_slice(self.pair.public_key().as_ref());
        b64_encode(&raw)
    }

    #[must_use]
    pub fn public(&self) -> PublicKey {
        let mut key = [0; 32];
        key.copy_from_slice(self.pair.public_key().as_ref());
        PublicKey {
            key_id: self.key_id,
            key,
        }
    }

    /// A legacy (`Ed`) `.minisig` of `data`, as `minisign -S -l -t <comment>` writes it.
    /// `trusted_comment` must be one line.
    #[must_use]
    pub fn sign(&self, data: &[u8], trusted_comment: &str) -> String {
        let comment: String = trusted_comment
            .chars()
            .filter(|c| *c != '\n' && *c != '\r')
            .collect();
        let sig = self.pair.sign(data);
        let mut line = ALG_LEGACY.to_vec();
        line.extend_from_slice(&self.key_id);
        line.extend_from_slice(sig.as_ref());
        let mut signed = sig.as_ref().to_vec();
        signed.extend_from_slice(comment.as_bytes());
        let global = self.pair.sign(&signed);
        format!(
            "{UNTRUSTED}signature from blog-release secret key\n{}\n{TRUSTED}{comment}\n{}\n",
            b64_encode(&line),
            b64_encode(global.as_ref())
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Made with the real minisign 0.12 (Homebrew), `minisign -G -W`, then
    // `minisign -S -l -t "plumblog test vector"` (legacy) and `minisign -S -t prehashed`
    // over SUMS; `minisign -V` verifies the legacy one.
    const PUB: &str = "untrusted comment: minisign public key 9F7E54B071C4E541\n\
        RWRB5cRxsFR+n5KScdS+8hJoqv3apQa/VW2cxRS4nEcw/9x6TFMn38/y\n";
    const SUMS: &[u8] = b"abc123  plumblog-bundle-v0.1.0.tar.gz\n";
    const SIG: &str = "untrusted comment: signature from minisign secret key\n\
        RWRB5cRxsFR+n7OMUDooobNBdY6tpDLDbkrj5x2Ivf87v+ovRLWKAgVj3mwrbSrTiAqDXpp1o3sIG7fV6wuuDQG8tfL87h5U/AU=\n\
        trusted comment: plumblog test vector\n\
        sU5bojq5ZoayM9zmEwGFYh+FXwn23yaJwEXSmFVpflrofHhHC5fwIyRyzN371FUSq/Mu6Kh/PIW05rI++YmMAQ==\n";
    const PREHASHED: &str = "untrusted comment: signature from minisign secret key\n\
        RURB5cRxsFR+n7at2fLQCGEu/u6IJ8EFom0/kuv613fpF6QPhR/szCF4OGANylTLxm023mirrb1t02SzJ4ytLhj28bJRn5NbtA0=\n\
        trusted comment: prehashed\n\
        V5NqMP27kOXR/a0S112wEDh4q1KzLz+9OCW2r+bibDft4eiIetS+NTLI2zzp4GDq+u4E+fqnfp+v3w5tyf+nDQ==\n";

    #[test]
    fn real_minisign_vectors() {
        let pk = PublicKey::parse(PUB).unwrap();
        assert_eq!(key_id_hex(&pk.key_id), "9F7E54B071C4E541");
        assert_eq!(pk.to_file(), PUB.replace("        ", ""));
        assert_eq!(pk.verify(SIG, SUMS).unwrap(), "plumblog test vector");
        assert_eq!(pk.verify(SIG, b"abc124  x\n"), Err(SigError::Invalid));
        assert_eq!(pk.verify(PREHASHED, SUMS), Err(SigError::Prehashed));
        // Another trusted comment under the same global signature.
        let swapped = SIG.replace("plumblog test vector", "plumblog test vectoR");
        assert_eq!(pk.verify(&swapped, SUMS), Err(SigError::Invalid));
    }

    #[test]
    fn placeholder_and_malformed_keys() {
        assert_eq!(
            PublicKey::parse("PLACEHOLDER\n"),
            Err(KeyError::Placeholder)
        );
        assert_eq!(
            PublicKey::parse("untrusted comment: x\nPLACEHOLDER\n"),
            Err(KeyError::Placeholder)
        );
        assert!(matches!(PublicKey::parse(""), Err(KeyError::Malformed(_))));
        assert!(matches!(
            PublicKey::parse("RWRB5cRxsFR+n5KScdS+8hJoqv3apQa/VW2cxRS4nEcw/9x6TFMn38"),
            Err(KeyError::Malformed(_))
        ));
    }

    #[test]
    fn our_signatures_verify_and_bind_the_key_id() {
        let k = SecretKey::generate().unwrap();
        let again = SecretKey::parse(&k.to_secret()).unwrap();
        assert_eq!(again.public(), k.public());
        let sig = k.sign(SUMS, "timestamp:1\tfile:SHA256SUMS");
        let pk = PublicKey::parse(&k.public().to_file()).unwrap();
        assert_eq!(
            pk.verify(&sig, SUMS).unwrap(),
            "timestamp:1\tfile:SHA256SUMS"
        );
        let other = SecretKey::generate().unwrap().public();
        assert!(matches!(
            other.verify(&sig, SUMS),
            Err(SigError::KeyMismatch { .. })
        ));
        // Same key id, another key: the signature itself fails.
        let forged = PublicKey {
            key_id: k.public().key_id,
            key: other.key,
        };
        assert_eq!(forged.verify(&sig, SUMS), Err(SigError::Invalid));
        assert!(SecretKey::parse("x").is_err());
        let mut bad = k.to_secret().into_bytes();
        bad[60] = if bad[60] == b'A' { b'B' } else { b'A' };
        assert!(SecretKey::parse(std::str::from_utf8(&bad).unwrap()).is_err());
    }
}
