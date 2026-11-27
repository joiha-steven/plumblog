//! Test builds only (feature `contract-tests` plus the same variable as `/__contract`):
//! `POST /__crypto` runs one `WebCrypto` operation on inputs the test computed with Node, so
//! tools/worker-test/auth.test.mjs can hold `crate::webcrypto` to node:crypto's answers.
//! Body: flat JSON of strings, bytes in base64url. Answers the result as text.

use plumblog_auth::Crypto;
use plumblog_auth::base64url;
use plumblog_auth::json::{Json, parse_object};
use plumblog_auth::webauthn::{Es256Key, Rs256Key};
use worker::{Env, Request, Response, Result};

use crate::webcrypto::WebCrypto;

const PATH: &str = "/__crypto";

fn enabled(env: &Env) -> bool {
    env.var("PLUMBLOG_CONTRACT_TESTS")
        .map(|v| v.to_string())
        .ok()
        .as_deref()
        == Some("wipe-db")
}

pub async fn handle(req: &mut Request, env: &Env) -> Result<Option<Response>> {
    if req.path() != PATH || req.method() != worker::Method::Post || !enabled(env) {
        return Ok(None);
    }
    let body = req.bytes().await?;
    match run(&body).await {
        Ok(out) => Response::ok(out).map(Some),
        Err(e) => Response::error(e, 400).map(Some),
    }
}

fn fixed<const N: usize>(v: Vec<u8>, k: &str) -> std::result::Result<[u8; N], String> {
    v.try_into().map_err(|_| format!("{k}: wrong length"))
}

async fn run(body: &[u8]) -> std::result::Result<String, String> {
    let fields = parse_object(body, 64 * 1024).map_err(|e| e.to_string())?;
    let get = |k: &str| match fields.iter().find(|(n, _)| n == k) {
        Some((_, Json::Str(s))) => Ok(s.as_str()),
        _ => Err(format!("{k} missing")),
    };
    let bytes = |k: &str| base64url::decode(get(k)?).ok_or_else(|| format!("{k}: not base64url"));
    let c = WebCrypto::new().map_err(|e| e.to_string())?;
    let e = |e: plumblog_model::Error| e.to_string();
    match get("op")? {
        "pbkdf2" => {
            let iters: u32 = get("iterations")?
                .parse()
                .map_err(|_| "iterations".to_owned())?;
            let out = c
                .pbkdf2_sha256(&bytes("password")?, &bytes("salt")?, iters)
                .await
                .map_err(e)?;
            Ok(base64url::encode(&out))
        }
        // Runs the server-side hash `n` times, so a client can time n=0 against n=N.
        "pbkdf2_repeat" => {
            let n: u32 = get("n")?.parse().map_err(|_| "n".to_owned())?;
            for _ in 0..n {
                c.pbkdf2_sha256(
                    &[7; 32],
                    &[1; 16],
                    plumblog_auth::password::SERVER_ITERATIONS,
                )
                .await
                .map_err(e)?;
            }
            Ok(n.to_string())
        }
        // The browser stretch done in Rust (salt from `stretch_salt`), to hold
        // web/admin/js/stretch.js to the same vector.
        "stretch" => {
            use plumblog_auth::password::{STRETCH_ITERATIONS, stretch_salt};
            let salt = stretch_salt(get("site_id")?, get("login")?);
            let out = c
                .pbkdf2_sha256(
                    get("password")?.as_bytes(),
                    salt.as_bytes(),
                    STRETCH_ITERATIONS,
                )
                .await
                .map_err(e)?;
            Ok(base64url::encode(&out))
        }
        "sha256" => Ok(base64url::encode(
            &c.sha256(&bytes("data")?).await.map_err(e)?,
        )),
        "random" => {
            let mut b = vec![0u8; 70_000];
            c.random_bytes(&mut b).map_err(e)?;
            Ok(base64url::encode(&b))
        }
        "es256" => {
            let point: [u8; 65] = fixed(bytes("key")?, "key")?;
            let mut x = [0u8; 32];
            let mut y = [0u8; 32];
            x.copy_from_slice(&point[1..33]);
            y.copy_from_slice(&point[33..]);
            let ok = c
                .verify_es256(
                    &Es256Key { x, y },
                    &bytes("msg")?,
                    &fixed(bytes("sig")?, "sig")?,
                )
                .await
                .map_err(e)?;
            Ok(ok.to_string())
        }
        "rs256" => {
            let key = Rs256Key {
                n: bytes("n")?,
                e: bytes("e")?,
            };
            let ok = c
                .verify_rs256(&key, &bytes("msg")?, &bytes("sig")?)
                .await
                .map_err(e)?;
            Ok(ok.to_string())
        }
        "ed25519" => {
            let ok = c
                .verify_ed25519(
                    &fixed(bytes("key")?, "key")?,
                    &bytes("msg")?,
                    &fixed(bytes("sig")?, "sig")?,
                )
                .await
                .map_err(e)?;
            Ok(ok.to_string())
        }
        other => Err(format!("unknown op {other:?}")),
    }
}
