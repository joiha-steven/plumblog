//! `save_post`: validation first, then every row the save touches.

use plumblog_model::{Error, PostId, Result, RevisionId};

use super::{State, conflict, invalid, next_seq, post_not_found};
use crate::{NewRevision, Post, PostStatus, Revision, SavePost, search};

/// The `Conflict` of a save from a version that is no longer stored.
pub(super) fn stale_version(id: PostId, current: i64, expected: Option<i64>) -> Error {
    conflict(format!(
        "post {} is at version {current}, not {}; reload it and edit again",
        id.0,
        expected.unwrap_or_default()
    ))
}

/// Reader URL of a post; redirects are keyed by it.
pub(super) fn post_path(lang: &str, slug: &str) -> String {
    format!("/{lang}/{slug}")
}

/// Checks that need no stored row; D1 makes them before any query.
pub(super) fn check_post_shape(input: &SavePost) -> Result<()> {
    if input.status == PostStatus::Trashed {
        return Err(invalid("save_post cannot trash; use trash_post"));
    }
    if matches!(input.status, PostStatus::Scheduled | PostStatus::Published)
        && input.published_at.is_none()
    {
        return Err(invalid(format!(
            "a {} post needs published_at",
            input.status.as_str()
        )));
    }
    match (input.id, input.expected_version) {
        (None, Some(_)) => Err(invalid("a new post has no version to expect")),
        (Some(id), None) => Err(invalid(format!(
            "updating post {} needs the version it edited",
            id.0
        ))),
        _ => Ok(()),
    }
}

impl State {
    /// Every FK the `posts`, `post_tags`, `post_media` and `revisions` inserts would hit.
    fn check_post_refs(&self, input: &SavePost) -> Result<()> {
        if !self.user_exists(input.author_id) {
            return Err(invalid(format!(
                "author {} does not exist",
                input.author_id.0
            )));
        }
        if let Some(m) = input.cover_media_id
            && !self.media.contains_key(&m)
        {
            return Err(invalid(format!("cover media {} does not exist", m.0)));
        }
        if let Some(x) = input.series_id
            && !self.series.contains_key(&x)
        {
            return Err(invalid(format!("series {} does not exist", x.0)));
        }
        if let Some(t) = input.tag_ids.iter().find(|t| !self.tags.contains_key(t)) {
            return Err(invalid(format!("tag {} does not exist", t.0)));
        }
        if let Some(m) = input.media_ids.iter().find(|m| !self.media.contains_key(m)) {
            return Err(invalid(format!("media {} does not exist", m.0)));
        }
        if let Some(u) = input.revision.and_then(|r| r.author_id)
            && !self.user_exists(u)
        {
            return Err(invalid(format!("revision author {} does not exist", u.0)));
        }
        Ok(())
    }

    pub(super) fn save_post(&mut self, input: SavePost) -> Result<PostId> {
        check_post_shape(&input)?;
        let old = match input.id {
            Some(id) => {
                let old = self.posts.get(&id).ok_or_else(|| post_not_found(id))?;
                if old.status == PostStatus::Trashed {
                    return Err(invalid(format!(
                        "post {} is trashed; restore it first",
                        id.0
                    )));
                }
                if input.expected_version != Some(old.version) {
                    return Err(stale_version(id, old.version, input.expected_version));
                }
                Some(old.clone())
            }
            None => None,
        };
        self.check_post_refs(&input)?;
        if old.is_none()
            && let Some(g) = input.translation_group
            && !self.posts.values().any(|p| p.translation_group == g)
        {
            return Err(invalid(format!("translation group {} does not exist", g.0)));
        }
        // Only reserved once the input is valid; `write` discards it on a later error anyway.
        let id = input
            .id
            .unwrap_or_else(|| PostId(next_seq(&mut self.seq.posts)));
        let group = old
            .as_ref()
            .map(|o| o.translation_group)
            .or(input.translation_group)
            .unwrap_or(id);
        for p in self.posts.values().filter(|p| p.id != id) {
            if p.lang == input.lang && p.slug == input.slug {
                return Err(conflict(format!(
                    "{} is taken by post {}",
                    post_path(&input.lang, &input.slug),
                    p.id.0
                )));
            }
            if p.translation_group == group && p.lang == input.lang {
                return Err(conflict(format!(
                    "translation group {} already has {:?} (post {})",
                    group.0, input.lang, p.id.0
                )));
            }
        }

        let mut tag_ids = input.tag_ids;
        tag_ids.sort_unstable();
        tag_ids.dedup();
        let post = Post {
            id,
            kind: input.kind,
            translation_group: group,
            lang: input.lang,
            slug: input.slug,
            status: input.status,
            trashed_from: None,
            published_at: input.published_at,
            author_id: input.author_id,
            title: input.title,
            markdown: input.markdown,
            html: input.html,
            toc_json: input.toc_json,
            excerpt: input.excerpt,
            word_count: input.word_count,
            reading_minutes: input.reading_minutes,
            pinned: input.pinned,
            comments_enabled: input.comments_enabled,
            cover_media_id: input.cover_media_id,
            series_id: input.series_id,
            series_position: input.series_position,
            created_at: old.as_ref().map_or(input.now, |o| o.created_at),
            updated_at: input.now,
            version: old.as_ref().map_or(1, |o| o.version + 1),
            tag_ids,
        };
        self.index_and_link(
            &post,
            old.as_ref(),
            &input.search_text,
            input.revision,
            input.now,
        )?;
        self.posts.insert(id, post);
        self.post_media.retain(|(p, _)| *p != id);
        self.post_media
            .extend(input.media_ids.into_iter().map(|m| (id, m)));
        Ok(id)
    }

    /// Rows that follow a saved post: search text, revision, redirects, version.
    fn index_and_link(
        &mut self,
        post: &Post,
        old: Option<&Post>,
        search_text: &str,
        revision: Option<NewRevision>,
        now: i64,
    ) -> Result<()> {
        let id = post.id;
        self.fts
            .insert(id, (search::fold(&post.title), search::fold(search_text)));
        if let Some(r) = revision {
            let rid = RevisionId(next_seq(&mut self.seq.revisions));
            self.revisions.insert(
                rid,
                Revision {
                    id: rid,
                    post_id: id,
                    author_id: r.author_id,
                    title: post.title.clone(),
                    markdown: post.markdown.clone(),
                    created_at: r.created_at,
                },
            );
        }
        let new_path = post_path(&post.lang, &post.slug);
        let was_published = old.is_some_and(|o| o.status == PostStatus::Published);
        if let Some(o) = old.filter(|_| was_published) {
            let old_path = post_path(&o.lang, &o.slug);
            if old_path != new_path {
                self.put_redirect(&old_path, &new_path, now)?;
            }
        }
        // A live path must not redirect away, but a draft must not turn a working old URL
        // into a 404 either: only a published save takes the path over.
        if post.status == PostStatus::Published {
            self.redirects.remove(&new_path);
        }
        if was_published || post.status == PostStatus::Published {
            self.bump();
        }
        Ok(())
    }
}
