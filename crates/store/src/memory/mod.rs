//! In-memory store for tests. Mirrors D1 exactly, including the schema's unique, CHECK and
//! ON DELETE rules, so the contract suite passes against both.

mod auth;
mod comments;
mod log;
mod media;
mod post_list;
mod post_save;
mod posts;
mod redirects;
mod settings;
mod taxonomy;
mod users;
mod webhooks;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use plumblog_model::{
    CommentId, Error, ErrorCode, MediaId, PostId, Result, RevisionId, SeriesId, TagId, UserId,
};
use plumblog_syslog::Event;

use crate::{
    ApiToken, Comment, Invite, Media, Passkey, PluginState, Post, Redirect, Revision, Series,
    Session, Tag, User, Webhook,
};

/// In-memory store. `RefCell` because traits take `&self` like the D1 impl.
#[derive(Debug, Default)]
pub struct MemoryStore {
    state: RefCell<State>,
}

/// Last id handed out per AUTOINCREMENT table: deleted ids are never reused there.
#[derive(Clone, Debug, Default)]
struct Sequences {
    users: i64,
    media: i64,
    series: i64,
    tags: i64,
    posts: i64,
    revisions: i64,
    comments: i64,
}

/// One value per table; post tags live in `Post::tag_ids`.
#[derive(Clone, Debug)]
struct State {
    content_version: i64,
    seq: Sequences,
    users: BTreeMap<UserId, User>,
    sessions: BTreeMap<i64, Session>,
    passkeys: BTreeMap<i64, Passkey>,
    tokens: BTreeMap<i64, ApiToken>,
    invites: BTreeMap<i64, Invite>,
    media: BTreeMap<MediaId, Media>,
    tags: BTreeMap<TagId, Tag>,
    series: BTreeMap<SeriesId, Series>,
    posts: BTreeMap<PostId, Post>,
    /// `post_media`: media used in post bodies.
    post_media: BTreeSet<(PostId, MediaId)>,
    /// `posts_fts`: folded `(title, body)` per post.
    fts: BTreeMap<PostId, (String, String)>,
    revisions: BTreeMap<RevisionId, Revision>,
    redirects: BTreeMap<String, Redirect>,
    comments: BTreeMap<CommentId, Comment>,
    /// key -> (`value_json`, `updated_at`)
    settings: BTreeMap<String, (String, i64)>,
    /// (`owner_kind` code, `owner_id`, key) -> (`value_json`, `updated_at`)
    options: BTreeMap<(&'static str, String, String), (String, i64)>,
    plugins: BTreeMap<String, PluginState>,
    webhooks: BTreeMap<i64, Webhook>,
    log: BTreeMap<i64, Event>,
}

impl Default for State {
    fn default() -> Self {
        // The migration inserts the site row at version 1.
        Self {
            content_version: 1,
            seq: Sequences::default(),
            users: BTreeMap::new(),
            sessions: BTreeMap::new(),
            passkeys: BTreeMap::new(),
            tokens: BTreeMap::new(),
            invites: BTreeMap::new(),
            media: BTreeMap::new(),
            tags: BTreeMap::new(),
            series: BTreeMap::new(),
            posts: BTreeMap::new(),
            post_media: BTreeSet::new(),
            fts: BTreeMap::new(),
            revisions: BTreeMap::new(),
            redirects: BTreeMap::new(),
            comments: BTreeMap::new(),
            settings: BTreeMap::new(),
            options: BTreeMap::new(),
            plugins: BTreeMap::new(),
            webhooks: BTreeMap::new(),
            log: BTreeMap::new(),
        }
    }
}

impl MemoryStore {
    fn read<T>(&self, f: impl FnOnce(&State) -> T) -> T {
        f(&self.state.borrow())
    }

    /// Runs `f` on a copy and keeps it only on `Ok`, so a failure anywhere leaves no partial
    /// write, like a D1 batch. Copying is fine for a test store.
    fn write<T>(&self, f: impl FnOnce(&mut State) -> Result<T>) -> Result<T> {
        let mut draft = self.state.borrow().clone();
        let out = f(&mut draft)?;
        *self.state.borrow_mut() = draft;
        Ok(out)
    }
}

impl State {
    fn bump(&mut self) {
        self.content_version += 1;
    }

    fn user_exists(&self, id: UserId) -> bool {
        self.users.contains_key(&id)
    }
}

/// Next id of a plain `INTEGER PRIMARY KEY` table: SQLite uses max(rowid) + 1, so ids of
/// deleted rows at the end can come back (only AUTOINCREMENT tables forbid that).
fn next_rowid<V>(map: &BTreeMap<i64, V>) -> i64 {
    map.keys().next_back().map_or(1, |k| k + 1)
}

/// Next id of an AUTOINCREMENT table.
fn next_seq(seq: &mut i64) -> i64 {
    *seq += 1;
    *seq
}

fn count(n: usize) -> u64 {
    u64::try_from(n).unwrap_or(u64::MAX)
}

fn not_found(what: &str, id: impl std::fmt::Display) -> Error {
    Error::new(ErrorCode::NotFound, format!("{what} {id} not found"))
}

fn conflict(detail: impl Into<String>) -> Error {
    Error::new(ErrorCode::Conflict, detail)
}

fn invalid(detail: impl Into<String>) -> Error {
    Error::new(ErrorCode::InvalidInput, detail)
}

fn user_not_found(id: UserId) -> Error {
    Error::new(ErrorCode::UserNotFound, format!("user {} not found", id.0))
}

fn post_not_found(id: PostId) -> Error {
    Error::new(ErrorCode::PostNotFound, format!("post {} not found", id.0))
}

#[cfg(test)]
mod tests;
