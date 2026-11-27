//! The JSON body (Standard Webhooks shape: `id`, `type`, `created_at`, `data`). Post
//! bodies are never sent: receivers fetch the post from `url` if they want it.

use plumblog_model::PostId;
use plumblog_store::{Post, PostStatus};
use serde_json::{Value, json};

use crate::WebhookEvent;

/// What a payload says about a post. The caller supplies `url` (it knows the site's base URL
/// and permalink rules).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostInfo {
    pub post_id: PostId,
    pub slug: String,
    pub lang: String,
    pub title: String,
    pub url: String,
    pub status: PostStatus,
    pub published_at: Option<i64>,
    pub updated_at: i64,
}

impl PostInfo {
    #[must_use]
    pub fn from_post(post: &Post, url: String) -> Self {
        Self {
            post_id: post.id,
            slug: post.slug.clone(),
            lang: post.lang.clone(),
            title: post.title.clone(),
            url,
            status: post.status,
            published_at: post.published_at,
            updated_at: post.updated_at,
        }
    }
}

/// The body for `event` about `post`, as the exact bytes to store and sign. `post.deleted`
/// carries only `post_id`, `slug` and `lang`: the post is gone, so nothing else is true.
#[must_use]
pub fn build(msg_id: &str, event: WebhookEvent, post: &PostInfo, now_ms: i64) -> String {
    let data = match event {
        WebhookEvent::PostDeleted => json!({
            "post_id": post.post_id.0,
            "slug": post.slug,
            "lang": post.lang,
        }),
        WebhookEvent::PostPublished | WebhookEvent::PostUpdated => json!({
            "post_id": post.post_id.0,
            "slug": post.slug,
            "lang": post.lang,
            "title": post.title,
            "url": post.url,
            "status": post.status.as_str(),
            "published_at": post.published_at.map_or(Value::Null, |t| Value::from(rfc3339(t))),
            "updated_at": rfc3339(post.updated_at),
        }),
    };
    json!({
        "id": msg_id,
        "type": event.as_str(),
        "created_at": rfc3339(now_ms),
        "data": data,
    })
    .to_string()
}

/// UTC milliseconds as RFC 3339 with milliseconds, e.g. `2026-11-27T08:05:09.042Z`.
#[must_use]
pub fn rfc3339(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let milli = ms.rem_euclid(1000);
    let days = secs.div_euclid(86_400);
    let sod = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{milli:03}Z",
        sod / 3600,
        sod % 3600 / 60,
        sod % 60
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian date (H. Hinnant's `civil_from_days`).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::{PostInfo, build, rfc3339};
    use crate::WebhookEvent;
    use plumblog_model::PostId;
    use plumblog_store::PostStatus;
    use serde_json::{Value, json};

    fn info() -> PostInfo {
        PostInfo {
            post_id: PostId(7),
            slug: "hello".into(),
            lang: "vi".into(),
            title: "Xin \"chao\"".into(),
            url: "https://blog.example/vi/hello".into(),
            status: PostStatus::Published,
            published_at: Some(1_764_230_400_000),
            updated_at: 1_764_230_401_500,
        }
    }

    #[test]
    fn rfc3339_utc_with_millis() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(rfc3339(1_764_230_400_000), "2025-11-27T08:00:00.000Z");
        assert_eq!(rfc3339(951_782_400_042), "2000-02-29T00:00:00.042Z");
        assert_eq!(rfc3339(-1), "1969-12-31T23:59:59.999Z");
    }

    #[test]
    fn published_payload_shape() {
        let body = build(
            "msg_1",
            WebhookEvent::PostPublished,
            &info(),
            1_764_230_402_000,
        );
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(
            v,
            json!({
                "id": "msg_1",
                "type": "post.published",
                "created_at": "2025-11-27T08:00:02.000Z",
                "data": {
                    "post_id": 7,
                    "slug": "hello",
                    "lang": "vi",
                    "title": "Xin \"chao\"",
                    "url": "https://blog.example/vi/hello",
                    "status": "published",
                    "published_at": "2025-11-27T08:00:00.000Z",
                    "updated_at": "2025-11-27T08:00:01.500Z",
                }
            })
        );
        // The same input gives the same bytes: what is stored is what gets signed.
        assert_eq!(
            body,
            build(
                "msg_1",
                WebhookEvent::PostPublished,
                &info(),
                1_764_230_402_000
            )
        );
    }

    #[test]
    fn updated_payload_allows_no_publish_date() {
        let mut p = info();
        p.published_at = None;
        p.status = PostStatus::Draft;
        let v: Value = serde_json::from_str(&build("m", WebhookEvent::PostUpdated, &p, 0)).unwrap();
        assert_eq!(v["type"], "post.updated");
        assert_eq!(v["data"]["published_at"], Value::Null);
        assert_eq!(v["data"]["status"], "draft");
    }

    #[test]
    fn deleted_payload_has_no_title_or_url() {
        let v: Value =
            serde_json::from_str(&build("m", WebhookEvent::PostDeleted, &info(), 0)).unwrap();
        assert_eq!(
            v["data"],
            json!({"post_id": 7, "slug": "hello", "lang": "vi"})
        );
    }
}
