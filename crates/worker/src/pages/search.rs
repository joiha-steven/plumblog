//! Reader search (`/search?q=`): one store query per search, never cached (any query string
//! is a new URL, and the store's content can change without the pages' content version).
//! The FTS query is built by `plumblog_store::search::to_fts_query`, which quotes every word.

use super::{PageResult, Pages, err, routes};
use crate::d1::D1Store;
use plumblog_markdown::Rendered;
use plumblog_render::{Body, Post};
use plumblog_store::{PostStore, PostSummary, search};
use worker::Env;

/// Longest query kept, in characters. The store keeps at most `search::MAX_QUERY_WORDS`
/// words anyway; this bounds the work and the text echoed on the page.
pub const MAX_QUERY_CHARS: usize = 100;
/// Most results shown: one page, rendered well inside the CPU budget. A wider query is
/// told to add a word rather than paged.
pub const RESULT_LIMIT: usize = 20;

/// Where search reads from. The Worker uses D1; host tests use the in-memory store.
// Workers are single-threaded, so futures need not be `Send` (see auth::Crypto).
#[allow(async_fn_in_trait)]
pub trait SearchSource {
    async fn search(
        &self,
        lang: &str,
        query: &str,
        limit: u32,
    ) -> std::result::Result<Vec<PostSummary>, String>;
}

/// Any store with posts.
pub struct StoreSearch<'a, S>(pub &'a S);

impl<S: PostStore> SearchSource for StoreSearch<'_, S> {
    async fn search(
        &self,
        lang: &str,
        query: &str,
        limit: u32,
    ) -> std::result::Result<Vec<PostSummary>, String> {
        self.0.search(lang, query, limit).await.map_err(err)
    }
}

/// D1 from the Worker's `DB` binding, opened only when a search runs.
pub struct EnvSearch<'a>(pub &'a Env);

impl SearchSource for EnvSearch<'_> {
    async fn search(
        &self,
        lang: &str,
        query: &str,
        limit: u32,
    ) -> std::result::Result<Vec<PostSummary>, String> {
        let db = D1Store::new(self.0.d1("DB").map_err(|e| format!("D1 binding DB: {e}"))?);
        StoreSearch(&db).search(lang, query, limit).await
    }
}

/// The query as shown and searched: trimmed, at most [`MAX_QUERY_CHARS`] characters.
pub fn clean(raw: &str) -> &str {
    let t = raw.trim();
    let end = t
        .char_indices()
        .nth(MAX_QUERY_CHARS)
        .map_or(t.len(), |(i, _)| i);
    t[..end].trim_end()
}

impl<S: super::source::ReaderSource> Pages<'_, S> {
    /// Empty or word-less queries show the form (or "nothing found") without a store query.
    pub(super) async fn search(&self, from: &impl SearchSource, raw: &str) -> PageResult {
        let query = clean(raw);
        let mut found = if search::query_words(query).is_empty() {
            Vec::new()
        } else {
            let limit = u32::try_from(RESULT_LIMIT + 1).unwrap_or(u32::MAX);
            from.search(&self.info.lang, query, limit).await?
        };
        let more = found.len() > RESULT_LIMIT;
        found.truncate(RESULT_LIMIT);
        let posts: Vec<Post> = found.iter().map(to_render).collect();
        let refs: Vec<&Post> = posts.iter().collect();
        self.show(&Body::Search {
            query,
            posts: &refs,
            more,
        })
    }
}

/// A search hit as a list entry. Tags are left out: their names would cost a second query.
fn to_render(s: &PostSummary) -> Post {
    Post {
        url: routes::post(&s.slug),
        title: s.title.clone(),
        lang: s.lang.clone(),
        // The store keeps milliseconds; pages use seconds.
        published_at: s.published_at.unwrap_or(s.created_at).div_euclid(1000),
        pinned: false,
        excerpt: (!s.excerpt.trim().is_empty()).then(|| s.excerpt.clone()),
        body: Rendered {
            reading_minutes: u32::try_from(s.reading_minutes).unwrap_or(0),
            ..Rendered::default()
        },
        tags: Vec::new(),
        cover: None,
        thumb: None,
        translations: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_QUERY_CHARS, clean};

    #[test]
    fn queries_are_trimmed_and_capped_on_a_char_boundary() {
        assert_eq!(clean("  da lat  "), "da lat");
        let long = "\u{0111}".repeat(MAX_QUERY_CHARS + 50);
        assert_eq!(clean(&long).chars().count(), MAX_QUERY_CHARS);
        assert_eq!(clean(""), "");
    }
}
