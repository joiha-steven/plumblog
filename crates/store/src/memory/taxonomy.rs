use plumblog_model::{Result, SeriesId, TagId};

use super::{MemoryStore, conflict, next_seq, not_found};
use crate::{NewSeries, PostStatus, Series, Tag, TaxonomyStore};

#[expect(
    clippy::unused_async_trait_impl,
    reason = "in-memory stand-in for async D1 storage"
)]
impl TaxonomyStore for MemoryStore {
    async fn ensure_tag(&self, slug: &str, name: &str) -> Result<TagId> {
        self.write(|s| {
            if let Some(t) = s.tags.values().find(|t| t.slug == slug) {
                return Ok(t.id);
            }
            let id = TagId(next_seq(&mut s.seq.tags));
            s.tags.insert(
                id,
                Tag {
                    id,
                    slug: slug.to_owned(),
                    name: name.to_owned(),
                },
            );
            Ok(id)
        })
    }

    async fn update_tag(&self, tag: &Tag) -> Result<()> {
        self.write(|s| {
            if !s.tags.contains_key(&tag.id) {
                return Err(not_found("tag", tag.id.0));
            }
            if s.tags
                .values()
                .any(|t| t.id != tag.id && t.slug == tag.slug)
            {
                return Err(conflict(format!("tag slug {:?} is taken", tag.slug)));
            }
            s.tags.insert(tag.id, tag.clone());
            if s.posts
                .values()
                .any(|p| p.status == PostStatus::Published && p.tag_ids.contains(&tag.id))
            {
                s.bump();
            }
            Ok(())
        })
    }

    async fn find_tag(&self, slug: &str) -> Result<Option<Tag>> {
        Ok(self.read(|s| s.tags.values().find(|t| t.slug == slug).cloned()))
    }

    async fn list_tags(&self) -> Result<Vec<Tag>> {
        Ok(self.read(|s| {
            let mut all: Vec<Tag> = s.tags.values().cloned().collect();
            all.sort_by(|a, b| (&a.name, a.id).cmp(&(&b.name, b.id)));
            all
        }))
    }

    async fn delete_tag(&self, id: TagId) -> Result<()> {
        self.write(|s| {
            s.tags.remove(&id).ok_or_else(|| not_found("tag", id.0))?;
            let mut published = false;
            // post_tags.tag_id is ON DELETE CASCADE.
            for p in s.posts.values_mut().filter(|p| p.tag_ids.contains(&id)) {
                p.tag_ids.retain(|t| *t != id);
                p.version += 1;
                published |= p.status == PostStatus::Published;
            }
            if published {
                s.bump();
            }
            Ok(())
        })
    }

    async fn create_series(&self, series: NewSeries) -> Result<SeriesId> {
        self.write(|s| {
            if s.series.values().any(|x| x.slug == series.slug) {
                return Err(conflict(format!("series slug {:?} is taken", series.slug)));
            }
            let id = SeriesId(next_seq(&mut s.seq.series));
            s.series.insert(
                id,
                Series {
                    id,
                    slug: series.slug,
                    title: series.title,
                    description: series.description,
                },
            );
            Ok(id)
        })
    }

    async fn update_series(&self, series: &Series) -> Result<()> {
        self.write(|s| {
            if !s.series.contains_key(&series.id) {
                return Err(not_found("series", series.id.0));
            }
            if s.series
                .values()
                .any(|x| x.id != series.id && x.slug == series.slug)
            {
                return Err(conflict(format!("series slug {:?} is taken", series.slug)));
            }
            s.series.insert(series.id, series.clone());
            if s.posts
                .values()
                .any(|p| p.status == PostStatus::Published && p.series_id == Some(series.id))
            {
                s.bump();
            }
            Ok(())
        })
    }

    async fn find_series(&self, slug: &str) -> Result<Option<Series>> {
        Ok(self.read(|s| s.series.values().find(|x| x.slug == slug).cloned()))
    }

    async fn list_series(&self) -> Result<Vec<Series>> {
        Ok(self.read(|s| {
            let mut all: Vec<Series> = s.series.values().cloned().collect();
            all.sort_by(|a, b| (&a.title, a.id).cmp(&(&b.title, b.id)));
            all
        }))
    }

    async fn delete_series(&self, id: SeriesId) -> Result<()> {
        self.write(|s| {
            s.series
                .remove(&id)
                .ok_or_else(|| not_found("series", id.0))?;
            let mut published = false;
            // series_id is ON DELETE SET NULL; the position means nothing without it.
            for p in s.posts.values_mut().filter(|p| p.series_id == Some(id)) {
                p.series_id = None;
                p.series_position = None;
                p.version += 1;
                published |= p.status == PostStatus::Published;
            }
            if published {
                s.bump();
            }
            Ok(())
        })
    }
}
