//! Storage behind traits. The Worker implements them over D1/R2; `memory` implements them
//! in plain Rust so logic is tested without Cloudflare.

use plumblog_model::{PostId, Result};
use plumblog_syslog::Event;

/// The minimal post record storage needs today; fields grow with the posts issue.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostRecord {
    pub id: PostId,
    pub slug: String,
    pub title: String,
}

// Workers are single-threaded, so futures need not be `Send` (see auth::Crypto).
#[allow(async_fn_in_trait)]
pub trait PostStore {
    async fn get_post(&self, id: PostId) -> Result<PostRecord>;
    async fn put_post(&self, post: PostRecord) -> Result<()>;
}

#[allow(async_fn_in_trait)]
pub trait LogStore {
    async fn append(&self, event: Event) -> Result<()>;
    /// Newest first, at most `limit` entries.
    async fn recent(&self, limit: usize) -> Result<Vec<Event>>;
}

pub mod memory {
    use super::{Event, LogStore, PostId, PostRecord, PostStore, Result};
    use plumblog_model::{Error, ErrorCode};
    use plumblog_syslog::Logger;
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    /// In-memory store for tests. `RefCell` because traits take `&self` like the D1 impl.
    #[derive(Debug, Default)]
    pub struct MemoryStore {
        posts: RefCell<BTreeMap<PostId, PostRecord>>,
        log: RefCell<Vec<Event>>,
    }

    // `async fn` so nothing is read or written until the future is polled, as with D1.
    // The bodies never await, which pedantic clippy flags; that is the point of an in-memory
    // stand-in, so the lint is expected here and only here.
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "in-memory stand-in for async D1 storage"
    )]
    impl PostStore for MemoryStore {
        async fn get_post(&self, id: PostId) -> Result<PostRecord> {
            self.posts.borrow().get(&id).cloned().ok_or_else(|| {
                Error::new(ErrorCode::PostNotFound, format!("post {} not found", id.0))
            })
        }

        async fn put_post(&self, post: PostRecord) -> Result<()> {
            self.posts.borrow_mut().insert(post.id, post);
            Ok(())
        }
    }

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "in-memory stand-in for async D1 storage"
    )]
    impl LogStore for MemoryStore {
        async fn append(&self, event: Event) -> Result<()> {
            self.log.borrow_mut().push(event);
            Ok(())
        }

        async fn recent(&self, limit: usize) -> Result<Vec<Event>> {
            Ok(self
                .log
                .borrow()
                .iter()
                .rev()
                .take(limit)
                .cloned()
                .collect())
        }
    }

    impl Logger for MemoryStore {
        async fn record(&self, event: Event) -> Result<()> {
            self.append(event).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::memory::MemoryStore;
    use super::{PostRecord, PostStore};
    use plumblog_model::{ErrorCode, PostId};
    use std::pin::pin;
    use std::task::{Context, Poll, Waker};

    /// The memory store never awaits anything, so one poll finishes it; avoids an executor
    /// dependency just for tests.
    fn run<F: Future>(f: F) -> F::Output {
        match pin!(f).poll(&mut Context::from_waker(Waker::noop())) {
            Poll::Ready(v) => v,
            Poll::Pending => panic!("memory store future was pending"),
        }
    }

    #[test]
    fn missing_post_is_not_found_then_found_after_put() {
        let s = MemoryStore::default();
        let err = run(s.get_post(PostId(1))).unwrap_err();
        assert_eq!(err.code, ErrorCode::PostNotFound);
        let post = PostRecord {
            id: PostId(1),
            slug: "hello".into(),
            title: "Hello".into(),
        };
        // Like D1, an un-awaited write does nothing.
        drop(s.put_post(post.clone()));
        assert!(run(s.get_post(PostId(1))).is_err());
        run(s.put_post(post.clone())).unwrap();
        assert_eq!(run(s.get_post(PostId(1))).unwrap(), post);
    }
}
