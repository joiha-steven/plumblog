//! `WebhookCrypto` over the runtime's `WebCrypto` (`crypto.subtle`, `crypto.getRandomValues`).
//! Called through `js_sys::Reflect`: the `web_sys` that `worker` builds has no `Crypto`
//! feature, and enabling it would mean a second direct dependency on `web-sys`.

use plumblog_model::{Error, ErrorCode, Result};
use plumblog_webhook::WebhookCrypto;
use worker::js_sys::{self, Array, Function, Object, Promise, Reflect, Uint8Array};
use worker::wasm_bindgen::{JsCast, JsValue};
use worker::wasm_bindgen_futures::JsFuture;

/// The Worker's `WebCrypto`.
pub struct WebCrypto;

fn fail(what: &str, e: &JsValue) -> Error {
    Error::new(
        ErrorCode::Internal,
        format!("webcrypto {what} failed: {e:?}"),
    )
}

fn get(target: &JsValue, name: &str) -> Result<JsValue> {
    let v = Reflect::get(target, &JsValue::from_str(name)).map_err(|e| fail(name, &e))?;
    if v.is_undefined() {
        return Err(fail(name, &JsValue::from_str("missing")));
    }
    Ok(v)
}

fn crypto() -> Result<JsValue> {
    get(&js_sys::global(), "crypto")
}

/// `target.method(...args)`.
fn call(target: &JsValue, method: &str, args: &[JsValue]) -> Result<JsValue> {
    let f: Function = get(target, method)?
        .dyn_into()
        .map_err(|e| fail(method, &e))?;
    let args: Array = args.iter().collect();
    Reflect::apply(&f, target, &args).map_err(|e| fail(method, &e))
}

/// `await crypto.subtle.method(...args)`.
async fn subtle(method: &str, args: &[JsValue]) -> Result<JsValue> {
    let p: Promise = call(&get(&crypto()?, "subtle")?, method, args)?
        .dyn_into()
        .map_err(|e| fail(method, &e))?;
    JsFuture::from(p).await.map_err(|e| fail(method, &e))
}

fn object(pairs: &[(&str, JsValue)]) -> Result<JsValue> {
    let o = Object::new();
    for (k, v) in pairs {
        Reflect::set(&o, &JsValue::from_str(k), v).map_err(|e| fail("object", &e))?;
    }
    Ok(o.into())
}

fn bytes(b: &[u8]) -> JsValue {
    Uint8Array::from(b).into()
}

/// A non-extractable raw key for one use.
async fn import(key: &[u8], algorithm: JsValue, usage: &str) -> Result<JsValue> {
    subtle(
        "importKey",
        &[
            JsValue::from_str("raw"),
            bytes(key),
            algorithm,
            JsValue::FALSE,
            Array::of1(&JsValue::from_str(usage)).into(),
        ],
    )
    .await
}

fn aes_gcm(nonce: Option<&[u8; 12]>) -> Result<JsValue> {
    let name = ("name", JsValue::from_str("AES-GCM"));
    match nonce {
        // 128-bit tag is the default and what `WebhookCrypto` documents.
        Some(n) => object(&[name, ("iv", bytes(n))]),
        None => object(&[name]),
    }
}

impl WebhookCrypto for WebCrypto {
    async fn hmac_sha256(&self, key: &[u8], msg: &[u8]) -> Result<[u8; 32]> {
        let alg = object(&[
            ("name", JsValue::from_str("HMAC")),
            ("hash", JsValue::from_str("SHA-256")),
        ])?;
        let k = import(key, alg, "sign").await?;
        let mac = subtle("sign", &[JsValue::from_str("HMAC"), k, bytes(msg)]).await?;
        <[u8; 32]>::try_from(Uint8Array::new(&mac).to_vec())
            .map_err(|v| fail("sign", &JsValue::from_str(&format!("{} bytes", v.len()))))
    }

    async fn aes_gcm_encrypt(
        &self,
        key: &[u8; 32],
        nonce: &[u8; 12],
        plaintext: &[u8],
    ) -> Result<Vec<u8>> {
        let k = import(key, aes_gcm(None)?, "encrypt").await?;
        let ct = subtle("encrypt", &[aes_gcm(Some(nonce))?, k, bytes(plaintext)]).await?;
        Ok(Uint8Array::new(&ct).to_vec())
    }

    async fn aes_gcm_decrypt(
        &self,
        key: &[u8; 32],
        nonce: &[u8; 12],
        sealed: &[u8],
    ) -> Result<Vec<u8>> {
        let k = import(key, aes_gcm(None)?, "decrypt").await?;
        let pt = subtle("decrypt", &[aes_gcm(Some(nonce))?, k, bytes(sealed)]).await?;
        Ok(Uint8Array::new(&pt).to_vec())
    }

    fn random_bytes(&self, buf: &mut [u8]) -> Result<()> {
        let arr = Uint8Array::new_with_length(
            u32::try_from(buf.len()).map_err(|_| fail("getRandomValues", &"too long".into()))?,
        );
        call(&crypto()?, "getRandomValues", &[arr.clone().into()])?;
        arr.copy_to(buf);
        Ok(())
    }
}
