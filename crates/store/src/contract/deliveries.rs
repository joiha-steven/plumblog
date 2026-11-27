//! Webhook deliveries: queueing, claiming with a lease, finishing, cascade and pruning.

use plumblog_model::ErrorCode;

use super::kit::{Check, Got, T0, eq, fails, ok};
use crate::{AttemptResult, Delivery, NewDelivery, NewWebhook, Store};

async fn webhook<S: Store>(db: &S, url: &str) -> Got<i64> {
    ok(
        "create_webhook",
        db.create_webhook(NewWebhook {
            url: url.into(),
            events: "post.published".into(),
            secret: "v1:x:y".into(),
            enabled: true,
            created_at: T0,
        })
        .await,
    )
}

fn delivery(webhook_id: i64, msg: &str, at: i64) -> NewDelivery {
    NewDelivery {
        webhook_id,
        msg_id: msg.into(),
        event: "post.published".into(),
        payload: format!("{{\"id\":\"{msg}\"}}"),
        created_at_ms: at,
    }
}

fn ids(v: &[Delivery]) -> Vec<i64> {
    v.iter().map(|d| d.id).collect()
}

fn retry_at(t: i64) -> AttemptResult {
    AttemptResult {
        next_at_ms: Some(t),
        last_status: Some(500),
        last_error: Some("status".into()),
    }
}

fn done(status: i64) -> AttemptResult {
    AttemptResult {
        next_at_ms: None,
        last_status: Some(status),
        last_error: None,
    }
}

pub async fn deliveries_round_trip<S: Store>(db: &S) -> Check {
    let a = webhook(db, "https://a.example").await?;
    let b = webhook(db, "https://b.example").await?;
    let got = ok(
        "add_deliveries",
        db.add_deliveries(&[delivery(a, "m1", 100), delivery(b, "m2", 100)])
            .await,
    )?;
    eq!("two ids", got.len(), 2)?;
    eq!(
        "empty batch",
        ok("add_deliveries []", db.add_deliveries(&[]).await)?,
        Vec::<i64>::new(),
    )?;
    let more = ok(
        "add_deliveries",
        db.add_deliveries(&[delivery(a, "m3", 200)]).await,
    )?;
    let listed = ok("list_deliveries", db.list_deliveries(a, 10).await)?;
    eq!(
        "list_deliveries newest first, one webhook",
        ids(&listed),
        vec![more[0], got[0]],
    )?;
    eq!(
        "a new delivery is due at once, no attempts",
        listed.last().cloned(),
        Some(Delivery {
            id: got[0],
            webhook_id: a,
            msg_id: "m1".into(),
            event: "post.published".into(),
            payload: "{\"id\":\"m1\"}".into(),
            attempts: 0,
            next_at_ms: Some(100),
            last_status: None,
            last_error: None,
            created_at_ms: 100,
        }),
    )?;
    eq!(
        "list_deliveries limit",
        ok("list_deliveries", db.list_deliveries(a, 1).await)?.len(),
        1,
    )?;
    fails(
        "add_deliveries for a missing webhook",
        db.add_deliveries(&[delivery(b, "ok", 300), delivery(a.max(b) + 1000, "x", 300)])
            .await,
        ErrorCode::InvalidInput,
    )?;
    eq!(
        "a failed batch queues nothing",
        ok("list_deliveries", db.list_deliveries(b, 10).await)?.len(),
        1,
    )
}

pub async fn deliveries_claim_and_lease<S: Store>(db: &S) -> Check {
    let w = webhook(db, "https://a.example").await?;
    let late = ok(
        "add_deliveries",
        db.add_deliveries(&[delivery(w, "late", 300)]).await,
    )?[0];
    let first = ok(
        "add_deliveries",
        db.add_deliveries(&[delivery(w, "first", 100), delivery(w, "second", 200)])
            .await,
    )?;
    let future = ok(
        "add_deliveries",
        db.add_deliveries(&[delivery(w, "future", 5_000)]).await,
    )?[0];

    let claimed = ok("claim", db.claim_deliveries(1_000, 2_000, 2).await)?;
    eq!(
        "claims the earliest due first, up to the limit, returned by id",
        ids(&claimed),
        first.clone(),
    )?;
    eq!(
        "a claim counts the attempt and leases the row",
        claimed
            .iter()
            .map(|d| (d.attempts, d.next_at_ms))
            .collect::<Vec<_>>(),
        vec![(1, Some(2_000)), (1, Some(2_000))],
    )?;
    eq!(
        "leased rows are not claimed again; rows not yet due are left",
        ids(&ok("claim", db.claim_deliveries(1_000, 2_000, 10).await)?),
        vec![late],
    )?;
    eq!(
        "nothing due while leased",
        ok("claim", db.claim_deliveries(1_999, 3_000, 10).await)?.len(),
        0,
    )?;
    let again = ok("claim", db.claim_deliveries(2_000, 9_000, 10).await)?;
    eq!(
        "an expired lease is claimed again with the next attempt number",
        again.iter().map(|d| (d.id, d.attempts)).collect::<Vec<_>>(),
        {
            let mut v: Vec<(i64, i64)> = first.iter().map(|id| (*id, 2)).collect();
            v.push((late, 2));
            v.sort_unstable();
            v
        },
    )?;
    eq!(
        "limit 0 claims nothing",
        ok("claim", db.claim_deliveries(10_000, 20_000, 0).await)?.len(),
        0,
    )?;
    eq!(
        "the future row is due later",
        ids(&ok("claim", db.claim_deliveries(5_000, 6_000, 10).await)?),
        vec![future],
    )
}

pub async fn deliveries_finish<S: Store>(db: &S) -> Check {
    let w = webhook(db, "https://a.example").await?;
    let q = ok(
        "add_deliveries",
        db.add_deliveries(&[delivery(w, "a", 100), delivery(w, "b", 100)])
            .await,
    )?;
    let (a, b) = (q[0], q[1]);
    ok("claim", db.claim_deliveries(100, 1_000, 10).await)?;

    fails(
        "finish with a stale attempt number",
        db.finish_attempt(a, 2, &done(200)).await,
        ErrorCode::Conflict,
    )?;
    fails(
        "finish a missing delivery",
        db.finish_attempt(a.max(b) + 1000, 1, &done(200)).await,
        ErrorCode::NotFound,
    )?;
    ok("finish a", db.finish_attempt(a, 1, &retry_at(500)).await)?;
    ok("finish b", db.finish_attempt(b, 1, &done(204)).await)?;
    let rows = ok("list_deliveries", db.list_deliveries(w, 10).await)?;
    let get = |id: i64| rows.iter().find(|d| d.id == id).cloned();
    eq!(
        "a failed attempt is due again with its status and reason",
        get(a).map(|d| (d.attempts, d.next_at_ms, d.last_status, d.last_error)),
        Some((1, Some(500), Some(500), Some("status".to_owned()))),
    )?;
    eq!(
        "a delivered row is finished",
        get(b).map(|d| (d.attempts, d.next_at_ms, d.last_status, d.last_error)),
        Some((1, None, Some(204), None)),
    )?;
    eq!(
        "only the retry is claimed; finished rows never are",
        ids(&ok("claim", db.claim_deliveries(10_000, 20_000, 10).await)?),
        vec![a],
    )
}

pub async fn deliveries_cascade_and_prune<S: Store>(db: &S) -> Check {
    let keep = webhook(db, "https://a.example").await?;
    let gone = webhook(db, "https://b.example").await?;
    let q = ok(
        "add_deliveries",
        db.add_deliveries(&[
            delivery(keep, "old1", 100),
            delivery(keep, "old2", 200),
            delivery(keep, "old3", 300),
            delivery(keep, "pending", 50),
            delivery(keep, "new", 900),
            delivery(gone, "x", 100),
        ])
        .await,
    )?;
    let claimed = ok("claim", db.claim_deliveries(1_000, 2_000, 10).await)?;
    eq!("all claimed", claimed.len(), q.len())?;
    for id in [q[0], q[1], q[2], q[4], q[5]] {
        ok("finish", db.finish_attempt(id, 1, &done(200)).await)?;
    }

    ok("delete_webhook", db.delete_webhook(gone).await)?;
    eq!(
        "deleting a webhook deletes its deliveries",
        ok("list_deliveries", db.list_deliveries(gone, 10).await)?.len(),
        0,
    )?;
    fails(
        "finish after the webhook went",
        db.finish_attempt(q[5], 1, &done(200)).await,
        ErrorCode::NotFound,
    )?;

    eq!(
        "prune: oldest finished first, at most limit",
        ok("prune", db.prune_deliveries(500, 2).await)?,
        2,
    )?;
    eq!(
        "prune kept the newer old row, the pending row and the recent row",
        ids(&ok("list_deliveries", db.list_deliveries(keep, 10).await)?),
        vec![q[4], q[3], q[2]],
    )?;
    eq!(
        "prune: the rest of the backlog",
        ok("prune", db.prune_deliveries(500, 10).await)?,
        1,
    )?;
    eq!(
        "pending rows are never pruned",
        ids(&ok("list_deliveries", db.list_deliveries(keep, 10).await)?),
        vec![q[4], q[3]],
    )?;
    eq!(
        "nothing left to prune",
        ok("prune", db.prune_deliveries(500, 10).await)?,
        0,
    )
}
