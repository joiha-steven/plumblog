//! Test-only platform: a real SHA-256 (so hashes match published vectors and what browsers
//! compute), a deterministic RNG, a stand-in PBKDF2 and signature checks that record the exact
//! message they were given. Real signatures are tested end to end in the Worker.

use crate::Crypto;
use crate::webauthn::{Es256Key, PublicKey, Rs256Key};
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

pub use crate::sha256::sha256;

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
    /// Whether the platform accepts keys; false stands for an import refused (off the curve).
    pub key_ok: Cell<bool>,
    pub calls: RefCell<Vec<Verified>>,
    pub pbkdf2_calls: RefCell<Vec<Pbkdf2Call>>,
}

impl FakeCrypto {
    /// The import step of every key use, like `WebCrypto`'s `importKey`.
    fn import(&self) -> Result<()> {
        if self.key_ok.get() {
            Ok(())
        } else {
            Err(Error::new(
                ErrorCode::PasskeyInvalid,
                "fake key import refused",
            ))
        }
    }

    pub fn new() -> Self {
        Self {
            rng: RefCell::new(Prng(0x9e37_79b9_7f4a_7c15)),
            rng_fails: Cell::new(false),
            signature_ok: Cell::new(true),
            key_ok: Cell::new(true),
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
        self.import()?;
        self.calls.borrow_mut().push(Verified::Es256 {
            key: key.clone(),
            message: message.to_vec(),
            signature: *signature,
        });
        Ok(self.signature_ok.get())
    }

    async fn verify_rs256(&self, key: &Rs256Key, message: &[u8], signature: &[u8]) -> Result<bool> {
        self.import()?;
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
        self.import()?;
        self.calls.borrow_mut().push(Verified::Ed25519 {
            key: *key,
            message: message.to_vec(),
            signature: *signature,
        });
        Ok(self.signature_ok.get())
    }

    async fn validate_public_key(&self, _key: &PublicKey) -> Result<()> {
        self.import()
    }
}
