//! Hand-built `WebAuthn` vectors: a tiny CBOR writer, COSE keys, authenticator data, attestation
//! objects and client data, verified against a fake crypto that records the exact message.

use super::{
    Expected, FLAG_AT, FLAG_BE, FLAG_BS, FLAG_ED, FLAG_UP, FLAG_UV, PublicKey,
    parse_authenticator_data, verify_assertion, verify_registration,
};
use crate::base64url;
use crate::testutil::{FakeCrypto, Prng, Verified, block_on, sha256};
use crate::webauthn::{Es256Key, Rs256Key};
use plumblog_model::ErrorCode;

/// Test-side CBOR value (owned), encoded minimally.
#[derive(Clone)]
pub enum C {
    Int(i64),
    Bytes(Vec<u8>),
    Text(String),
    Map(Vec<(C, C)>),
    Bool(bool),
}

fn head(out: &mut Vec<u8>, major: u8, n: u64) {
    let m = major << 5;
    match n {
        0..=23 => out.push(m | u8::try_from(n).unwrap()),
        24..=0xff => out.extend([m | 0x18, u8::try_from(n).unwrap()]),
        0x100..=0xffff => {
            out.push(m | 0x19);
            out.extend(u16::try_from(n).unwrap().to_be_bytes());
        }
        _ => {
            out.push(m | 0x1b);
            out.extend(n.to_be_bytes());
        }
    }
}

impl C {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        self.write(&mut out);
        out
    }

    fn write(&self, out: &mut Vec<u8>) {
        match self {
            Self::Int(i) if *i >= 0 => head(out, 0, i.unsigned_abs()),
            Self::Int(i) => head(out, 1, (-1 - i).unsigned_abs()),
            Self::Bytes(b) => {
                head(out, 2, b.len() as u64);
                out.extend(b);
            }
            Self::Text(t) => {
                head(out, 3, t.len() as u64);
                out.extend(t.as_bytes());
            }
            Self::Map(m) => {
                head(out, 5, m.len() as u64);
                for (k, v) in m {
                    k.write(out);
                    v.write(out);
                }
            }
            Self::Bool(b) => out.push(if *b { 0xf5 } else { 0xf4 }),
        }
    }
}

/// A COSE key under construction, so tests can change one label at a time.
pub struct CoseMap(Vec<(i64, C)>);

impl CoseMap {
    pub fn es256(x: &[u8], y: &[u8]) -> Self {
        Self(vec![
            (1, C::Int(2)),
            (3, C::Int(-7)),
            (-1, C::Int(1)),
            (-2, C::Bytes(x.to_vec())),
            (-3, C::Bytes(y.to_vec())),
        ])
    }

    pub fn rs256(n: &[u8], e: &[u8]) -> Self {
        Self(vec![
            (1, C::Int(3)),
            (3, C::Int(-257)),
            (-1, C::Bytes(n.to_vec())),
            (-2, C::Bytes(e.to_vec())),
        ])
    }

    pub fn ed25519(x: &[u8]) -> Self {
        Self(vec![
            (1, C::Int(1)),
            (3, C::Int(-8)),
            (-1, C::Int(6)),
            (-2, C::Bytes(x.to_vec())),
        ])
    }

    fn put(mut self, label: i64, value: C) -> Self {
        match self.0.iter_mut().find(|(k, _)| *k == label) {
            Some(slot) => slot.1 = value,
            None => self.0.push((label, value)),
        }
        self
    }

    pub fn set(self, label: i64, value: i64) -> Self {
        self.put(label, C::Int(value))
    }

    pub fn set_bool(self, label: i64, value: bool) -> Self {
        self.put(label, C::Bool(value))
    }

    pub fn set_bytes(self, label: i64, value: &[u8]) -> Self {
        self.put(label, C::Bytes(value.to_vec()))
    }

    pub fn remove(mut self, label: i64) -> Self {
        self.0.retain(|(k, _)| *k != label);
        self
    }

    pub fn encode(&self) -> Vec<u8> {
        C::Map(
            self.0
                .iter()
                .map(|(k, v)| (C::Int(*k), v.clone()))
                .collect(),
        )
        .encode()
    }
}

/// Test helper: the DER encoding of a raw `r || s` (minimal, as authenticators produce it).
pub fn raw_to_der(raw: &[u8; 64]) -> Vec<u8> {
    let int = |x: &[u8]| {
        let start = x.iter().position(|b| *b != 0).unwrap_or(31);
        let mut v = x[start..].to_vec();
        if v[0] & 0x80 != 0 {
            v.insert(0, 0);
        }
        let mut out = vec![0x02, u8::try_from(v.len()).unwrap()];
        out.extend(v);
        out
    };
    let body: Vec<u8> = [int(&raw[..32]), int(&raw[32..])].concat();
    let mut out = vec![0x30, u8::try_from(body.len()).unwrap()];
    out.extend(body);
    out
}

pub fn cose_es256(x: &[u8], y: &[u8]) -> Vec<u8> {
    CoseMap::es256(x, y).encode()
}

pub fn cose_rs256(n: &[u8], e: &[u8]) -> Vec<u8> {
    CoseMap::rs256(n, e).encode()
}

pub fn cose_ed25519(x: &[u8]) -> Vec<u8> {
    CoseMap::ed25519(x).encode()
}

const RP_ID: &str = "blog.example";
const ORIGIN: &str = "https://blog.example";
const CHALLENGE: [u8; 32] = [0x5a; 32];
const CRED_ID: [u8; 16] = [0xc1; 16];

fn expected() -> Expected<'static> {
    Expected {
        challenge: &CHALLENGE,
        origin: ORIGIN,
        rp_id: RP_ID,
    }
}

fn client_data(ty: &str) -> Vec<u8> {
    format!(
        r#"{{"type":"{ty}","challenge":"{}","origin":"{ORIGIN}","crossOrigin":false}}"#,
        base64url::encode(&CHALLENGE)
    )
    .into_bytes()
}

/// Authenticator data with the given flags; attested data when `cose_key` is set; `ext` is
/// appended raw (an encoded extensions map when `FLAG_ED` is set).
fn auth_data(flags: u8, count: u32, cose_key: Option<&[u8]>, ext: &[u8]) -> Vec<u8> {
    let mut out = sha256(RP_ID.as_bytes()).to_vec();
    out.push(flags);
    out.extend(count.to_be_bytes());
    if let Some(key) = cose_key {
        out.extend([0u8; 16]);
        out.extend(u16::try_from(CRED_ID.len()).unwrap().to_be_bytes());
        out.extend(CRED_ID);
        out.extend(key);
    }
    out.extend(ext);
    out
}

fn attestation(fmt: &str, att_stmt: Vec<(C, C)>, auth: &[u8]) -> Vec<u8> {
    C::Map(vec![
        (C::Text("fmt".into()), C::Text(fmt.into())),
        (C::Text("attStmt".into()), C::Map(att_stmt)),
        (C::Text("authData".into()), C::Bytes(auth.to_vec())),
    ])
    .encode()
}

fn good_attestation(cose_key: &[u8]) -> Vec<u8> {
    attestation(
        "none",
        vec![],
        &auth_data(FLAG_UP | FLAG_UV | FLAG_AT, 0, Some(cose_key), &[]),
    )
}

fn register(att: &[u8]) -> plumblog_model::Result<super::NewCredential> {
    block_on(verify_registration(
        &FakeCrypto::new(),
        att,
        &client_data("webauthn.create"),
        &expected(),
    ))
}

fn rsa_n() -> Vec<u8> {
    let mut n = vec![0xc5; 256];
    n[255] = 1;
    n
}

#[test]
fn registration_returns_credential_for_each_algorithm() {
    for (key, want) in [
        (
            cose_es256(&[1; 32], &[2; 32]),
            PublicKey::Es256(Es256Key {
                x: [1; 32],
                y: [2; 32],
            }),
        ),
        (
            cose_rs256(&rsa_n(), &[1, 0, 1]),
            PublicKey::Rs256(Rs256Key {
                n: rsa_n(),
                e: vec![1, 0, 1],
            }),
        ),
        (cose_ed25519(&[3; 32]), PublicKey::Ed25519([3; 32])),
    ] {
        let cred = register(&good_attestation(&key)).unwrap();
        assert_eq!(cred.credential_id, base64url::encode(&CRED_ID));
        assert_eq!(cred.public_key, want.to_stored());
        assert_eq!(cred.sign_count, 0);
    }
}

#[test]
fn registration_accepts_backup_flags_extensions_and_extra_top_level_keys() {
    let key = cose_ed25519(&[3; 32]);
    let ext = C::Map(vec![(C::Text("credProtect".into()), C::Int(2))]).encode();
    let auth = auth_data(
        FLAG_UP | FLAG_UV | FLAG_AT | FLAG_BE | FLAG_BS | FLAG_ED,
        7,
        Some(&key),
        &ext,
    );
    assert_eq!(
        register(&attestation("none", vec![], &auth))
            .unwrap()
            .sign_count,
        7
    );
    let extra = C::Map(vec![
        (C::Text("fmt".into()), C::Text("none".into())),
        (C::Text("attStmt".into()), C::Map(vec![])),
        (
            C::Text("authData".into()),
            C::Bytes(auth_data(FLAG_UP | FLAG_UV | FLAG_AT, 0, Some(&key), &[])),
        ),
        (C::Text("epAtt".into()), C::Bool(false)),
    ])
    .encode();
    assert!(register(&extra).is_ok());
}

#[test]
fn registration_refuses_every_failed_check() {
    let key = cose_es256(&[1; 32], &[2; 32]);
    let flags = FLAG_UP | FLAG_UV | FLAG_AT;
    let mut wrong_rp = auth_data(flags, 0, Some(&key), &[]);
    wrong_rp[0] ^= 1;
    let mut long_id = sha256(RP_ID.as_bytes()).to_vec();
    long_id.push(flags);
    long_id.extend([0; 4 + 16]);
    long_id.extend(1024u16.to_be_bytes());
    long_id.extend([7; 1024]);
    long_id.extend(&key);
    let mut zero_id = sha256(RP_ID.as_bytes()).to_vec();
    zero_id.push(flags);
    zero_id.extend([0; 4 + 16 + 2]);
    zero_id.extend(&key);
    let packed_stmt = vec![(C::Text("alg".into()), C::Int(-7))];
    let none = |auth: &[u8]| attestation("none", vec![], auth);
    let ad = |flags: u8, ext: &[u8]| auth_data(flags, 0, Some(&key), ext);
    let es384 = CoseMap::es256(&[1; 32], &[2; 32]).set(3, -35).encode();
    let no_auth_data = C::Map(vec![
        (C::Text("fmt".into()), C::Text("none".into())),
        (C::Text("attStmt".into()), C::Map(vec![])),
    ]);
    let cases: Vec<(&str, Vec<u8>)> = vec![
        ("packed", attestation("packed", vec![], &ad(flags, &[]))),
        (
            "none with stmt",
            attestation("none", packed_stmt, &ad(flags, &[])),
        ),
        ("wrong rp", none(&wrong_rp)),
        ("no UP", none(&ad(FLAG_UV | FLAG_AT, &[]))),
        ("no UV", none(&ad(FLAG_UP | FLAG_AT, &[]))),
        ("no AT", none(&auth_data(FLAG_UP | FLAG_UV, 0, None, &[]))),
        ("BS without BE", none(&ad(flags | FLAG_BS, &[]))),
        ("trailing", none(&ad(flags, &[0]))),
        ("ED but none", none(&ad(flags | FLAG_ED, &[]))),
        ("ED not map", none(&ad(flags | FLAG_ED, &[0x01]))),
        ("ES384 key", none(&auth_data(flags, 0, Some(&es384), &[]))),
        ("id 1024", none(&long_id)),
        ("id 0", none(&zero_id)),
        ("not a map", C::Bytes(vec![1]).encode()),
        ("no authData", no_auth_data.encode()),
        ("too long", vec![0; super::MAX_ATTESTATION_OBJECT_LEN + 1]),
    ];
    for (name, att) in cases {
        let err = register(&att).unwrap_err();
        assert_eq!(
            err.code,
            ErrorCode::PasskeyInvalid,
            "{name}: {}",
            err.detail
        );
    }
}

#[test]
fn registration_refuses_wrong_client_data() {
    let key = cose_es256(&[1; 32], &[2; 32]);
    // Client data for a sign-in cannot register.
    let err = block_on(verify_registration(
        &FakeCrypto::new(),
        &good_attestation(&key),
        &client_data("webauthn.get"),
        &expected(),
    ))
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::PasskeyInvalid);
    // Another site's origin.
    let other = Expected {
        origin: "https://evil.example",
        ..expected()
    };
    assert!(
        block_on(verify_registration(
            &FakeCrypto::new(),
            &good_attestation(&key),
            &client_data("webauthn.create"),
            &other
        ))
        .is_err()
    );
}

fn assert_signed(crypto: &FakeCrypto, auth: &[u8]) -> Vec<u8> {
    let mut message = auth.to_vec();
    message.extend(sha256(&client_data("webauthn.get")));
    let calls = crypto.calls.borrow();
    assert_eq!(calls.len(), 1);
    let got = match &calls[0] {
        Verified::Es256 { message, .. }
        | Verified::Rs256 { message, .. }
        | Verified::Ed25519 { message, .. } => message.clone(),
    };
    assert_eq!(got, message);
    got
}

fn assert_with(
    crypto: &FakeCrypto,
    stored: &str,
    auth: &[u8],
    sig: &[u8],
) -> plumblog_model::Result<u32> {
    block_on(verify_assertion(
        crypto,
        stored,
        auth,
        &client_data("webauthn.get"),
        sig,
        &expected(),
    ))
}

#[test]
fn assertion_verifies_exact_message_for_each_algorithm() {
    let auth = auth_data(FLAG_UP | FLAG_UV, 42, None, &[]);

    let es = PublicKey::Es256(Es256Key {
        x: [1; 32],
        y: [2; 32],
    });
    let mut raw = [0x11u8; 64];
    raw[0] = 0x91; // high bit set: DER needs a 0x00 prefix on r
    let crypto = FakeCrypto::new();
    assert_eq!(
        assert_with(&crypto, &es.to_stored(), &auth, &raw_to_der(&raw)),
        Ok(42)
    );
    assert_signed(&crypto, &auth);
    match &crypto.calls.borrow()[0] {
        Verified::Es256 { key, signature, .. } => {
            assert_eq!(PublicKey::Es256(key.clone()), es);
            assert_eq!(*signature, raw);
        }
        other => panic!("{other:?}"),
    }

    let rs = PublicKey::Rs256(Rs256Key {
        n: rsa_n(),
        e: vec![1, 0, 1],
    });
    let crypto = FakeCrypto::new();
    assert_eq!(
        assert_with(&crypto, &rs.to_stored(), &auth, &[9; 256]),
        Ok(42)
    );
    assert_signed(&crypto, &auth);

    let ed = PublicKey::Ed25519([3; 32]);
    let crypto = FakeCrypto::new();
    assert_eq!(
        assert_with(&crypto, &ed.to_stored(), &auth, &[8; 64]),
        Ok(42)
    );
    assert_signed(&crypto, &auth);
    match &crypto.calls.borrow()[0] {
        Verified::Ed25519 { key, signature, .. } => {
            assert_eq!(*key, [3; 32]);
            assert_eq!(*signature, [8; 64]);
        }
        other => panic!("{other:?}"),
    }

    // Extensions are part of the signed bytes.
    let ext = C::Map(vec![(C::Text("x".into()), C::Bool(true))]).encode();
    let with_ext = auth_data(FLAG_UP | FLAG_UV | FLAG_ED, 43, None, &ext);
    let crypto = FakeCrypto::new();
    assert_eq!(
        assert_with(&crypto, &ed.to_stored(), &with_ext, &[8; 64]),
        Ok(43)
    );
    assert_signed(&crypto, &with_ext);
}

#[test]
fn assertion_refuses_every_failed_check() {
    let ed = PublicKey::Ed25519([3; 32]).to_stored();
    let es = PublicKey::Es256(Es256Key {
        x: [1; 32],
        y: [2; 32],
    })
    .to_stored();
    let rs = PublicKey::Rs256(Rs256Key {
        n: rsa_n(),
        e: vec![1, 0, 1],
    })
    .to_stored();
    let good = auth_data(FLAG_UP | FLAG_UV, 1, None, &[]);
    let mut wrong_rp = good.clone();
    wrong_rp[31] ^= 0x80;
    let attested = auth_data(
        FLAG_UP | FLAG_UV | FLAG_AT,
        1,
        Some(&cose_ed25519(&[3; 32])),
        &[],
    );
    let cases: Vec<(&str, &str, Vec<u8>, Vec<u8>)> = vec![
        ("wrong rp", &ed, wrong_rp, vec![8; 64]),
        ("no UP", &ed, auth_data(FLAG_UV, 1, None, &[]), vec![8; 64]),
        ("no UV", &ed, auth_data(FLAG_UP, 1, None, &[]), vec![8; 64]),
        ("AT in assertion", &ed, attested, vec![8; 64]),
        ("short auth", &ed, good[..36].to_vec(), vec![8; 64]),
        (
            "trailing",
            &ed,
            [good.clone(), vec![0]].concat(),
            vec![8; 64],
        ),
        ("ed sig 63", &ed, good.clone(), vec![8; 63]),
        ("ed sig 65", &ed, good.clone(), vec![8; 65]),
        ("rs sig short", &rs, good.clone(), vec![9; 255]),
        ("rs sig long", &rs, good.clone(), vec![9; 257]),
        ("es raw sig", &es, good.clone(), vec![0x11; 64]),
        (
            "es bad der",
            &es,
            good.clone(),
            vec![0x30, 6, 2, 1, 0, 2, 1, 7],
        ),
        ("sig too long", &es, good.clone(), vec![0x30; 600]),
    ];
    for (name, stored, auth, sig) in cases {
        let crypto = FakeCrypto::new();
        let err = assert_with(&crypto, stored, &auth, &sig).unwrap_err();
        assert_eq!(
            err.code,
            ErrorCode::PasskeyInvalid,
            "{name}: {}",
            err.detail
        );
        assert!(crypto.calls.borrow().is_empty(), "{name}: verified anyway");
    }
}

#[test]
fn assertion_refuses_bad_signature_stored_key_and_client_data() {
    let ed = PublicKey::Ed25519([3; 32]).to_stored();
    let good = auth_data(FLAG_UP | FLAG_UV, 1, None, &[]);
    // A bad signature.
    let crypto = FakeCrypto::new();
    crypto.signature_ok.set(false);
    let err = assert_with(&crypto, &ed, &good, &[8; 64]).unwrap_err();
    assert_eq!(err.code, ErrorCode::PasskeyInvalid);
    // A corrupt stored key is our fault, not the user's.
    let err = assert_with(&FakeCrypto::new(), "ed25519:AQ", &good, &[8; 64]).unwrap_err();
    assert_eq!(err.code, ErrorCode::Internal);
    // Registration client data, wrong challenge, wrong origin.
    for cdj in [
        client_data("webauthn.create"),
        client_data("webauthn.get")
            .iter()
            .map(|b| if *b == b'a' { b'b' } else { *b })
            .collect(),
    ] {
        let err = block_on(verify_assertion(
            &FakeCrypto::new(),
            &ed,
            &good,
            &cdj,
            &[8; 64],
            &expected(),
        ))
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::PasskeyInvalid);
    }
    let other_challenge = Expected {
        challenge: &[0x5b; 32],
        ..expected()
    };
    assert!(
        block_on(verify_assertion(
            &FakeCrypto::new(),
            &ed,
            &good,
            &client_data("webauthn.get"),
            &[8; 64],
            &other_challenge
        ))
        .is_err()
    );
    let other_rp = Expected {
        rp_id: "evil.example",
        ..expected()
    };
    assert!(
        block_on(verify_assertion(
            &FakeCrypto::new(),
            &ed,
            &good,
            &client_data("webauthn.get"),
            &[8; 64],
            &other_rp
        ))
        .is_err()
    );
}

/// Every truncation of real-shaped inputs fails cleanly; random mutations never panic.
#[test]
fn truncations_and_mutations_never_panic() {
    let keys = [
        cose_es256(&[1; 32], &[2; 32]),
        cose_rs256(&rsa_n(), &[1, 0, 1]),
        cose_ed25519(&[3; 32]),
    ];
    let ext = C::Map(vec![(C::Text("credProtect".into()), C::Int(2))]).encode();
    let mut cases = 0;
    let mut rng = Prng(0xfeed);
    for key in &keys {
        let auth = auth_data(FLAG_UP | FLAG_UV | FLAG_AT | FLAG_ED, 5, Some(key), &ext);
        assert!(parse_authenticator_data(&auth).is_ok());
        for cut in 0..auth.len() {
            assert!(parse_authenticator_data(&auth[..cut]).is_err());
            cases += 1;
        }
        let att = attestation("none", vec![], &auth);
        assert!(register(&att).is_ok());
        for cut in 0..att.len() {
            assert!(register(&att[..cut]).is_err());
            cases += 1;
        }
        for _ in 0..2_000 {
            let mut m = att.clone();
            for _ in 0..=rng.below(4) {
                let at = rng.below(m.len());
                m[at] = rng.bytes(1)[0];
            }
            let _ = register(&m);
            let mut a = auth.clone();
            let at = rng.below(a.len());
            a[at] = rng.bytes(1)[0];
            let _ = parse_authenticator_data(&a);
            cases += 2;
        }
    }
    for _ in 0..5_000 {
        let len = rng.below(200);
        let _ = parse_authenticator_data(&rng.bytes(len));
        let _ = register(&rng.bytes(len));
        cases += 2;
    }
    assert!(cases > 20_000, "{cases}");
}

#[test]
fn a_key_the_platform_refuses_is_passkey_invalid_at_registration_and_sign_in() {
    let crypto = FakeCrypto::new();
    crypto.key_ok.set(false);
    let att = good_attestation(&cose_es256(&[7; 32], &[9; 32]));
    let err = block_on(verify_registration(
        &crypto,
        &att,
        &client_data("webauthn.create"),
        &expected(),
    ))
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::PasskeyInvalid, "{err}");

    let stored = PublicKey::Es256(Es256Key {
        x: [7; 32],
        y: [9; 32],
    })
    .to_stored();
    let auth = auth_data(FLAG_UP | FLAG_UV, 1, None, &[]);
    let err = assert_with(&crypto, &stored, &auth, &raw_to_der(&[0x11; 64])).unwrap_err();
    assert_eq!(err.code, ErrorCode::PasskeyInvalid, "{err}");
    assert!(crypto.calls.borrow().is_empty());
}
