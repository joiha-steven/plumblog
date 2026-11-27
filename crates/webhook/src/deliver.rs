//! The flows: save a webhook, queue deliveries for an event, send what is due, prune.

use plumblog_model::{Error, ErrorCode, Result};
use plumblog_store::{
    AttemptResult, Delivery, DeliveryStore, NewDelivery, NewWebhook, Webhook, WebhookStore,
};
use plumblog_syslog::{EventKind, LogSink, Scope};

use crate::retry::{
    Failure, LEASE_MS, MAX_ATTEMPTS, MAX_PER_RUN, Outcome, classify, next_attempt_at,
};
use crate::secret::{SealKey, Secret, open, seal};
use crate::ssrf::{Blocked, Policy, check};
use crate::{
    KEEP_MS, PRUNE_PER_RUN, PostInfo, WebhookCrypto, WebhookEvent, WebhookHttp, events_column,
    payload, sign, wants,
};

/// Subject of the `job.run` entry written when a delivery run cannot start.
pub const DELIVERY_JOB: &str = "webhook_deliveries.send";
/// Subject of the prune job's `job.run` entry.
pub const PRUNE_JOB: &str = "webhook_deliveries.retention";

/// A saved webhook and its secret in the `whsec_` form: the only time it is ever returned.
#[derive(Clone, PartialEq, Eq)]
pub struct Created {
    pub id: i64,
    pub secret: String,
}

/// Hides the secret: a `{:?}` in a log line or a failed assertion must not print it.
impl std::fmt::Debug for Created {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Created")
            .field("id", &self.id)
            .field("secret", &"<hidden>")
            .finish()
    }
}

fn guard_url(url: &str, policy: Policy) -> Result<()> {
    check(url, policy).map(drop).map_err(|why| {
        let hint = if why == Blocked::NonAsciiHost {
            "; write the host in its punycode form (xn--...)"
        } else {
            ""
        };
        Error::new(
            ErrorCode::WebhookUrlBlocked,
            format!("webhook URL refused: {}{hint}", why.as_str()),
        )
    })
}

/// Saves an enabled webhook with a new secret sealed under `raw_key` (the `WEBHOOK_KEY`
/// value). Nothing is saved on any error.
///
/// # Errors
/// [`ErrorCode::WebhookUrlBlocked`] for a URL the guard refuses; `InvalidInput` for no
/// events; [`ErrorCode::WebhookKeyInvalid`] when the key is missing or malformed; the
/// crypto backend's or the store's error.
pub async fn create_webhook<S: WebhookStore, C: WebhookCrypto>(
    store: &S,
    crypto: &C,
    raw_key: Option<&str>,
    url: &str,
    events: &[WebhookEvent],
    policy: Policy,
    now_ms: i64,
) -> Result<Created> {
    guard_url(url, policy)?;
    if events.is_empty() {
        return Err(Error::new(
            ErrorCode::InvalidInput,
            "a webhook needs at least one event",
        ));
    }
    let key = SealKey::parse(raw_key)?;
    let secret = Secret::generate(crypto)?;
    let sealed = seal(crypto, &key, &secret).await?;
    let id = store
        .create_webhook(NewWebhook {
            url: url.to_owned(),
            events: events_column(events),
            secret: sealed,
            enabled: true,
            created_at: now_ms,
        })
        .await?;
    Ok(Created {
        id,
        secret: secret.display(),
    })
}

/// Replaces webhook `id`'s secret and returns the new one in the `whsec_` form, once. The
/// old secret stops working at once; deliveries already queued are signed with the new one.
///
/// # Errors
/// [`ErrorCode::WebhookKeyInvalid`], `NotFound`, or the crypto backend's or store's error.
pub async fn rotate_secret<S: WebhookStore, C: WebhookCrypto>(
    store: &S,
    crypto: &C,
    raw_key: Option<&str>,
    id: i64,
) -> Result<String> {
    let key = SealKey::parse(raw_key)?;
    let mut webhook = store
        .list_webhooks()
        .await?
        .into_iter()
        .find(|w| w.id == id)
        .ok_or_else(|| Error::new(ErrorCode::NotFound, format!("webhook {id} not found")))?;
    let secret = Secret::generate(crypto)?;
    webhook.secret = seal(crypto, &key, &secret).await?;
    store.update_webhook(&webhook).await?;
    Ok(secret.display())
}

fn msg_id<C: WebhookCrypto>(crypto: &C) -> Result<String> {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut b = [0u8; 16];
    crypto.random_bytes(&mut b)?;
    let mut s = String::with_capacity(4 + 32);
    s.push_str("msg_");
    for byte in b {
        s.push(char::from(HEX[usize::from(byte >> 4)]));
        s.push(char::from(HEX[usize::from(byte & 15)]));
    }
    Ok(s)
}

/// Queues one delivery of `event` per enabled webhook subscribed to it, in one write, due at
/// once; returns their ids (empty when nobody listens). The caller then runs
/// [`Sender::deliver_due`] (attempt 1 right after the response; retries from cron).
///
/// # Errors
/// The RNG's or the store's error; then nothing was queued.
pub async fn enqueue<S: WebhookStore + DeliveryStore, C: WebhookCrypto>(
    store: &S,
    crypto: &C,
    event: WebhookEvent,
    post: &PostInfo,
    now_ms: i64,
) -> Result<Vec<i64>> {
    let mut rows = Vec::new();
    for w in store.list_webhooks().await? {
        if !wants(&w, event) {
            continue;
        }
        let id = msg_id(crypto)?;
        rows.push(NewDelivery {
            webhook_id: w.id,
            payload: payload::build(&id, event, post, now_ms),
            msg_id: id,
            event: event.as_str().to_owned(),
            created_at_ms: now_ms,
        });
    }
    store.add_deliveries(&rows).await
}

/// What a delivery run did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Run {
    pub claimed: usize,
    pub delivered: usize,
    pub failed: usize,
    /// Rows whose webhook was deleted mid-run.
    pub skipped: usize,
}

/// Everything a delivery run needs from the platform.
pub struct Sender<'a, S, C, H> {
    pub store: &'a S,
    pub crypto: &'a C,
    pub http: &'a H,
    /// The `WEBHOOK_KEY` value.
    pub raw_key: Option<&'a str>,
    pub policy: Policy,
}

/// One attempt's result before it is saved and logged.
struct Attempt {
    host: Option<String>,
    status: Option<u16>,
    failure: Option<Failure>,
    url_rule: Option<&'static str>,
}

impl Attempt {
    fn failed(failure: Failure) -> Self {
        Self {
            host: None,
            status: None,
            failure: Some(failure),
            url_rule: None,
        }
    }
}

impl<S, C, H> Sender<'_, S, C, H>
where
    S: WebhookStore + DeliveryStore + LogSink,
    C: WebhookCrypto,
    H: WebhookHttp,
{
    /// Claims up to `limit` due deliveries (cap it with [`crate::MAX_PER_RUN`]), sends each
    /// once and records the outcome: delivered, retry at the next step of the schedule, or
    /// given up. Each outcome is one `webhook.delivery` log entry (host only, never the URL
    /// or secret). `clock` is read per attempt so a long run still sends fresh timestamps.
    /// Problems saving one outcome go to `report`, and the run goes on.
    ///
    /// # Errors
    /// When the due rows or the webhooks cannot be read; a `job.run` entry records it.
    pub async fn deliver_due(
        &self,
        scope: &Scope,
        clock: impl Fn() -> i64,
        limit: u32,
        report: impl Fn(&str),
    ) -> Result<Run> {
        let start = clock();
        let loaded = match self
            .store
            // Capped here, not by each caller: more would overrun the subrequest budget and
            // the lease (see HTTP_TIMEOUT_MS).
            .claim_deliveries(
                start,
                start.saturating_add(LEASE_MS),
                limit.min(MAX_PER_RUN),
            )
            .await
        {
            Ok(due) if due.is_empty() => Ok((due, Vec::new())),
            Ok(due) => self.store.list_webhooks().await.map(|w| (due, w)),
            Err(e) => Err(e),
        };
        let (due, webhooks) = match loaded {
            Ok(v) => v,
            Err(e) => {
                let event = scope
                    .event(EventKind::ScheduledJob, ms(start))
                    .subject(DELIVERY_JOB)
                    .error(e.code);
                plumblog_syslog::record(self.store, event, &report).await;
                return Err(e);
            }
        };
        let key = SealKey::parse(self.raw_key);
        let mut run = Run {
            claimed: due.len(),
            ..Run::default()
        };
        for d in &due {
            let Some(w) = webhooks.iter().find(|w| w.id == d.webhook_id) else {
                run.skipped += 1;
                continue;
            };
            let attempt = self.attempt(d, w, key.as_ref().ok(), &clock).await;
            if attempt.failure.is_some() {
                run.failed += 1;
            } else {
                run.delivered += 1;
            }
            self.finish(scope, d, w, &attempt, clock(), &report).await;
        }
        Ok(run)
    }

    async fn attempt(
        &self,
        d: &Delivery,
        w: &Webhook,
        key: Option<&SealKey>,
        clock: &impl Fn() -> i64,
    ) -> Attempt {
        if d.attempts > MAX_ATTEMPTS {
            return Attempt::failed(Failure::Abandoned);
        }
        if !w.enabled {
            return Attempt::failed(Failure::Disabled);
        }
        let target = match check(&w.url, self.policy) {
            Ok(t) => t,
            Err(why) => {
                return Attempt {
                    url_rule: Some(why.as_str()),
                    ..Attempt::failed(Failure::UrlBlocked)
                };
            }
        };
        let host = Some(target.host);
        let fail = |failure| Attempt {
            host: host.clone(),
            ..Attempt::failed(failure)
        };
        let Some(key) = key else {
            return fail(Failure::KeyInvalid);
        };
        let Ok(secret) = open(self.crypto, key, &w.secret).await else {
            return fail(Failure::SecretUnreadable);
        };
        let ts = clock().div_euclid(1000);
        let content = sign::signed_content(&d.msg_id, ts, &d.payload);
        let Ok(mac) = self
            .crypto
            .hmac_sha256(secret.bytes(), content.as_bytes())
            .await
        else {
            return fail(Failure::Crypto);
        };
        let headers = sign::headers(&d.msg_id, ts, &mac);
        match classify(self.http.post(&w.url, &headers, &d.payload).await) {
            Outcome::Delivered { status } => Attempt {
                host,
                status: Some(status),
                failure: None,
                url_rule: None,
            },
            Outcome::Failed { status, failure } => Attempt {
                status,
                ..fail(failure)
            },
        }
    }

    async fn finish(
        &self,
        scope: &Scope,
        d: &Delivery,
        w: &Webhook,
        a: &Attempt,
        now_ms: i64,
        report: &impl Fn(&str),
    ) {
        let next_at_ms = match a.failure {
            Some(f) if f.retryable() => next_attempt_at(d.attempts, now_ms),
            _ => None,
        };
        let result = AttemptResult {
            next_at_ms,
            last_status: a.status.map(i64::from),
            last_error: a.failure.map(|f| f.as_str().to_owned()),
        };
        if let Err(e) = self.store.finish_attempt(d.id, d.attempts, &result).await {
            // NotFound: the webhook was deleted mid-run, nothing to keep. Conflict: a later
            // run took the row after our lease ran out and owns the outcome now.
            report(&format!(
                "{} webhook.finish_failed delivery={} attempt={} error={e}",
                scope.log_prefix(),
                d.id,
                d.attempts
            ));
        }
        let mut event = scope
            .event(EventKind::WebhookDelivery, ms(now_ms))
            .actor(None)
            .subject(&d.event)
            .field("webhook", &w.id.to_string())
            .field("attempt", &d.attempts.to_string());
        if let Some(host) = &a.host {
            event = event.field("host", host);
        }
        if let Some(s) = a.status {
            event = event.field("status", &s.to_string());
        }
        if let Some(f) = a.failure {
            event = event
                .field("reason", f.as_str())
                .field("retry", if next_at_ms.is_some() { "yes" } else { "no" })
                .error(error_code(f));
        }
        if let Some(rule) = a.url_rule {
            event = event.field("url_rule", rule);
        }
        plumblog_syslog::record(self.store, event, report).await;
    }
}

fn error_code(f: Failure) -> ErrorCode {
    match f {
        Failure::UrlBlocked => ErrorCode::WebhookUrlBlocked,
        Failure::KeyInvalid => ErrorCode::WebhookKeyInvalid,
        Failure::SecretUnreadable => ErrorCode::WebhookSecretUnreadable,
        Failure::Redirect
        | Failure::Status
        | Failure::Timeout
        | Failure::Network
        | Failure::Disabled
        | Failure::Abandoned
        | Failure::Crypto => ErrorCode::WebhookDeliveryFailed,
    }
}

/// Log times are unsigned; a time before 1970 is a bug upstream, logged as 0.
fn ms(t: i64) -> u64 {
    u64::try_from(t).unwrap_or(0)
}

/// Deletes up to [`PRUNE_PER_RUN`] deliveries that finished and are older than
/// [`KEEP_MS`], and records one `job.run` entry with how many went (or the error).
///
/// # Errors
/// The prune's error.
pub async fn prune_job<S: DeliveryStore + LogSink>(
    store: &S,
    scope: &Scope,
    now_ms: i64,
    report: impl FnOnce(&str),
) -> Result<u64> {
    let pruned = store
        .prune_deliveries(now_ms.saturating_sub(KEEP_MS), PRUNE_PER_RUN)
        .await;
    let event = scope
        .event(EventKind::ScheduledJob, ms(now_ms))
        .subject(PRUNE_JOB);
    let event = match &pruned {
        Ok(n) => event.field("deleted", &n.to_string()),
        Err(e) => event.error(e.code),
    };
    plumblog_syslog::record(store, event, report).await;
    pruned
}
