//! Which event, if any, a change to a post sends. Every write path (cron `publish_due` today,
//! the write routes later) asks this one function, so receivers see the same rules from all.

use plumblog_store::PostStatus;

use crate::{PostInfo, WebhookEvent};

fn live(p: Option<&PostInfo>) -> Option<&PostInfo> {
    p.filter(|p| p.status == PostStatus::Published)
}

/// The event for a post going from `before` to `after` (`None`: did not exist / is gone),
/// and the post to send it about. `body_changed`: the caller saw the Markdown change (the
/// payload has no body, and `updated_at` moves on every save, so it cannot tell).
///
/// - not published -> published (incl. scheduled -> published by cron): `post.published`;
/// - published -> published: `post.updated` only when something a receiver sees changed
///   (title, slug, lang, url, status, `published_at`, body); a slug or URL change also
///   carries the old URL as `previous_url`. A no-op save sends nothing;
/// - published -> anything else or gone: `post.deleted` about the post as it was;
/// - never published on either side: nothing. Deleting a trashed post sends nothing: its
///   `post.deleted` went out when it left published (trash).
#[must_use]
pub fn event_for(
    before: Option<&PostInfo>,
    after: Option<&PostInfo>,
    body_changed: bool,
) -> Option<(WebhookEvent, PostInfo)> {
    match (live(before), live(after)) {
        (None, Some(a)) => Some((
            WebhookEvent::PostPublished,
            PostInfo {
                previous_url: None,
                ..a.clone()
            },
        )),
        (Some(b), None) => Some((
            WebhookEvent::PostDeleted,
            PostInfo {
                previous_url: None,
                ..b.clone()
            },
        )),
        (Some(b), Some(a)) => {
            let moved = b.url != a.url;
            let seen = moved
                || body_changed
                || b.title != a.title
                || b.slug != a.slug
                || b.lang != a.lang
                || b.published_at != a.published_at;
            seen.then(|| {
                (
                    WebhookEvent::PostUpdated,
                    PostInfo {
                        previous_url: moved.then(|| b.url.clone()),
                        ..a.clone()
                    },
                )
            })
        }
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::event_for;
    use crate::{PostInfo, WebhookEvent};
    use plumblog_model::PostId;
    use plumblog_store::PostStatus::{self, Draft, Published, Scheduled, Trashed};

    fn p(status: PostStatus) -> PostInfo {
        PostInfo {
            post_id: PostId(9),
            slug: "hello".into(),
            lang: "en".into(),
            title: "Hello".into(),
            url: "https://blog.example/en/hello".into(),
            status,
            published_at: Some(1_000),
            updated_at: 2_000,
            previous_url: None,
        }
    }

    fn ev(
        b: Option<&PostInfo>,
        a: Option<&PostInfo>,
        body: bool,
    ) -> Option<(WebhookEvent, Option<String>)> {
        event_for(b, a, body).map(|(e, p)| (e, p.previous_url))
    }

    #[test]
    fn becoming_published_sends_published() {
        for before in [None, Some(p(Draft)), Some(p(Scheduled)), Some(p(Trashed))] {
            assert_eq!(
                ev(before.as_ref(), Some(&p(Published)), false),
                Some((WebhookEvent::PostPublished, None)),
                "{before:?}"
            );
        }
    }

    #[test]
    fn leaving_published_sends_deleted_about_the_old_post() {
        for after in [None, Some(p(Draft)), Some(p(Scheduled)), Some(p(Trashed))] {
            let (e, post) = event_for(Some(&p(Published)), after.as_ref(), true).unwrap();
            assert_eq!(e, WebhookEvent::PostDeleted, "{after:?}");
            assert_eq!(post.status, Published, "the post as receivers knew it");
        }
    }

    #[test]
    fn never_published_sends_nothing() {
        let states = [None, Some(p(Draft)), Some(p(Scheduled)), Some(p(Trashed))];
        for b in &states {
            for a in &states {
                assert_eq!(ev(b.as_ref(), a.as_ref(), true), None, "{b:?} -> {a:?}");
            }
        }
    }

    #[test]
    fn noop_save_of_published_sends_nothing() {
        let mut a = p(Published);
        // Every save moves updated_at; alone it is not a change a receiver sees.
        a.updated_at += 5_000;
        assert_eq!(ev(Some(&p(Published)), Some(&a), false), None);
    }

    #[test]
    fn visible_changes_send_updated() {
        let base = p(Published);
        let edits: [fn(&mut PostInfo); 3] = [
            |x| x.title = "New".into(),
            |x| x.published_at = Some(500),
            |x| x.lang = "vi".into(),
        ];
        for edit in edits {
            let mut a = base.clone();
            edit(&mut a);
            assert_eq!(
                ev(Some(&base), Some(&a), false),
                Some((WebhookEvent::PostUpdated, None))
            );
        }
        assert_eq!(
            ev(Some(&base), Some(&base), true),
            Some((WebhookEvent::PostUpdated, None)),
            "body changed"
        );
    }

    #[test]
    fn slug_change_carries_previous_url() {
        let base = p(Published);
        let mut a = base.clone();
        a.slug = "hi".into();
        a.url = "https://blog.example/en/hi".into();
        let (e, post) = event_for(Some(&base), Some(&a), false).unwrap();
        assert_eq!(e, WebhookEvent::PostUpdated);
        assert_eq!(post.url, "https://blog.example/en/hi");
        assert_eq!(
            post.previous_url.as_deref(),
            Some("https://blog.example/en/hello")
        );
    }
}
