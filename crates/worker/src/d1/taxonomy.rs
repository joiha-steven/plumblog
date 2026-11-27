use plumblog_model::{Result, SeriesId, TagId};
use plumblog_store::{NewSeries, Series, Tag, TaxonomyStore};

use super::{D1Store, Row, bound, bump_if, not_found, storage, text};

const TAG_COLS: &str = "id, slug, name";
const SERIES_COLS: &str = "id, slug, title, description";

/// True when a published post has the tag `?`.
const TAG_IS_PUBLISHED: &str = "EXISTS (SELECT 1 FROM post_tags pt JOIN posts p ON p.id = pt.post_id \
     WHERE pt.tag_id = ? AND p.status = 'published')";
/// True when a published post is in the series `?`.
const SERIES_IS_PUBLISHED: &str =
    "EXISTS (SELECT 1 FROM posts WHERE series_id = ? AND status = 'published')";

fn read_tag(r: &mut Row) -> Result<Tag> {
    Ok(Tag {
        id: TagId(r.int()?),
        slug: r.text()?,
        name: r.text()?,
    })
}

fn read_series(r: &mut Row) -> Result<Series> {
    Ok(Series {
        id: SeriesId(r.int()?),
        slug: r.text()?,
        title: r.text()?,
        description: r.text()?,
    })
}

impl TaxonomyStore for D1Store {
    async fn ensure_tag(&self, slug: &str, name: &str) -> Result<TagId> {
        // One statement: insert, or on a taken slug touch nothing (the no-op update only
        // exists so RETURNING yields the existing id; the name is kept).
        let s = self.stmt(
            "ensure_tag",
            "INSERT INTO tags (slug, name) VALUES (?, ?) \
             ON CONFLICT (slug) DO UPDATE SET slug = excluded.slug RETURNING id",
            &[text(slug), text(name)],
        )?;
        self.row(s)
            .await?
            .ok_or_else(|| storage("ensure_tag", "upsert returned no row"))?
            .int()
            .map(TagId)
    }

    async fn update_tag(&self, tag: &Tag) -> Result<()> {
        let up = self.stmt(
            "update_tag",
            "UPDATE tags SET slug = ?, name = ? WHERE id = ?",
            &[text(&tag.slug), text(&tag.name), bound(tag.id.0)],
        )?;
        // A missing tag has no post links, so this bump is a no-op then.
        let bump = bump_if(self, TAG_IS_PUBLISHED, &[bound(tag.id.0)])?;
        let results = self.batch("update_tag", vec![up, bump]).await?;
        if Self::changes("update_tag", &results[0])? == 0 {
            return Err(not_found("tag", tag.id.0));
        }
        Ok(())
    }

    // 1 query.
    async fn find_tag(&self, slug: &str) -> Result<Option<Tag>> {
        let s = self.stmt(
            "find_tag",
            &format!("SELECT {TAG_COLS} FROM tags WHERE slug = ?"),
            &[text(slug)],
        )?;
        self.row(s).await?.map(|mut r| read_tag(&mut r)).transpose()
    }

    // 1 query.
    async fn list_tags(&self) -> Result<Vec<Tag>> {
        let s = self.stmt(
            "list_tags",
            &format!("SELECT {TAG_COLS} FROM tags ORDER BY name, id"),
            &[],
        )?;
        self.rows(s).await?.iter_mut().map(read_tag).collect()
    }

    async fn delete_tag(&self, id: TagId) -> Result<()> {
        // Bump while the links still exist; the FK cascade then removes them.
        let bump = bump_if(self, TAG_IS_PUBLISHED, &[bound(id.0)])?;
        let del = self.stmt(
            "delete_tag",
            "DELETE FROM tags WHERE id = ?",
            &[bound(id.0)],
        )?;
        let results = self.batch("delete_tag", vec![bump, del]).await?;
        if Self::changes("delete_tag", &results[1])? == 0 {
            return Err(not_found("tag", id.0));
        }
        Ok(())
    }

    async fn create_series(&self, series: NewSeries) -> Result<SeriesId> {
        let s = self.stmt(
            "create_series",
            "INSERT INTO series (slug, title, description) VALUES (?, ?, ?)",
            &[
                text(&series.slug),
                text(&series.title),
                text(&series.description),
            ],
        )?;
        let r = self.exec(s).await?;
        Self::last_row_id("create_series", &r).map(SeriesId)
    }

    async fn update_series(&self, series: &Series) -> Result<()> {
        let up = self.stmt(
            "update_series",
            "UPDATE series SET slug = ?, title = ?, description = ? WHERE id = ?",
            &[
                text(&series.slug),
                text(&series.title),
                text(&series.description),
                bound(series.id.0),
            ],
        )?;
        let bump = bump_if(self, SERIES_IS_PUBLISHED, &[bound(series.id.0)])?;
        let results = self.batch("update_series", vec![up, bump]).await?;
        if Self::changes("update_series", &results[0])? == 0 {
            return Err(not_found("series", series.id.0));
        }
        Ok(())
    }

    // 1 query.
    async fn find_series(&self, slug: &str) -> Result<Option<Series>> {
        let s = self.stmt(
            "find_series",
            &format!("SELECT {SERIES_COLS} FROM series WHERE slug = ?"),
            &[text(slug)],
        )?;
        self.row(s)
            .await?
            .map(|mut r| read_series(&mut r))
            .transpose()
    }

    // 1 query.
    async fn list_series(&self) -> Result<Vec<Series>> {
        let s = self.stmt(
            "list_series",
            &format!("SELECT {SERIES_COLS} FROM series ORDER BY title, id"),
            &[],
        )?;
        self.rows(s).await?.iter_mut().map(read_series).collect()
    }

    async fn delete_series(&self, id: SeriesId) -> Result<()> {
        // Bump, clear positions and bump post versions while posts still point at it; the
        // FK then sets series_id NULL. The position means nothing without the series, and
        // the version bump makes an editor holding the old post reload.
        let bump = bump_if(self, SERIES_IS_PUBLISHED, &[bound(id.0)])?;
        let clear = self.stmt(
            "delete_series_positions",
            "UPDATE posts SET series_position = NULL, version = version + 1 WHERE series_id = ?",
            &[bound(id.0)],
        )?;
        let del = self.stmt(
            "delete_series",
            "DELETE FROM series WHERE id = ?",
            &[bound(id.0)],
        )?;
        let results = self.batch("delete_series", vec![bump, clear, del]).await?;
        if Self::changes("delete_series", &results[2])? == 0 {
            return Err(not_found("series", id.0));
        }
        Ok(())
    }
}
