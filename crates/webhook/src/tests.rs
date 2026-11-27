//! Fakes and flow tests. The fake crypto is deterministic and NOT the real algorithms: it
//! proves what is signed, sealed and refused; the Worker tests check real HMAC-SHA256 and
//! AES-GCM against node:crypto.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use plumblog_model::{Error, ErrorCode, PostId, RequestId, Result};
use plumblog_store::memory::MemoryStore;
use plumblog_store::{
    AttemptResult, Delivery, DeliveryStore, LogQuery, LogStore, NewDelivery, NewWebhook,
    PostStatus, Webhook, WebhookStore,
};
use plumblog_syslog::{Event, EventKind, LogSink, Scope, Severity};

use crate::{
    Created, HttpError, LEASE_MS, Policy, PostInfo, Run, Secret, Sender, WebhookCrypto,
    WebhookEvent, WebhookHttp, b64, create_webhook, enqueue, prune_job, rotate_secret,
};

/// The futures here never wait, so one poll runs them to completion.
pub fn run<F: Future>(f: F) -> F::Output {
    match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
        Poll::Ready(v) => v,
        Poll::Pending => panic!("future was pending"),
    }
}

/// FNV-1a over `parts`, stretched to 32 bytes. Deterministic stand-in, not a MAC.
pub fn mix(parts: &[&[u8]]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, chunk) in out.chunks_mut(8).enumerate() {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325 ^ (i as u64);
        for p in parts {
            for &b in *p {
                h ^= u64::from(b);
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
            h ^= 0xff;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        chunk.copy_from_slice(&h.to_be_bytes());
    }
    out
}

#[derive(Default)]
pub struct FakeCrypto {
    counter: Cell<u8>,
    pub broken_hmac: Cell<bool>,
}

fn keystream(key: &[u8; 32], nonce: &[u8; 12], len: usize) -> Vec<u8> {
    (0..len.div_ceil(32))
        .flat_map(|i| mix(&[b"ks", key, nonce, &i.to_be_bytes()]))
        .take(len)
        .collect()
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "stands in for async WebCrypto"
)]
impl WebhookCrypto for FakeCrypto {
    async fn hmac_sha256(&self, key: &[u8], msg: &[u8]) -> Result<[u8; 32]> {
        if self.broken_hmac.get() {
            return Err(Error::new(ErrorCode::Internal, "hmac failed"));
        }
        Ok(mix(&[b"hmac", key, msg]))
    }

    async fn aes_gcm_encrypt(
        &self,
        key: &[u8; 32],
        nonce: &[u8; 12],
        plaintext: &[u8],
    ) -> Result<Vec<u8>> {
        let mut ct: Vec<u8> = plaintext
            .iter()
            .zip(keystream(key, nonce, plaintext.len()))
            .map(|(a, b)| a ^ b)
            .collect();
        let tag = mix(&[b"tag", key, nonce, &ct]);
        ct.extend_from_slice(&tag[..16]);
        Ok(ct)
    }

    async fn aes_gcm_decrypt(
        &self,
        key: &[u8; 32],
        nonce: &[u8; 12],
        sealed: &[u8],
    ) -> Result<Vec<u8>> {
        let split = sealed
            .len()
            .checked_sub(16)
            .ok_or_else(|| Error::new(ErrorCode::Internal, "too short"))?;
        let (ct, tag) = sealed.split_at(split);
        if mix(&[b"tag", key, nonce, ct])[..16] != *tag {
            return Err(Error::new(ErrorCode::Internal, "tag mismatch"));
        }
        Ok(ct
            .iter()
            .zip(keystream(key, nonce, ct.len()))
            .map(|(a, b)| a ^ b)
            .collect())
    }

    fn random_bytes(&self, buf: &mut [u8]) -> Result<()> {
        for b in buf {
            self.counter.set(self.counter.get().wrapping_add(1));
            *b = self.counter.get();
        }
        Ok(())
    }
}

type Sent = (String, Vec<(&'static str, String)>, String);

/// Answers from a script (default 200) and records what was sent.
#[derive(Default)]
struct FakeHttp {
    answers: RefCell<VecDeque<std::result::Result<u16, HttpError>>>,
    sent: RefCell<Vec<Sent>>,
}

impl FakeHttp {
    fn answer(&self, a: std::result::Result<u16, HttpError>) {
        self.answers.borrow_mut().push_back(a);
    }
}

#[expect(clippy::unused_async_trait_impl, reason = "stands in for async fetch")]
impl WebhookHttp for FakeHttp {
    async fn post(
        &self,
        url: &str,
        headers: &[(&'static str, String)],
        body: &str,
    ) -> std::result::Result<u16, HttpError> {
        self.sent
            .borrow_mut()
            .push((url.to_owned(), headers.to_vec(), body.to_owned()));
        self.answers.borrow_mut().pop_front().unwrap_or(Ok(200))
    }
}

const T: i64 = 1_764_230_400_000;
const URL: &str = "https://hooks.example.com/in/PATHSECRET?token=abc";

fn key() -> String {
    b64::encode(&[7u8; 32])
}

fn post() -> PostInfo {
    PostInfo {
        post_id: PostId(3),
        slug: "hello".into(),
        lang: "en".into(),
        title: "Hello".into(),
        url: "https://blog.example/en/hello".into(),
        status: PostStatus::Published,
        published_at: Some(T),
        updated_at: T,
        previous_url: None,
    }
}

fn scope() -> Scope {
    Scope::new(RequestId::parse("cron-1").unwrap(), None)
}

fn webhooks(db: &MemoryStore) -> Vec<Webhook> {
    run(db.list_webhooks()).unwrap()
}

fn deliveries(db: &MemoryStore, webhook: i64) -> Vec<Delivery> {
    run(db.list_deliveries(webhook, 100)).unwrap()
}

fn log(db: &MemoryStore, kind: EventKind) -> Vec<Event> {
    let mut v: Vec<Event> = run(db.query_log(LogQuery {
        kind: Some(kind),
        actor: None,
        from_ms: None,
        to_ms: None,
        before: None,
        limit: 100,
    }))
    .unwrap()
    .into_iter()
    .map(|e| e.event)
    .collect();
    v.reverse();
    v
}

fn field<'a>(e: &'a Event, k: &str) -> Option<&'a str> {
    e.fields()
        .iter()
        .find(|(n, _)| n == k)
        .map(|(_, v)| v.as_str())
}

fn create(db: &MemoryStore, c: &FakeCrypto, events: &[WebhookEvent]) -> Created {
    run(create_webhook(
        db,
        c,
        Some(&key()),
        URL,
        events,
        Policy::Production,
        T,
    ))
    .unwrap()
}

fn deliver(db: &MemoryStore, c: &FakeCrypto, http: &FakeHttp, at: i64, key: Option<&str>) -> Run {
    let sender = Sender {
        store: db,
        crypto: c,
        http,
        raw_key: key,
        policy: Policy::Production,
    };
    run(sender.deliver_due(
        &scope(),
        || at,
        crate::MAX_PER_RUN,
        |m| panic!("unexpected report: {m}"),
    ))
    .unwrap()
}

#[test]
fn create_stores_a_sealed_secret_and_returns_it_once() {
    let (db, cr) = (MemoryStore::default(), FakeCrypto::default());
    let made = create(
        &db,
        &cr,
        &[WebhookEvent::PostDeleted, WebhookEvent::PostPublished],
    );
    assert!(made.secret.starts_with("whsec_"), "{}", made.secret);
    let [w] = webhooks(&db).try_into().unwrap();
    assert_eq!(w.id, made.id);
    assert_eq!(w.events, "post.published,post.deleted");
    assert!(w.enabled);
    assert!(w.secret.starts_with("v1:"), "{}", w.secret);
    let secret = Secret::parse_display(&made.secret).unwrap();
    assert!(!w.secret.contains(&b64::encode(secret.bytes())));
    let key = crate::SealKey::parse(Some(&key())).unwrap();
    assert_eq!(run(crate::open(&cr, &key, &w.secret)).unwrap(), secret);
}

#[test]
fn create_refuses_and_saves_nothing() {
    let (db, cr) = (MemoryStore::default(), FakeCrypto::default());
    let all = [WebhookEvent::PostPublished];
    let (good, short) = (key(), b64::encode(&[7u8; 16]));
    for (raw, url, events, code) in [
        (None, URL, &all[..], ErrorCode::WebhookKeyInvalid),
        (Some(""), URL, &all[..], ErrorCode::WebhookKeyInvalid),
        (
            Some("not base64"),
            URL,
            &all[..],
            ErrorCode::WebhookKeyInvalid,
        ),
        (
            Some(short.as_str()),
            URL,
            &all[..],
            ErrorCode::WebhookKeyInvalid,
        ),
        (
            Some(good.as_str()),
            "http://example.com/",
            &all[..],
            ErrorCode::WebhookUrlBlocked,
        ),
        (
            Some(good.as_str()),
            "https://169.254.169.254/",
            &all[..],
            ErrorCode::WebhookUrlBlocked,
        ),
        (
            Some(good.as_str()),
            "https://localhost./",
            &all[..],
            ErrorCode::WebhookUrlBlocked,
        ),
        (Some(good.as_str()), URL, &[][..], ErrorCode::InvalidInput),
    ] {
        let e = run(create_webhook(
            &db,
            &cr,
            raw,
            url,
            events,
            Policy::Production,
            T,
        ))
        .unwrap_err();
        assert_eq!(e.code, code, "{raw:?} {url}");
    }
    assert_eq!(webhooks(&db), []);
}

#[test]
fn unicode_host_is_refused_asking_for_punycode_and_punycode_is_saved() {
    let (db, cr) = (MemoryStore::default(), FakeCrypto::default());
    let all = [WebhookEvent::PostPublished];
    let key = key();
    let make = |url: &str| {
        run(create_webhook(
            &db,
            &cr,
            Some(&key),
            url,
            &all,
            Policy::Production,
            T,
        ))
    };
    let e = make("https://b\u{fc}cher.example/hook").unwrap_err();
    assert_eq!(e.code, ErrorCode::WebhookUrlBlocked);
    assert!(e.detail.contains("xn--"), "unclear error: {}", e.detail);
    make("https://xn--bcher-kva.example/hook").unwrap();
    assert_eq!(webhooks(&db).len(), 1);
}

#[test]
fn rotate_replaces_the_secret() {
    let (db, cr) = (MemoryStore::default(), FakeCrypto::default());
    let made = create(&db, &cr, &[WebhookEvent::PostPublished]);
    let before = webhooks(&db)[0].clone();
    let again = run(rotate_secret(&db, &cr, Some(&key()), made.id)).unwrap();
    assert_ne!(again, made.secret);
    let after = webhooks(&db)[0].clone();
    assert_ne!(after.secret, before.secret);
    assert_eq!((after.url, after.events), (before.url, before.events));
    let e = run(rotate_secret(&db, &cr, None, made.id)).unwrap_err();
    assert_eq!(e.code, ErrorCode::WebhookKeyInvalid);
    let e = run(rotate_secret(&db, &cr, Some(&key()), made.id + 9)).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound);
}

#[test]
fn enqueue_only_for_enabled_subscribers() {
    let (db, cr) = (MemoryStore::default(), FakeCrypto::default());
    let a = create(&db, &cr, &[WebhookEvent::PostPublished]).id;
    let b = create(&db, &cr, &[WebhookEvent::PostDeleted]).id;
    let off = create(&db, &cr, &[WebhookEvent::PostPublished]).id;
    let mut w = webhooks(&db).into_iter().find(|w| w.id == off).unwrap();
    w.enabled = false;
    run(db.update_webhook(&w)).unwrap();

    let none = run(enqueue(&db, &cr, WebhookEvent::PostUpdated, &post(), T)).unwrap();
    assert_eq!(none, Vec::<i64>::new());
    let ids = run(enqueue(&db, &cr, WebhookEvent::PostPublished, &post(), T)).unwrap();
    assert_eq!(ids.len(), 1);
    let [d] = deliveries(&db, a).try_into().unwrap();
    assert!(
        d.msg_id.starts_with("msg_") && d.msg_id.len() == 36,
        "{}",
        d.msg_id
    );
    assert_eq!(d.event, "post.published");
    assert_eq!(d.next_at_ms, Some(T));
    assert!(d.payload.contains(&format!("\"id\":\"{}\"", d.msg_id)));
    assert!(!d.payload.contains("markdown"));
    assert_eq!(deliveries(&db, b), []);
    assert_eq!(deliveries(&db, off), []);

    run(enqueue(&db, &cr, WebhookEvent::PostDeleted, &post(), T)).unwrap();
    let [gone] = deliveries(&db, b).try_into().unwrap();
    assert!(!gone.payload.contains("title"), "{}", gone.payload);
    assert_ne!(gone.msg_id, d.msg_id);
}

#[test]
fn delivery_is_signed_standard_webhooks_style_and_logged_by_host() {
    let (db, cr, http) = (
        MemoryStore::default(),
        FakeCrypto::default(),
        FakeHttp::default(),
    );
    let made = create(&db, &cr, &[WebhookEvent::PostPublished]);
    run(enqueue(&db, &cr, WebhookEvent::PostPublished, &post(), T)).unwrap();
    let at = T + 1_234;
    let r = deliver(&db, &cr, &http, at, Some(&key()));
    assert_eq!((r.claimed, r.delivered, r.failed), (1, 1, 0));

    let d = deliveries(&db, made.id).remove(0);
    let [(url, headers, body)] = http.sent.take().try_into().unwrap();
    assert_eq!(url, URL);
    assert_eq!(body, d.payload);
    let h = |k: &str| headers.iter().find(|(n, _)| *n == k).unwrap().1.clone();
    assert_eq!(h("content-type"), "application/json");
    assert_eq!(h("webhook-id"), d.msg_id);
    assert_eq!(h("webhook-timestamp"), (at / 1000).to_string());
    let secret = Secret::parse_display(&made.secret).unwrap();
    let want = mix(&[
        b"hmac",
        secret.bytes(),
        format!("{}.{}.{}", d.msg_id, at / 1000, d.payload).as_bytes(),
    ]);
    assert_eq!(h("webhook-signature"), format!("v1,{}", b64::encode(&want)));

    assert_eq!(
        (d.attempts, d.next_at_ms, d.last_status, d.last_error),
        (1, None, Some(200), None)
    );
    let [e] = log(&db, EventKind::WebhookDelivery).try_into().unwrap();
    assert_eq!(e.severity(), Severity::Info);
    assert_eq!(e.subject_text(), "post.published");
    assert_eq!(field(&e, "webhook"), Some(made.id.to_string().as_str()));
    assert_eq!(field(&e, "attempt"), Some("1"));
    assert_eq!(field(&e, "host"), Some("hooks.example.com"));
    assert_eq!(field(&e, "status"), Some("200"));
    let detail = e.detail();
    for leak in ["PATHSECRET", "token", "whsec_", "v1:", &made.secret[6..]] {
        assert!(!detail.contains(leak), "{leak} in {detail}");
    }
}

#[test]
fn failures_retry_at_10_min_1_h_6_h_then_give_up() {
    let (db, cr, http) = (
        MemoryStore::default(),
        FakeCrypto::default(),
        FakeHttp::default(),
    );
    let id = create(&db, &cr, &[WebhookEvent::PostPublished]).id;
    run(enqueue(&db, &cr, WebhookEvent::PostPublished, &post(), T)).unwrap();
    let answers = [
        Ok(500),
        Ok(301),
        Err(HttpError::Timeout),
        Err(HttpError::Network),
    ];
    let mut at = T;
    let mut bodies = Vec::new();
    for (n, a) in answers.into_iter().enumerate() {
        http.answer(a);
        let r = deliver(&db, &cr, &http, at, Some(&key()));
        assert_eq!((r.claimed, r.failed), (1, 1), "attempt {}", n + 1);
        let d = deliveries(&db, id).remove(0);
        assert_eq!(d.attempts, i64::try_from(n).unwrap() + 1);
        let wait = crate::RETRY_DELAYS_MS.get(n).copied();
        assert_eq!(d.next_at_ms, wait.map(|w| at + w), "attempt {}", n + 1);
        bodies.push(http.sent.borrow().last().unwrap().clone());
        // Nothing is due before the wait is over.
        if let Some(w) = wait {
            assert_eq!(
                deliver(&db, &cr, &http, at + w - 1, Some(&key())).claimed,
                0
            );
            at += w;
        }
    }
    let d = deliveries(&db, id).remove(0);
    assert_eq!(
        (d.last_status, d.last_error.as_deref()),
        (None, Some("network"))
    );
    assert_eq!(
        deliver(&db, &cr, &http, at + 100 * 3_600_000, Some(&key())).claimed,
        0
    );
    // Same id and bytes on every attempt; only the timestamp and signature move.
    let ids: Vec<_> = bodies.iter().map(|(_, h, _)| h[1].1.clone()).collect();
    assert!(ids.iter().all(|i| *i == ids[0]));
    assert!(bodies.iter().all(|(_, _, b)| *b == bodies[0].2));

    let events = log(&db, EventKind::WebhookDelivery);
    let summary: Vec<_> = events
        .iter()
        .map(|e| {
            (
                field(e, "attempt").unwrap().to_owned(),
                field(e, "status").map(str::to_owned),
                field(e, "reason").unwrap().to_owned(),
                field(e, "retry").unwrap().to_owned(),
                e.error_code(),
                e.severity(),
            )
        })
        .collect();
    let f = Some(ErrorCode::WebhookDeliveryFailed);
    let w = Severity::Warning;
    assert_eq!(
        summary,
        [
            (
                "1".into(),
                Some("500".into()),
                "status".into(),
                "yes".into(),
                f,
                w
            ),
            (
                "2".into(),
                Some("301".into()),
                "redirect".into(),
                "yes".into(),
                f,
                w
            ),
            ("3".into(), None, "timeout".into(), "yes".into(), f, w),
            ("4".into(), None, "network".into(), "no".into(), f, w),
        ]
    );
}

/// Sets webhook `id`'s url, enabled flag or stored secret behind the API's back.
fn tamper(db: &MemoryStore, id: i64, f: impl FnOnce(&mut Webhook)) {
    let mut w = webhooks(db).into_iter().find(|w| w.id == id).unwrap();
    f(&mut w);
    run(db.update_webhook(&w)).unwrap();
}

#[test]
fn unsafe_or_unusable_rows_are_not_sent() {
    type Case = (
        fn(&mut Webhook),
        Option<&'static str>,
        &'static str,
        bool,
        ErrorCode,
    );
    let cases: [Case; 5] = [
        (
            |w| w.secret = "s3cret".into(),
            Some("k"),
            "secret_unreadable",
            false,
            ErrorCode::WebhookSecretUnreadable,
        ),
        (
            |w| w.secret = "v1:AAAAAAAAAAAAAAAA:AAAA".into(),
            Some("k"),
            "secret_unreadable",
            false,
            ErrorCode::WebhookSecretUnreadable,
        ),
        (
            |w| w.url = "http://127.0.0.1:8080/".into(),
            Some("k"),
            "url_blocked",
            false,
            ErrorCode::WebhookUrlBlocked,
        ),
        (
            |w| w.enabled = false,
            Some("k"),
            "disabled",
            false,
            ErrorCode::WebhookDeliveryFailed,
        ),
        (
            |_| {},
            None,
            "key_invalid",
            true,
            ErrorCode::WebhookKeyInvalid,
        ),
    ];
    for (change, k, reason, retried, code) in cases {
        let (db, cr, http) = (
            MemoryStore::default(),
            FakeCrypto::default(),
            FakeHttp::default(),
        );
        let id = create(&db, &cr, &[WebhookEvent::PostPublished]).id;
        run(enqueue(&db, &cr, WebhookEvent::PostPublished, &post(), T)).unwrap();
        tamper(&db, id, change);
        let good = key();
        let r = deliver(&db, &cr, &http, T, k.map(|_| good.as_str()));
        assert_eq!((r.claimed, r.failed), (1, 1), "{reason}");
        assert!(
            http.sent.borrow().is_empty(),
            "{reason}: nothing may be sent"
        );
        let d = deliveries(&db, id).remove(0);
        assert_eq!(d.last_error.as_deref(), Some(reason));
        assert_eq!(d.next_at_ms.is_some(), retried, "{reason}");
        let [e] = log(&db, EventKind::WebhookDelivery).try_into().unwrap();
        assert_eq!(e.error_code(), Some(code), "{reason}");
        assert_eq!(field(&e, "reason"), Some(reason));
        assert!(!e.detail().contains("s3cret"), "{}", e.detail());
        if reason == "url_blocked" {
            assert_eq!(field(&e, "url_rule"), Some("not_https"));
            assert_eq!(field(&e, "host"), None);
        }
    }
}

#[test]
fn crypto_failure_is_retried() {
    let (db, cr, http) = (
        MemoryStore::default(),
        FakeCrypto::default(),
        FakeHttp::default(),
    );
    let id = create(&db, &cr, &[WebhookEvent::PostPublished]).id;
    run(enqueue(&db, &cr, WebhookEvent::PostPublished, &post(), T)).unwrap();
    cr.broken_hmac.set(true);
    deliver(&db, &cr, &http, T, Some(&key()));
    let d = deliveries(&db, id).remove(0);
    assert_eq!(
        (d.last_error.as_deref(), d.next_at_ms),
        (Some("crypto"), Some(T + 600_000))
    );
    assert!(http.sent.borrow().is_empty());
}

#[test]
fn a_run_that_died_is_retried_after_the_lease_and_abandoned_after_max() {
    let (db, cr, http) = (
        MemoryStore::default(),
        FakeCrypto::default(),
        FakeHttp::default(),
    );
    let id = create(&db, &cr, &[WebhookEvent::PostPublished]).id;
    run(enqueue(&db, &cr, WebhookEvent::PostPublished, &post(), T)).unwrap();
    // A run claims, then dies before sending or saving.
    run(db.claim_deliveries(T, T + LEASE_MS, 40)).unwrap();
    assert_eq!(
        deliver(&db, &cr, &http, T, Some(&key())).claimed,
        0,
        "leased"
    );
    let r = deliver(&db, &cr, &http, T + LEASE_MS, Some(&key()));
    assert_eq!((r.claimed, r.delivered), (1, 1));
    assert_eq!(deliveries(&db, id)[0].attempts, 2);

    // Runs that keep dying use up the attempts; the next claim gives up without sending.
    run(enqueue(&db, &cr, WebhookEvent::PostPublished, &post(), T)).unwrap();
    let mut at = T;
    for _ in 0..crate::MAX_ATTEMPTS {
        run(db.claim_deliveries(at, at + LEASE_MS, 40)).unwrap();
        at += LEASE_MS;
    }
    http.sent.take();
    let r = deliver(&db, &cr, &http, at, Some(&key()));
    assert_eq!((r.claimed, r.failed), (1, 1));
    assert!(http.sent.borrow().is_empty());
    let d = deliveries(&db, id).remove(0);
    assert_eq!(
        (d.attempts, d.last_error.as_deref(), d.next_at_ms),
        (5, Some("abandoned"), None)
    );
}

#[test]
fn limit_and_clock_per_attempt() {
    let (db, cr, http) = (
        MemoryStore::default(),
        FakeCrypto::default(),
        FakeHttp::default(),
    );
    create(&db, &cr, &[WebhookEvent::PostPublished]);
    for _ in 0..3 {
        run(enqueue(&db, &cr, WebhookEvent::PostPublished, &post(), T)).unwrap();
    }
    let now = Cell::new(T);
    let sender = Sender {
        store: &db,
        crypto: &cr,
        http: &http,
        raw_key: Some(&key()),
        policy: Policy::Production,
    };
    let clock = || {
        now.set(now.get() + 60_000);
        now.get()
    };
    let r = run(sender.deliver_due(&scope(), clock, 2, |m| panic!("{m}"))).unwrap();
    assert_eq!((r.claimed, r.delivered), (2, 2));
    let stamps: Vec<String> = http
        .sent
        .borrow()
        .iter()
        .map(|(_, h, _)| h[2].1.clone())
        .collect();
    assert_eq!(stamps.len(), 2);
    assert_ne!(stamps[0], stamps[1], "each attempt reads the clock");
}

/// A store whose claims fail, to show the failure is logged and returned.
struct NoClaims(MemoryStore);

impl WebhookStore for NoClaims {
    async fn create_webhook(&self, w: NewWebhook) -> Result<i64> {
        self.0.create_webhook(w).await
    }
    async fn list_webhooks(&self) -> Result<Vec<Webhook>> {
        self.0.list_webhooks().await
    }
    async fn update_webhook(&self, w: &Webhook) -> Result<()> {
        self.0.update_webhook(w).await
    }
    async fn delete_webhook(&self, id: i64) -> Result<()> {
        self.0.delete_webhook(id).await
    }
}

#[expect(
    clippy::unused_async_trait_impl,
    reason = "delegates to the memory store"
)]
impl DeliveryStore for NoClaims {
    async fn add_deliveries(&self, rows: &[NewDelivery]) -> Result<Vec<i64>> {
        self.0.add_deliveries(rows).await
    }
    async fn claim_deliveries(&self, _: i64, _: i64, _: u32) -> Result<Vec<Delivery>> {
        Err(Error::new(ErrorCode::Storage, "D1 unavailable"))
    }
    async fn finish_attempt(&self, id: i64, attempt: i64, r: &AttemptResult) -> Result<()> {
        self.0.finish_attempt(id, attempt, r).await
    }
    async fn list_deliveries(&self, webhook_id: i64, limit: u32) -> Result<Vec<Delivery>> {
        self.0.list_deliveries(webhook_id, limit).await
    }
    async fn prune_deliveries(&self, before: i64, limit: u32) -> Result<u64> {
        Err(Error::new(ErrorCode::Storage, format!("{before} {limit}")))
    }
}

impl LogSink for NoClaims {
    async fn store_event(&self, event: Event) -> Result<()> {
        self.0.store_event(event).await
    }
}

#[test]
fn store_failures_are_returned_and_logged() {
    let (db, cr, http) = (
        NoClaims(MemoryStore::default()),
        FakeCrypto::default(),
        FakeHttp::default(),
    );
    let sender = Sender {
        store: &db,
        crypto: &cr,
        http: &http,
        raw_key: Some(&key()),
        policy: Policy::Production,
    };
    let e = run(sender.deliver_due(&scope(), || T, 40, |m| panic!("{m}"))).unwrap_err();
    assert_eq!(e.code, ErrorCode::Storage);
    let e = run(prune_job(&db, &scope(), T, |m| panic!("{m}"))).unwrap_err();
    assert_eq!(e.code, ErrorCode::Storage);
    let jobs = log(&db.0, EventKind::ScheduledJob);
    let got: Vec<_> = jobs
        .iter()
        .map(|e| (e.subject_text(), e.error_code()))
        .collect();
    assert_eq!(
        got,
        [
            (crate::DELIVERY_JOB, Some(ErrorCode::Storage)),
            (crate::PRUNE_JOB, Some(ErrorCode::Storage)),
        ]
    );
}

#[test]
fn prune_job_drops_old_finished_rows_and_logs_the_count() {
    let (db, cr, http) = (
        MemoryStore::default(),
        FakeCrypto::default(),
        FakeHttp::default(),
    );
    let id = create(&db, &cr, &[WebhookEvent::PostPublished]).id;
    run(enqueue(&db, &cr, WebhookEvent::PostPublished, &post(), T)).unwrap();
    deliver(&db, &cr, &http, T, Some(&key()));
    assert_eq!(
        run(prune_job(&db, &scope(), T + crate::KEEP_MS, |m| panic!(
            "{m}"
        )))
        .unwrap(),
        0
    );
    assert_eq!(
        run(prune_job(
            &db,
            &scope(),
            T + crate::KEEP_MS + 1,
            |m| panic!("{m}")
        ))
        .unwrap(),
        1
    );
    assert_eq!(deliveries(&db, id), []);
    let jobs = log(&db, EventKind::ScheduledJob);
    assert_eq!(jobs.len(), 2);
    assert_eq!(field(&jobs[1], "deleted"), Some("1"));
    assert_eq!(jobs[1].subject_text(), crate::PRUNE_JOB);
}
