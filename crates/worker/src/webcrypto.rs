//! `plumblog_auth::Crypto` over the runtime's `WebCrypto` (`crypto` and `crypto.subtle`).
//! Calls go through `js_sys::Reflect` because the `web-sys` build that `worker` re-exports
//! has no `SubtleCrypto` bindings, and adding them would be a new dependency.
//! Every failure is an `Internal` error naming the operation; nothing here panics.

use plumblog_auth::Crypto;
use plumblog_auth::base64url;
use plumblog_auth::webauthn::{Es256Key, Rs256Key};
use plumblog_model::{Error, ErrorCode, Result};
use worker::js_sys::{Array, Function, Object, Promise, Reflect, Uint8Array};
use worker::wasm_bindgen::{JsCast, JsValue};
use worker::wasm_bindgen_futures::JsFuture;

/// `getRandomValues` refuses more than this many bytes per call (Web Crypto spec).
const RANDOM_CHUNK: usize = 65_536;

pub struct WebCrypto {
    crypto: JsValue,
    subtle: JsValue,
}

fn fail(op: &str, e: &JsValue) -> Error {
    Error::new(ErrorCode::Internal, format!("webcrypto {op}: {e:?}"))
}

fn get(obj: &JsValue, key: &str, op: &str) -> Result<JsValue> {
    let v = Reflect::get(obj, &JsValue::from_str(key)).map_err(|e| fail(op, &e))?;
    if v.is_undefined() || v.is_null() {
        return Err(Error::new(
            ErrorCode::Internal,
            format!("webcrypto {op}: {key} is missing"),
        ));
    }
    Ok(v)
}

/// A plain JS object from string keys.
fn object(fields: &[(&str, JsValue)], op: &str) -> Result<JsValue> {
    let o = Object::new();
    for (k, v) in fields {
        Reflect::set(&o, &JsValue::from_str(k), v).map_err(|e| fail(op, &e))?;
    }
    Ok(o.into())
}

fn bytes(data: &[u8]) -> JsValue {
    Uint8Array::from(data).into()
}

fn usages(usage: &str) -> JsValue {
    Array::of1(&JsValue::from_str(usage)).into()
}

/// Copies an `ArrayBuffer` result into exactly `N` bytes.
fn fixed<const N: usize>(value: &JsValue, op: &str) -> Result<[u8; N]> {
    let arr = Uint8Array::new(value);
    let len = arr.length() as usize;
    if len != N {
        return Err(Error::new(
            ErrorCode::Internal,
            format!("webcrypto {op}: got {len} bytes, want {N}"),
        ));
    }
    let mut out = [0u8; N];
    arr.copy_to(&mut out);
    Ok(out)
}

impl WebCrypto {
    /// # Errors
    ///
    /// `Internal` when the runtime has no `crypto.subtle`.
    pub fn new() -> Result<Self> {
        let global: JsValue = worker::js_sys::global().into();
        let crypto = get(&global, "crypto", "init")?;
        let subtle = get(&crypto, "subtle", "init")?;
        Ok(Self { crypto, subtle })
    }

    /// Calls `crypto.subtle[name](...args)` and awaits the promise.
    async fn subtle(&self, name: &str, args: &[JsValue]) -> Result<JsValue> {
        let f: Function = get(&self.subtle, name, name)?
            .dyn_into()
            .map_err(|e| fail(name, &e))?;
        let args: Array = args.iter().collect();
        let promise: Promise = f
            .apply(&self.subtle, &args)
            .map_err(|e| fail(name, &e))?
            .dyn_into()
            .map_err(|e| fail(name, &e))?;
        JsFuture::from(promise).await.map_err(|e| fail(name, &e))
    }

    async fn import_key(
        &self,
        format: &str,
        key: JsValue,
        algorithm: JsValue,
        usage: &str,
    ) -> Result<JsValue> {
        self.subtle(
            "importKey",
            &[
                JsValue::from_str(format),
                key,
                algorithm,
                JsValue::FALSE,
                usages(usage),
            ],
        )
        .await
    }

    async fn verify(
        &self,
        algorithm: JsValue,
        key: JsValue,
        signature: &[u8],
        message: &[u8],
    ) -> Result<bool> {
        let out = self
            .subtle(
                "verify",
                &[algorithm, key, bytes(signature), bytes(message)],
            )
            .await?;
        out.as_bool().ok_or_else(|| {
            Error::new(
                ErrorCode::Internal,
                "webcrypto verify: result is not a boolean",
            )
        })
    }
}

fn name(n: &str) -> JsValue {
    JsValue::from_str(n)
}

impl Crypto for WebCrypto {
    async fn pbkdf2_sha256(
        &self,
        password: &[u8],
        salt: &[u8],
        iterations: u32,
    ) -> Result<[u8; 32]> {
        let op = "pbkdf2";
        let key = self
            .import_key("raw", bytes(password), name("PBKDF2"), "deriveBits")
            .await?;
        let params = object(
            &[
                ("name", name("PBKDF2")),
                ("hash", name("SHA-256")),
                ("salt", bytes(salt)),
                ("iterations", JsValue::from(iterations)),
            ],
            op,
        )?;
        let bits = self
            .subtle("deriveBits", &[params, key, JsValue::from(256u32)])
            .await?;
        fixed::<32>(&bits, op)
    }

    fn random_bytes(&self, buf: &mut [u8]) -> Result<()> {
        let op = "getRandomValues";
        let f: Function = get(&self.crypto, op, op)?
            .dyn_into()
            .map_err(|e| fail(op, &e))?;
        for chunk in buf.chunks_mut(RANDOM_CHUNK) {
            let len = u32::try_from(chunk.len()).map_err(|_| {
                Error::new(
                    ErrorCode::Internal,
                    "webcrypto getRandomValues: chunk too long",
                )
            })?;
            let arr = Uint8Array::new_with_length(len);
            f.call1(&self.crypto, &arr).map_err(|e| fail(op, &e))?;
            arr.copy_to(chunk);
        }
        Ok(())
    }

    async fn sha256(&self, data: &[u8]) -> Result<[u8; 32]> {
        let out = self
            .subtle("digest", &[name("SHA-256"), bytes(data)])
            .await?;
        fixed::<32>(&out, "digest")
    }

    async fn verify_es256(
        &self,
        key: &Es256Key,
        message: &[u8],
        signature: &[u8; 64],
    ) -> Result<bool> {
        let op = "es256";
        let alg = object(
            &[("name", name("ECDSA")), ("namedCurve", name("P-256"))],
            op,
        )?;
        let k = self
            .import_key("raw", bytes(&key.uncompressed_point()), alg, "verify")
            .await?;
        let params = object(&[("name", name("ECDSA")), ("hash", name("SHA-256"))], op)?;
        self.verify(params, k, signature, message).await
    }

    async fn verify_rs256(&self, key: &Rs256Key, message: &[u8], signature: &[u8]) -> Result<bool> {
        let op = "rs256";
        let jwk = object(
            &[
                ("kty", name("RSA")),
                ("n", JsValue::from_str(&base64url::encode(&key.n))),
                ("e", JsValue::from_str(&base64url::encode(&key.e))),
                ("alg", name("RS256")),
                ("ext", JsValue::TRUE),
            ],
            op,
        )?;
        let alg = object(
            &[
                ("name", name("RSASSA-PKCS1-v1_5")),
                ("hash", name("SHA-256")),
            ],
            op,
        )?;
        let k = self.import_key("jwk", jwk, alg, "verify").await?;
        self.verify(name("RSASSA-PKCS1-v1_5"), k, signature, message)
            .await
    }

    async fn verify_ed25519(
        &self,
        key: &[u8; 32],
        message: &[u8],
        signature: &[u8; 64],
    ) -> Result<bool> {
        let k = self
            .import_key("raw", bytes(key), name("Ed25519"), "verify")
            .await?;
        self.verify(name("Ed25519"), k, signature, message).await
    }
}
