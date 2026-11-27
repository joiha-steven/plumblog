//! Posts and pages, their revisions, and reader search (`posts`, `post_tags`, `revisions`,
//! `posts_fts`).

use plumblog_model::{MediaId, PostId, Result, RevisionId, SeriesId, TagId, UserId, stable_codes};

stable_codes! {
    /// Stored in `posts.kind`.
    pub enum PostKind {
        Post => "post",
        Page => "page",
    }
}

stable_codes! {
    /// Stored in `posts.status` and `posts.trashed_from`.
    pub enum PostStatus {
        Draft => "draft",
        Scheduled => "scheduled",
        Published => "published",
        Trashed => "trashed",
    }
}

/// A row of `posts` with its tags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Post {
    pub id: PostId,
    pub kind: PostKind,
    /// Id of the first post of the group; translations share it.
    pub translation_group: PostId,
    pub lang: String,
    pub slug: String,
    pub status: PostStatus,
    /// Status to restore to; `Some` exactly when `status` is `Trashed`.
    pub trashed_from: Option<PostStatus>,
    /// Set for scheduled and published posts.
    pub published_at: Option<i64>,
    pub author_id: UserId,
    pub title: String,
    pub markdown: String,
    pub html: String,
    pub toc_json: String,
    pub excerpt: String,
    pub word_count: i64,
    pub reading_minutes: i64,
    pub pinned: bool,
    pub comments_enabled: bool,
    pub cover_media_id: Option<MediaId>,
    pub series_id: Option<SeriesId>,
    pub series_position: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    /// Optimistic lock: 1 on insert, +1 on every save, trash, restore and scheduled publish.
    pub version: i64,
    /// Ascending.
    pub tag_ids: Vec<TagId>,
}

/// A post without its bodies (`markdown`, `html`, `toc_json`): list pages never load them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostSummary {
    pub id: PostId,
    pub kind: PostKind,
    pub translation_group: PostId,
    pub lang: String,
    pub slug: String,
    pub status: PostStatus,
    pub trashed_from: Option<PostStatus>,
    pub published_at: Option<i64>,
    pub author_id: UserId,
    pub title: String,
    pub excerpt: String,
    pub word_count: i64,
    pub reading_minutes: i64,
    pub pinned: bool,
    pub comments_enabled: bool,
    pub cover_media_id: Option<MediaId>,
    pub series_id: Option<SeriesId>,
    pub series_position: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
    pub version: i64,
    pub tag_ids: Vec<TagId>,
}

impl From<&Post> for PostSummary {
    fn from(p: &Post) -> Self {
        Self {
            id: p.id,
            kind: p.kind,
            translation_group: p.translation_group,
            lang: p.lang.clone(),
            slug: p.slug.clone(),
            status: p.status,
            trashed_from: p.trashed_from,
            published_at: p.published_at,
            author_id: p.author_id,
            title: p.title.clone(),
            excerpt: p.excerpt.clone(),
            word_count: p.word_count,
            reading_minutes: p.reading_minutes,
            pinned: p.pinned,
            comments_enabled: p.comments_enabled,
            cover_media_id: p.cover_media_id,
            series_id: p.series_id,
            series_position: p.series_position,
            created_at: p.created_at,
            updated_at: p.updated_at,
            version: p.version,
            tag_ids: p.tag_ids.clone(),
        }
    }
}

/// A snapshot of the saved title and Markdown, kept with the save.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NewRevision {
    pub author_id: Option<UserId>,
    pub created_at: i64,
}

/// A row of `revisions`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Revision {
    pub id: RevisionId,
    pub post_id: PostId,
    /// `None` once the author is deleted.
    pub author_id: Option<UserId>,
    pub title: String,
    pub markdown: String,
    pub created_at: i64,
}

/// Insert (`id: None`) or full update (`id: Some`) of a post.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavePost {
    pub id: Option<PostId>,
    /// The `version` the editor started from: required on update, `None` on insert
    /// (else `InvalidInput`). A stale version is `Conflict`, so a save never overwrites
    /// changes it did not see.
    pub expected_version: Option<i64>,
    /// Insert only: join this existing group (a translation); `None` starts a new group.
    /// Ignored on update: the group never changes.
    pub translation_group: Option<PostId>,
    pub kind: PostKind,
    pub lang: String,
    pub slug: String,
    /// Never `Trashed` (use `trash_post`).
    pub status: PostStatus,
    /// Required for scheduled and published.
    pub published_at: Option<i64>,
    pub author_id: UserId,
    pub title: String,
    pub markdown: String,
    pub html: String,
    pub toc_json: String,
    pub excerpt: String,
    pub word_count: i64,
    pub reading_minutes: i64,
    pub pinned: bool,
    pub comments_enabled: bool,
    pub cover_media_id: Option<MediaId>,
    pub series_id: Option<SeriesId>,
    pub series_position: Option<i64>,
    /// Replaces the post's tags; duplicates are ignored.
    pub tag_ids: Vec<TagId>,
    /// Media used in the body, extracted by the caller from the rendered post (the store
    /// never parses HTML). Replaces the post's media rows; duplicates are ignored.
    pub media_ids: Vec<MediaId>,
    /// Plain text for search; indexed after `search::fold`.
    pub search_text: String,
    /// `Some` keeps a revision of the new title and Markdown.
    pub revision: Option<NewRevision>,
    pub now: i64,
}

/// Sort order of [`PostQuery`]; also says what [`PostCursor::key`] holds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PostOrder {
    /// `published_at` DESC, id DESC; posts without `published_at` are excluded.
    PublishedDesc,
    /// `updated_at` DESC, id DESC.
    UpdatedDesc,
    /// `series_position` ASC with NULLs last, id ASC.
    SeriesAsc,
}

impl PostOrder {
    /// The cursor that continues a listing after `last`.
    #[must_use]
    pub fn cursor(self, last: &PostSummary) -> PostCursor {
        let key = match self {
            Self::PublishedDesc => last.published_at,
            Self::UpdatedDesc => Some(last.updated_at),
            Self::SeriesAsc => last.series_position,
        };
        PostCursor { key, id: last.id }
    }
}

/// Keyset cursor: the sort value and id of the last row of the previous page; the next page
/// starts strictly after it. `key` is `None` only for a `SeriesAsc` row without a position;
/// a `None` key with the other orders is `InvalidInput`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PostCursor {
    pub key: Option<i64>,
    pub id: PostId,
}

/// Every filter set must hold; `None` means any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostQuery {
    pub kind: Option<PostKind>,
    pub status: Option<PostStatus>,
    pub lang: Option<String>,
    pub author: Option<UserId>,
    pub tag: Option<TagId>,
    pub series: Option<SeriesId>,
    pub pinned: Option<bool>,
    pub order: PostOrder,
    pub after: Option<PostCursor>,
    pub limit: u32,
}

impl PostQuery {
    /// No filters, first page; set fields to narrow it.
    #[must_use]
    pub fn new(order: PostOrder, limit: u32) -> Self {
        Self {
            kind: None,
            status: None,
            lang: None,
            author: None,
            tag: None,
            series: None,
            pinned: None,
            order,
            after: None,
            limit,
        }
    }
}

// Workers are single-threaded, so futures need not be `Send` (see plumblog_auth::Crypto).
#[allow(async_fn_in_trait)]
pub trait PostStore {
    /// One atomic write. Insert: `created_at = updated_at = now`, `version = 1`; the group is
    /// the given one (it must be the group of an existing post, else `InvalidInput`) or the
    /// new id.
    /// Update, checked in this order: `PostNotFound` if absent; a trashed post is
    /// `InvalidInput` (restore first), whatever the version; `expected_version` other than
    /// the stored one is `Conflict`, with the stored version in the detail. Then keeps
    /// `created_at` and the group; `updated_at = now`; `version + 1`.
    /// `InvalidInput` (checked before the row is read): status `Trashed`,
    /// scheduled/published without `published_at`, `expected_version` missing on update or
    /// set on insert. `InvalidInput` (after): a missing author, cover, series, tag, body
    /// media or revision author. `Conflict`: `(lang, slug)` or `(group, lang)` taken by
    /// another post. A failed save writes nothing.
    /// Also: replaces tags, body media and the search row; adds the revision if asked; if
    /// the old row was published and its `/<lang>/<slug>` changed, `put_redirect(old, new)`;
    /// if the new status is published, deletes the redirect from the new path (a draft or
    /// scheduled save keeps it, so the old URL works until the post is live); bumps the
    /// content version if the old or new status is published.
    async fn save_post(&self, post: SavePost) -> Result<PostId>;
    /// `PostNotFound`.
    async fn get_post(&self, id: PostId) -> Result<Post>;
    /// Any status.
    async fn find_post(&self, lang: &str, slug: &str) -> Result<Option<Post>>;
    /// Every post of the group, any status, ordered by lang.
    async fn list_translations(&self, group: PostId) -> Result<Vec<PostSummary>>;
    async fn list_posts(&self, query: &PostQuery) -> Result<Vec<PostSummary>>;
    /// `PostNotFound`; `InvalidInput` if already trashed. Keeps the old status in
    /// `trashed_from`; `updated_at = now`; `version + 1`; bumps if it was published.
    async fn trash_post(&self, id: PostId, now: i64) -> Result<()>;
    /// `PostNotFound`; `InvalidInput` if not trashed. Back to `trashed_from`;
    /// `updated_at = now`; `version + 1`; if that is published, deletes the redirect from
    /// its path (it is live again) and bumps.
    async fn restore_post(&self, id: PostId, now: i64) -> Result<()>;
    /// Permanent. `PostNotFound`; `InvalidInput` unless trashed. Deletes its tags, body
    /// media rows (freeing that media for deletion), revisions, comments and search row;
    /// translations are untouched. Never bumps the content version: a trashed post and its
    /// comments are not visible to readers.
    async fn delete_post(&self, id: PostId) -> Result<()>;
    /// Every scheduled post with `published_at <= now` becomes published, in one write;
    /// returns their ids ascending; deletes the redirect from each one's path (it is live
    /// now); bumps once if any. `updated_at` is unchanged; each one's `version + 1`.
    async fn publish_due(&self, now: i64) -> Result<Vec<PostId>>;
    /// Newest first (`created_at` DESC, id DESC); empty for an unknown post.
    async fn list_revisions(&self, post: PostId) -> Result<Vec<Revision>>;
    /// `NotFound`.
    async fn get_revision(&self, id: RevisionId) -> Result<Revision>;
    /// Published posts of `lang` matching every word of `query` (word prefixes, accents and
    /// case ignored), newest first (`published_at` DESC, id DESC). A query without words
    /// returns nothing; user text is never passed to FTS raw (see `search::to_fts_query`).
    async fn search(&self, lang: &str, query: &str, limit: u32) -> Result<Vec<PostSummary>>;
}
