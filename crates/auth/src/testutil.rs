//! Test-only platform: a real SHA-256 (so hashes match published vectors and what browsers
//! compute), a deterministic RNG, a stand-in PBKDF2 and signature checks that record the exact
//! message they were given. Real signatures are tested end to end in the Worker.

use crate::Crypto;
use crate::webauthn::{Es256Key, Rs256Key};
use plumblog_model::{Error, ErrorCode, Result};
use std::cell::{Cell, RefCell};
use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

/// Every fake future is ready on first poll.
pub fn block_on<F: Future>(f: F) -> F::Output {
    match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("fake crypto future was not ready"),
    }
}

/// SHA-256 round constants (FIPS 180-4).
const K: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

#[must_use]
pub fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09_e667,
        0xbb67_ae85,
        0x3c6e_f372,
        0xa54f_f53a,
        0x510e_527f,
        0x9b05_688c,
        0x1f83_d9ab,
        0x5be0_cd19,
    ];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in block.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        // Working variables a..h as one array; rotating it shifts them down one place.
        let mut v = h;
        for i in 0..64 {
            let s1 = v[4].rotate_right(6) ^ v[4].rotate_right(11) ^ v[4].rotate_right(25);
            let ch = (v[4] & v[5]) ^ (!v[4] & v[6]);
            let t1 = v[7]
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = v[0].rotate_right(2) ^ v[0].rotate_right(13) ^ v[0].rotate_right(22);
            let maj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
            v.rotate_right(1);
            v[4] = v[4].wrapping_add(t1);
            v[0] = t1.wrapping_add(s0.wrapping_add(maj));
        }
        for (x, y) in h.iter_mut().zip(v) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 32];
    for (i, v) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

/// Deterministic xorshift64*, for reproducible fuzz cases.
pub struct Prng(pub u64);

impl Prng {
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    pub fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next_u64() % (n as u64)).unwrap_or(0)
    }

    pub fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.next_u64().to_le_bytes()[0]).collect()
    }
}

/// Which algorithm a recorded verify call used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verified {
    Es256 {
        key: Es256Key,
        message: Vec<u8>,
        signature: [u8; 64],
    },
    Rs256 {
        key: Rs256Key,
        message: Vec<u8>,
        signature: Vec<u8>,
    },
    Ed25519 {
        key: [u8; 32],
        message: Vec<u8>,
        signature: [u8; 64],
    },
}

/// `(password, salt, iterations)` of one PBKDF2 call.
pub type Pbkdf2Call = (Vec<u8>, Vec<u8>, u32);

pub struct FakeCrypto {
    rng: RefCell<Prng>,
    pub rng_fails: Cell<bool>,
    pub signature_ok: Cell<bool>,
    pub calls: RefCell<Vec<Verified>>,
    pub pbkdf2_calls: RefCell<Vec<Pbkdf2Call>>,
}

impl FakeCrypto {
    pub fn new() -> Self {
        Self {
            rng: RefCell::new(Prng(0x9e37_79b9_7f4a_7c15)),
            rng_fails: Cell::new(false),
            signature_ok: Cell::new(true),
            calls: RefCell::new(Vec::new()),
            pbkdf2_calls: RefCell::new(Vec::new()),
        }
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "stand-in for async WebCrypto"
)]
impl Crypto for FakeCrypto {
    async fn pbkdf2_sha256(
        &self,
        password: &[u8],
        salt: &[u8],
        iterations: u32,
    ) -> Result<[u8; 32]> {
        self.pbkdf2_calls
            .borrow_mut()
            .push((password.to_vec(), salt.to_vec(), iterations));
        // Not PBKDF2: a keyed digest of every input is enough to test the format around it.
        let mut input = Vec::new();
        input.extend_from_slice(&iterations.to_be_bytes());
        input.extend_from_slice(&(salt.len() as u64).to_be_bytes());
        input.extend_from_slice(salt);
        input.extend_from_slice(password);
        Ok(sha256(&input))
    }

    fn random_bytes(&self, buf: &mut [u8]) -> Result<()> {
        if self.rng_fails.get() {
            return Err(Error::new(ErrorCode::Internal, "fake rng failure"));
        }
        let mut rng = self.rng.borrow_mut();
        for b in buf {
            *b = rng.next_u64().to_le_bytes()[0];
        }
        Ok(())
    }

    async fn sha256(&self, data: &[u8]) -> Result<[u8; 32]> {
        Ok(sha256(data))
    }

    async fn verify_es256(
        &self,
        key: &Es256Key,
        message: &[u8],
        signature: &[u8; 64],
    ) -> Result<bool> {
        self.calls.borrow_mut().push(Verified::Es256 {
            key: key.clone(),
            message: message.to_vec(),
            signature: *signature,
        });
        Ok(self.signature_ok.get())
    }

    async fn verify_rs256(&self, key: &Rs256Key, message: &[u8], signature: &[u8]) -> Result<bool> {
        self.calls.borrow_mut().push(Verified::Rs256 {
            key: key.clone(),
            message: message.to_vec(),
            signature: signature.to_vec(),
        });
        Ok(self.signature_ok.get())
    }

    async fn verify_ed25519(
        &self,
        key: &[u8; 32],
        message: &[u8],
        signature: &[u8; 64],
    ) -> Result<bool> {
        self.calls.borrow_mut().push(Verified::Ed25519 {
            key: *key,
            message: message.to_vec(),
            signature: *signature,
        });
        Ok(self.signature_ok.get())
    }
}

#[test]
fn sha256_matches_fips_vectors() {
    use crate::base64url::hex;
    assert_eq!(
        hex(&sha256(b"")),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        hex(&sha256(b"abc")),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        hex(&sha256(
            b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
        )),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
    );
}
