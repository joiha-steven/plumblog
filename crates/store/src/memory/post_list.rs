//! Post listings and search: filters, orders and keyset cursors.

use std::cmp::{Ordering, Reverse};

use plumblog_model::{PostId, Result};

use super::{State, invalid};
use crate::{
    Post, PostCursor, PostOrder, PostQuery, PostStatus, PostSummary, page_len, probe_len, search,
};

/// Sort key of a row under `order`, comparable with a cursor's.
fn key_of(order: PostOrder, p: &Post) -> Option<i64> {
    match order {
        PostOrder::PublishedDesc => p.published_at,
        PostOrder::UpdatedDesc => Some(p.updated_at),
        PostOrder::SeriesAsc => p.series_position,
    }
}

/// Position of `a` relative to `b` in the listing order.
fn compare(order: PostOrder, a: (Option<i64>, PostId), b: (Option<i64>, PostId)) -> Ordering {
    match order {
        PostOrder::PublishedDesc | PostOrder::UpdatedDesc => b.cmp(&a),
        // NULLS LAST: a missing position sorts after every number.
        PostOrder::SeriesAsc => (a.0.is_none(), a.0, a.1).cmp(&(b.0.is_none(), b.0, b.1)),
    }
}

impl State {
    pub(super) fn list_posts(&self, q: &PostQuery) -> Result<Vec<PostSummary>> {
        if let Some(PostCursor { key: None, id }) = q.after
            && q.order != PostOrder::SeriesAsc
        {
            return Err(invalid(format!(
                "cursor after post {} has no sort value for {:?}",
                id.0, q.order
            )));
        }
        let mut rows: Vec<&Post> = self
            .posts
            .values()
            .filter(|p| q.kind.is_none_or(|k| p.kind == k))
            .filter(|p| q.status.is_none_or(|s| p.status == s))
            .filter(|p| q.lang.as_ref().is_none_or(|l| &p.lang == l))
            .filter(|p| q.author.is_none_or(|a| p.author_id == a))
            .filter(|p| q.tag.is_none_or(|t| p.tag_ids.contains(&t)))
            .filter(|p| q.series.is_none_or(|x| p.series_id == Some(x)))
            .filter(|p| q.pinned.is_none_or(|b| p.pinned == b))
            .filter(|p| q.order != PostOrder::PublishedDesc || p.published_at.is_some())
            .filter(|p| {
                q.after.is_none_or(|c| {
                    compare(q.order, (key_of(q.order, p), p.id), (c.key, c.id)) == Ordering::Greater
                })
            })
            .collect();
        rows.sort_by(|a, b| {
            compare(
                q.order,
                (key_of(q.order, a), a.id),
                (key_of(q.order, b), b.id),
            )
        });
        Ok(rows
            .into_iter()
            .take(probe_len(q.limit))
            .map(PostSummary::from)
            .collect())
    }

    pub(super) fn search(&self, lang: &str, query: &str, limit: u32) -> Vec<PostSummary> {
        let mut rows: Vec<&Post> = self
            .posts
            .values()
            .filter(|p| p.status == PostStatus::Published && p.lang == lang)
            .filter(|p| {
                self.fts
                    .get(&p.id)
                    .is_some_and(|(title, body)| search::matches(query, title, body))
            })
            .collect();
        rows.sort_by_key(|p| Reverse((p.published_at, p.id)));
        rows.into_iter()
            .take(page_len(limit))
            .map(PostSummary::from)
            .collect()
    }
}
