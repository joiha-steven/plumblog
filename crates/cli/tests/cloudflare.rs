//! The Cloudflare client and token checks against the fake API (no real network).

mod fake_cf;

use fake_cf::{Failure, Fake, Owner, State, Token};
use plumblog_cli::cloudflare::{
    ApiErrorKind, ApiToken, Call, Client, Endpoint, Permission, TokenError, TokenKind, token,
};
use plumblog_cli::text::{Code, Text};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

const GOOD: &str = "tok_GOOD_0123456789abcdefABCDEF";
const D1_EDIT: &str = "Account | D1 | Edit";

fn one_account() -> State {
    let mut st = State::default();
    st.accounts.push(("acc1".into(), "Steven's blog".into()));
    st.tokens.insert(GOOD.into(), Token::user(&[D1_EDIT]));
    st
}

fn en() -> Text {
    Text::new("en")
}

fn client(fake: &Fake, tok: &str) -> Client {
    let mut c = Client::new(ApiToken::new(tok).unwrap(), Some(&fake.base)).unwrap();
    c.set_retry_delay(Duration::ZERO);
    c
}

fn paths(fake: &Fake) -> Vec<String> {
    fake.requests()
        .iter()
        .map(|r| format!("{} {}", r.method, r.path))
        .collect()
}

#[test]
fn good_token_with_one_account() {
    let fake = Fake::start(one_account());
    let mut c = client(&fake, GOOD);
    let v = token::verify(&mut c, None).unwrap();
    assert_eq!(v.account.id, "acc1");
    assert_eq!(v.kind, TokenKind::User);
    assert_eq!(c.account_id(), Some("acc1"));
    assert_eq!(paths(&fake), ["GET /user/tokens/verify", "GET /accounts"]);
    for r in fake.requests() {
        assert_eq!(
            r.header("authorization"),
            Some(format!("Bearer {GOOD}").as_str())
        );
    }
    assert_eq!(fake.requests()[1].query, "per_page=50&page=1");
}

#[test]
fn account_owned_token_verifies_under_its_account() {
    let mut st = one_account();
    st.accounts.push(("acc2".into(), "Other".into()));
    st.tokens.insert(
        "tok_ACCOUNT".into(),
        Token {
            owner: Owner::Account("acc2".into()),
            ..Token::user(&[])
        },
    );
    let fake = Fake::start(st);
    let mut c = client(&fake, "tok_ACCOUNT");
    let v = token::verify(&mut c, None).unwrap();
    assert_eq!(
        (v.account.id.as_str(), v.kind),
        ("acc2", TokenKind::Account)
    );
    assert_eq!(
        paths(&fake),
        [
            "GET /user/tokens/verify",
            "GET /accounts",
            "GET /accounts/acc2/tokens/verify"
        ]
    );
}

#[test]
fn several_accounts_need_an_account_id() {
    let mut st = one_account();
    st.accounts.push(("acc2".into(), "Side project".into()));
    let fake = Fake::start(st);

    let err = token::verify(&mut client(&fake, GOOD), None).unwrap_err();
    assert!(matches!(err, TokenError::SeveralAccounts(ref l) if l.len() == 2));
    let msg = err.message(&en());
    for part in [
        "CLOUDFLARE_ACCOUNT_ID",
        "acc1",
        "Steven's blog",
        "acc2",
        "Side project",
    ] {
        assert!(msg.contains(part), "{part}: {msg}");
    }

    let v = token::verify(&mut client(&fake, GOOD), Some("acc2")).unwrap();
    assert_eq!(v.account.name, "Side project");

    let err = token::verify(&mut client(&fake, GOOD), Some("nope")).unwrap_err();
    assert!(matches!(err, TokenError::AccountNotVisible(ref id, _) if id == "nope"));
}

#[test]
fn invalid_expired_and_disabled_tokens_say_what_to_do() {
    let mut st = one_account();
    for (tok, status) in [("tok_EXPIRED", "expired"), ("tok_DISABLED", "disabled")] {
        st.tokens.insert(
            tok.into(),
            Token {
                status,
                expires_on: Some("2026-01-01T00:00:00Z"),
                ..Token::user(&[D1_EDIT])
            },
        );
    }
    st.tokens.insert(
        "tok_ACC_EXPIRED".into(),
        Token {
            status: "expired",
            owner: Owner::Account("acc1".into()),
            ..Token::user(&[])
        },
    );
    let fake = Fake::start(st);

    let err = token::verify(&mut client(&fake, "tok_UNKNOWN"), None).unwrap_err();
    assert!(matches!(err, TokenError::Invalid), "{err}");
    assert!(err.message(&en()).contains("blog guide token"));

    let err = token::verify(&mut client(&fake, "tok_EXPIRED"), None).unwrap_err();
    assert!(matches!(err, TokenError::Expired(Some(_))), "{err}");
    assert!(err.message(&en()).contains("expired on 2026-01-01"));

    let err = token::verify(&mut client(&fake, "tok_DISABLED"), None).unwrap_err();
    assert!(matches!(err, TokenError::Disabled), "{err}");

    // Account-owned and expired: with the account id known, verify says expired, not invalid.
    let err = token::verify(&mut client(&fake, "tok_ACC_EXPIRED"), Some("acc1")).unwrap_err();
    assert!(matches!(err, TokenError::Expired(_)), "{err}");
}

#[test]
fn missing_permission_names_the_dashboard_label() {
    let mut st = one_account();
    st.tokens.insert("tok_NO_D1".into(), Token::user(&[]));
    let fake = Fake::start(st);
    let mut c = client(&fake, "tok_NO_D1");
    token::verify(&mut c, None).unwrap();
    let err = c.call(&Call::new(Endpoint::D1List)).unwrap_err();
    assert_eq!(
        err.kind,
        ApiErrorKind::MissingPermission(Some(Permission::D1Edit))
    );
    assert_eq!(err.status, Some(403));
    assert!(err.has_code(10000));
    let msg = err.message(&en());
    assert!(msg.contains("\"Account | D1 | Edit\""), "{msg}");
    assert!(
        msg.contains("dash.cloudflare.com/profile/api-tokens"),
        "{msg}"
    );

    // With the permission the same call works, and the write records its body.
    let mut c = client(&fake, GOOD);
    c.set_account("acc1").unwrap();
    let db = c
        .call(&Call::new(Endpoint::D1Create).json(json!({"name": "blog"})))
        .unwrap();
    assert_eq!(db["name"], "blog");
    let last = fake.requests().pop().unwrap();
    assert_eq!(last.header("content-type"), Some("application/json"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&last.body).unwrap()["name"],
        "blog"
    );
    // The fake checks shapes: a create without a name is refused.
    let err = c
        .call(&Call::new(Endpoint::D1Create).json(json!({})))
        .unwrap_err();
    assert_eq!((err.kind, err.status), (ApiErrorKind::Http, Some(400)));
}

#[test]
fn gets_retry_on_5xx_and_network_errors_writes_never() {
    let fake = Fake::start(one_account());
    let mut c = client(&fake, GOOD);
    c.set_account("acc1").unwrap();

    // GET: two failures (5xx, dropped connection), then success on the third try.
    fake.fail(1, Failure::Status(503));
    fake.fail(2, Failure::Drop);
    c.call(&Call::new(Endpoint::D1List)).unwrap();
    assert_eq!(fake.requests().len(), 3);

    // GET: three failures give up after exactly three tries.
    for n in 4..=6 {
        fake.fail(n, Failure::Status(502));
    }
    let err = c.call(&Call::new(Endpoint::D1List)).unwrap_err();
    assert_eq!(
        (err.kind, err.status, err.tries),
        (ApiErrorKind::Http, Some(502), 3)
    );
    assert_eq!(fake.requests().len(), 6);

    // Writes: one try on a 5xx and on a dropped connection.
    fake.fail(7, Failure::Status(500));
    let err = c
        .call(&Call::new(Endpoint::D1Create).json(json!({"name": "a"})))
        .unwrap_err();
    assert_eq!((err.kind, err.tries), (ApiErrorKind::Http, 1));
    fake.fail(8, Failure::Drop);
    let err = c
        .call(&Call::new(Endpoint::D1Create).json(json!({"name": "a"})))
        .unwrap_err();
    assert_eq!((err.kind, err.tries), (ApiErrorKind::Network, 1));
    assert_eq!(fake.requests().len(), 8);
    assert!(fake.state().d1.is_empty(), "no write may have been applied");
}

#[test]
fn malformed_reply_is_an_error_not_a_panic() {
    let fake = Fake::start(one_account());
    fake.fail(1, Failure::Malformed);
    let err = client(&fake, GOOD)
        .call(&Call::new(Endpoint::UserTokenVerify))
        .unwrap_err();
    assert_eq!(err.kind, ApiErrorKind::Malformed);
    assert!(err.message(&en()).contains("not JSON"), "{err}");
}

#[test]
fn calls_are_refused_before_sending_when_misused() {
    let fake = Fake::start(one_account());
    let c = client(&fake, GOOD);
    // No account chosen yet.
    assert_eq!(
        c.call(&Call::new(Endpoint::D1List)).unwrap_err().kind,
        ApiErrorKind::Usage
    );
    // The assets upload takes the session JWT, never the API token.
    let mut c = client(&fake, GOOD);
    c.set_account("acc1").unwrap();
    assert_eq!(
        c.call(&Call::new(Endpoint::AssetsUpload)).unwrap_err().kind,
        ApiErrorKind::Usage
    );
    assert!(fake.requests().is_empty());
}

#[test]
fn bad_api_base_is_refused() {
    for bad in [
        "https://api.example.com",
        "http://127.0.0.1:1@evil.example",
        "http://10.0.0.1:80",
    ] {
        let err = Client::new(ApiToken::new(GOOD).unwrap(), Some(bad)).unwrap_err();
        assert_eq!(err.kind, ApiErrorKind::BaseRefused, "{bad}");
        assert!(err.message(&en()).contains("PLUMBLOG_CF_API_BASE"));
    }
    let dir = TempDir::new();
    dir.write_env(GOOD);
    let out = blog(&dir, "https://api.example.com");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("PLUMBLOG_CF_API_BASE"), "{stderr}");
    assert_no_token(&out, &dir, GOOD);
}

// --- the real `blog` binary ---

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static N: AtomicUsize = AtomicUsize::new(0);
        let p = std::env::temp_dir().join(format!(
            "plumblog-cli-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }

    fn write_env(&self, tok: &str) {
        std::fs::write(self.0.join(".env"), format!("CLOUDFLARE_API_TOKEN={tok}\n")).unwrap();
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn blog(dir: &TempDir, base: &str) -> Output {
    blog_lang(dir, base, "en")
}

fn blog_lang(dir: &TempDir, base: &str, lang: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_blog"))
        .args(["--lang", lang, "setup", "--check-token"])
        .current_dir(&dir.0)
        .env_remove("CLOUDFLARE_API_TOKEN")
        .env_remove("CLOUDFLARE_ACCOUNT_ID")
        .env("PLUMBLOG_CF_API_BASE", base)
        .output()
        .unwrap()
}

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            files_under(&p, out);
        } else {
            out.push(p);
        }
    }
}

/// The token is in no output and in no file but the `.env` the test wrote.
fn assert_no_token(out: &Output, dir: &TempDir, tok: &str) {
    for (name, text) in [("stdout", &out.stdout), ("stderr", &out.stderr)] {
        let text = String::from_utf8_lossy(text);
        assert!(!text.contains(tok), "token leaked in {name}: {text}");
    }
    let mut files = Vec::new();
    files_under(&dir.0, &mut files);
    for f in files {
        if f == dir.0.join(".env") {
            continue;
        }
        let bytes = std::fs::read(&f).unwrap();
        assert!(
            !String::from_utf8_lossy(&bytes).contains(tok),
            "token leaked in {}",
            f.display()
        );
    }
}

#[test]
fn blog_binary_never_shows_the_token() {
    let fake = Fake::start(one_account());

    // Success.
    let dir = TempDir::new();
    dir.write_env(GOOD);
    let out = blog(&dir, &fake.base);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.contains("token is valid") && stdout.contains("acc1"),
        "{stdout}"
    );
    assert_no_token(&out, &dir, GOOD);

    // Cloudflare echoes the Authorization header in an error: scrubbed.
    let n = fake.requests().len();
    fake.fail(n + 1, Failure::EchoToken);
    let out = blog(&dir, &fake.base);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(stderr.contains("Bearer [hidden]"), "{stderr}");
    assert_no_token(&out, &dir, GOOD);

    // A token Cloudflare does not know.
    let bad = "tok_NOT_KNOWN_987654321";
    let dir = TempDir::new();
    dir.write_env(bad);
    let out = blog(&dir, &fake.base);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(
        stderr.contains("does not accept this API token"),
        "{stderr}"
    );
    assert_no_token(&out, &dir, bad);

    // No token at all.
    let dir = TempDir::new();
    let out = blog(&dir, &fake.base);
    assert!(String::from_utf8_lossy(&out.stderr).contains("CLOUDFLARE_API_TOKEN"));
}

#[test]
fn binary_speaks_the_chosen_language_and_prints_the_code() {
    let fake = Fake::start(one_account());
    let dir = TempDir::new();
    let bad = "tok_NOT_KNOWN_555";
    dir.write_env(bad);
    let en_out = blog_lang(&dir, &fake.base, "en");
    let vi_out = blog_lang(&dir, &fake.base, "vi");
    let en_err = String::from_utf8_lossy(&en_out.stderr);
    let vi_err = String::from_utf8_lossy(&vi_out.stderr);
    assert!(
        en_err.starts_with("error [cf.token_invalid]: Cloudflare does not accept"),
        "{en_err}"
    );
    let line = |lang: &str| {
        let text = Text::new(lang);
        text.error_line(Code::TokenInvalid, &TokenError::Invalid.message(&text))
    };
    assert_eq!(en_err.trim_end(), line("en"));
    assert_eq!(vi_err.trim_end(), line("vi"));
    assert_ne!(line("vi"), line("en"));
    // PLUMBLOG_LANG and the OS locale pick the language when --lang is absent.
    let out = Command::new(env!("CARGO_BIN_EXE_blog"))
        .args(["setup", "--check-token"])
        .current_dir(&dir.0)
        .env_remove("CLOUDFLARE_API_TOKEN")
        .env_remove("CLOUDFLARE_ACCOUNT_ID")
        .env_remove("PLUMBLOG_LANG")
        .env_remove("LC_ALL")
        .env_remove("LC_MESSAGES")
        .env("LANG", "vi_VN.UTF-8")
        .env("PLUMBLOG_CF_API_BASE", &fake.base)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stderr).trim_end(), line("vi"));
    for out in [&en_out, &vi_out, &out] {
        assert_no_token(out, &dir, bad);
    }
}

/// A token in a non-JSON reply is scrubbed before the excerpt is cut, so no part of it
/// survives when it straddles the cut.
#[test]
fn token_straddling_the_excerpt_cut_does_not_leak() {
    let fake = Fake::start(one_account());
    for n in 1..=3 {
        fake.fail(n, Failure::HtmlWithToken(180));
    }
    let err = client(&fake, GOOD)
        .call(&Call::new(Endpoint::UserTokenVerify))
        .unwrap_err();
    assert_eq!((err.kind, err.status), (ApiErrorKind::Http, Some(502)));
    let shown = format!("{} {} {err:?}", err.message(&en()), err);
    // Any 6-character piece of the token would show a partial leak.
    for w in GOOD.as_bytes().windows(6) {
        let piece = std::str::from_utf8(w).unwrap();
        assert!(!shown.contains(piece), "{piece} in {shown}");
    }
    assert!(shown.contains("[hidden]"), "{shown}");
}

#[test]
fn secrets_and_upload_jwt_are_hidden_in_errors() {
    let fake = Fake::start(one_account());
    let mut c = client(&fake, GOOD);
    c.set_account("acc1").unwrap();
    let jwt = "jwt.upload-session-value";
    fake.fail(1, Failure::EchoToken);
    let err = c
        .call(&Call::new(Endpoint::AssetsUpload).upload_jwt(ApiToken::new(jwt).unwrap()))
        .unwrap_err();
    assert_eq!(
        fake.requests()[0].header("authorization"),
        Some(format!("Bearer {jwt}").as_str())
    );
    let shown = format!("{} {err} {err:?}", err.message(&en()));
    assert!(
        !shown.contains(jwt) && shown.contains("Bearer [hidden]"),
        "{shown}"
    );

    let secret = "s3cr3t-webhook-key";
    fake.fail(2, Failure::EchoBody);
    let call = Call::new(Endpoint::SecretPut)
        .param("script_name", "blog")
        .json(json!({"name": "WEBHOOK_KEY", "text": secret, "type": "secret_text"}))
        .secret(ApiToken::new(secret).unwrap());
    assert!(!format!("{call:?}").contains(secret));
    let err = c.call(&call).unwrap_err();
    let shown = format!("{} {err} {err:?}", err.message(&en()));
    assert!(
        fake.requests()[1].body.contains(secret),
        "the secret was sent"
    );
    assert!(!shown.contains(secret), "{shown}");
}

#[test]
fn account_id_holding_the_token_is_never_echoed() {
    // Alphanumeric token, so the id passes the shape check and reaches Cloudflare.
    let tok = "abcDEF0123456789abcDEF0123456789zz";
    let mut st = one_account();
    st.tokens.insert(tok.into(), Token::user(&[]));
    let fake = Fake::start(st);
    let id = format!("{tok}9");
    let err = token::verify(&mut client(&fake, tok), Some(&id)).unwrap_err();
    assert!(matches!(err, TokenError::AccountNotVisible(..)), "{err:?}");
    for lang in ["en", "vi"] {
        let shown = format!("{} {err} {err:?}", err.message(&Text::new(lang)));
        assert!(!shown.contains(tok), "{shown}");
    }
}

#[test]
fn lists_follow_every_page_and_cursor() {
    let mut st = one_account();
    for i in 2..=120 {
        st.accounts
            .push((format!("acc{i}"), format!("Account {i}")));
    }
    st.r2 = (1..=45).map(|i| format!("bucket-{i}")).collect();
    st.tokens.insert(
        "tok_R2".into(),
        Token::user(&["Account | Workers R2 Storage | Edit"]),
    );
    let fake = Fake::start(st);

    // The account on page 3 of /accounts is found.
    let v = token::verify(&mut client(&fake, GOOD), Some("acc120")).unwrap();
    assert_eq!(v.account.name, "Account 120");
    let pages: Vec<String> = fake
        .requests()
        .iter()
        .filter(|r| r.path == "/accounts")
        .map(|r| r.query.clone())
        .collect();
    assert_eq!(
        pages,
        [
            "per_page=50&page=1",
            "per_page=50&page=2",
            "per_page=50&page=3"
        ]
    );

    // R2 pages by cursor and nests the list under `buckets`.
    let mut c = client(&fake, "tok_R2");
    c.set_account("acc1").unwrap();
    let all = c
        .call_all(&Call::new(Endpoint::R2BucketList), 20, Some("buckets"))
        .unwrap();
    assert_eq!(all.len(), 45);
    assert_eq!(all[44]["name"], "bucket-45");
    let (_, info) = c
        .call_page(&Call::new(Endpoint::R2BucketList).query("per_page", "20"))
        .unwrap();
    assert_eq!(info.unwrap().cursor.as_deref(), Some("20"));
    // Writes cannot be paged.
    let err = c
        .call_all(&Call::new(Endpoint::D1Create), 20, None)
        .unwrap_err();
    assert_eq!(err.kind, ApiErrorKind::Usage);
}

#[test]
fn network_failure_on_a_write_says_it_may_have_happened() {
    let fake = Fake::start(one_account());
    let mut c = client(&fake, GOOD);
    c.set_account("acc1").unwrap();
    fake.fail(1, Failure::Drop);
    let err = c
        .call(&Call::new(Endpoint::D1Create).json(json!({"name": "a"})))
        .unwrap_err();
    assert_eq!(err.code(), Code::NetworkWriteUncertain);
    let msg = err.message(&en());
    assert!(
        msg.contains("may already have been made") && !msg.contains("try again"),
        "{msg}"
    );
    fake.fail(2, Failure::Drop);
    fake.fail(3, Failure::Drop);
    fake.fail(4, Failure::Drop);
    let err = c.call(&Call::new(Endpoint::D1List)).unwrap_err();
    assert_eq!((err.code(), err.tries), (Code::Network, 3));
    assert!(err.message(&en()).contains("after 3 tries"));
}

#[test]
fn bad_arguments_exit_2_with_the_code_in_both_languages() {
    for lang in ["en", "vi"] {
        for args in [&["--bogus"][..], &["guide", "--lang"][..]] {
            let out = Command::new(env!("CARGO_BIN_EXE_blog"))
                .args(["--lang", lang])
                .args(args)
                .output()
                .unwrap();
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert_eq!(out.status.code(), Some(2), "{lang} {args:?}: {stderr}");
            assert!(
                stderr.contains("[cli.args_invalid]") && stderr.contains(args[args.len() - 1]),
                "{stderr}"
            );
            assert!(
                !stderr.contains("unexpected argument") && !stderr.contains("Usage:"),
                "{stderr}"
            );
        }
    }
}
