//! The reader's posts and site settings, read from the store (D1 in the Worker, the in-memory
//! store in host tests). Query counts per method are in docs/ARCHITECTURE.md (Storage).

use super::db::Db;
use super::routes::is_lang;
use super::source::{
    Filter, Neighbours, ReaderSource, Settings, SitemapPost, Slice, StoredPost, Tag,
};
use super::{core_text, warn};
use crate::api::body::json_string;
use crate::keys;
use plumblog_markdown::Rendered;
use plumblog_model::{PostId, Result, TagId};
use plumblog_store::{
    ORIGIN_KEY, PostKind, PostSummary, ReaderFilter, ReaderLink, ReaderPost, ReaderStore,
    ReaderTag, SettingsStore, TaxonomyStore,
};

/// Site settings keys (JSON values). Every one has a default, so a fresh site renders.
pub const SITE_TITLE_KEY: &str = "site_title";
pub const SITE_DESCRIPTION_KEY: &str = "site_description";
pub const SITE_FOOTER_KEY: &str = "site_footer";
/// The default language: its posts live at `/<slug>`.
pub const SITE_LANG_KEY: &str = "site_lang";
/// JSON array of language codes the site offers.
pub const SITE_LANGUAGES_KEY: &str = "site_languages";
/// Posts per listing page, a JSON integer.
pub const SITE_PER_PAGE_KEY: &str = "site_per_page";

const DEFAULT_LANG: &str = "en";
const DEFAULT_PER_PAGE: usize = 10;
const MAX_PER_PAGE: usize = 50;

/// [`Settings`] from the `(key, value_json)` rows; a missing or malformed value falls back
/// to its default (the bad ones are logged), so no stored value can break a page.
pub fn parse_settings(rows: &[(String, String)]) -> Settings {
    let raw = |key: &str| rows.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
    let text = |key: &str| {
        raw(key).and_then(|v| {
            let s = json_string(v);
            if s.is_none() {
                warn(&format!("settings.bad_value key={key}: expected a string"));
            }
            s
        })
    };
    let mut lang_set = false;
    let lang = match text(SITE_LANG_KEY) {
        Some(l) if is_lang(&l) => {
            lang_set = true;
            l
        }
        Some(l) => {
            warn(&format!(
                "settings.bad_value key={SITE_LANG_KEY} value={l:?}"
            ));
            DEFAULT_LANG.to_owned()
        }
        None => DEFAULT_LANG.to_owned(),
    };
    let mut languages = vec![lang.clone()];
    if let Some(v) = raw(SITE_LANGUAGES_KEY) {
        for item in array_items(v) {
            match json_string(item).filter(|l| is_lang(l)) {
                Some(l) if !languages.contains(&l) => languages.push(l),
                Some(_) => {}
                None => warn(&format!(
                    "settings.bad_value key={SITE_LANGUAGES_KEY} item={item}"
                )),
            }
        }
    }
    let per_page = raw(SITE_PER_PAGE_KEY).map_or(DEFAULT_PER_PAGE, |v| {
        v.trim().parse::<usize>().map_or_else(
            |_| {
                warn(&format!(
                    "settings.bad_value key={SITE_PER_PAGE_KEY} value={v}"
                ));
                DEFAULT_PER_PAGE
            },
            |n| n.clamp(1, MAX_PER_PAGE),
        )
    });
    Settings {
        title: text(SITE_TITLE_KEY)
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| core_text(&lang, keys::SITE_TITLE_FALLBACK)),
        description: text(SITE_DESCRIPTION_KEY).unwrap_or_default(),
        footer: text(SITE_FOOTER_KEY).unwrap_or_default(),
        lang,
        lang_set,
        languages,
        per_page,
        origin: text(ORIGIN_KEY).filter(|o| !o.is_empty()),
    }
}

/// The elements of a JSON array of strings, as JSON text each. Language codes hold no commas
/// or escapes, so splitting on commas is enough; anything odd fails the per-item check.
fn array_items(value_json: &str) -> Vec<&str> {
    let inner = value_json
        .trim()
        .strip_prefix('[')
        .and_then(|v| v.strip_suffix(']'))
        .unwrap_or("");
    inner
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect()
}

fn tags(tags: Vec<ReaderTag>) -> Vec<Tag> {
    tags.into_iter()
        .map(|t| Tag {
            id: TagId(0),
            slug: t.slug,
            name: t.name,
        })
        .collect()
}

/// A listing row; its body holds only what lists show (no HTML).
fn from_summary(s: PostSummary, row_tags: Vec<ReaderTag>) -> StoredPost {
    StoredPost {
        id: s.id,
        slug: s.slug,
        title: s.title,
        lang: s.lang,
        // The store keeps milliseconds; pages use seconds.
        published_at: s.published_at.unwrap_or(s.created_at).div_euclid(1000),
        pinned: s.pinned,
        is_page: s.kind == PostKind::Page,
        comments_enabled: s.comments_enabled,
        // The store keeps one excerpt, written or automatic, so it is always the "stored"
        // one the theme cuts to `excerpt_words`.
        excerpt: None,
        body: Rendered {
            excerpt: s.excerpt,
            reading_minutes: u32::try_from(s.reading_minutes).unwrap_or(0),
            ..Rendered::default()
        },
        tags: tags(row_tags),
        translations: Vec::new(),
    }
}

/// A post with its body. The asset flags are read off the HTML, which the renderer marks
/// (`div.code`, `<math>`, `div.embed`); only the post page uses them.
fn from_full(r: ReaderPost) -> StoredPost {
    let p = r.post;
    let body = Rendered {
        has_code: p.html.contains("<div class=\"code\""),
        has_math: p.html.contains("<math"),
        has_embed: p.html.contains("<div class=\"embed\""),
        reading_minutes: u32::try_from(p.reading_minutes).unwrap_or(0),
        excerpt: p.excerpt,
        html: p.html,
        ..Rendered::default()
    };
    StoredPost {
        id: p.id,
        slug: p.slug,
        title: p.title,
        lang: p.lang,
        published_at: p.published_at.unwrap_or(p.created_at).div_euclid(1000),
        pinned: p.pinned,
        is_page: p.kind == PostKind::Page,
        comments_enabled: p.comments_enabled,
        excerpt: None,
        body,
        tags: tags(r.tags),
        translations: Vec::new(),
    }
}

/// A neighbour: pages show only its title and link.
fn from_link(l: ReaderLink) -> StoredPost {
    StoredPost {
        id: PostId(0),
        slug: l.slug,
        title: l.title,
        lang: l.lang,
        published_at: 0,
        pinned: false,
        is_page: false,
        comments_enabled: false,
        excerpt: None,
        body: Rendered::default(),
        tags: Vec::new(),
        translations: Vec::new(),
    }
}

fn clamp(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

impl<S: SettingsStore + ReaderStore + TaxonomyStore> ReaderSource for Db<'_, S> {
    // 1 query.
    async fn settings(&self) -> Result<Settings> {
        Ok(parse_settings(&self.0.all_settings().await?))
    }

    // 1 query.
    async fn settings_updated(&self) -> Result<Option<i64>> {
        Ok(self
            .0
            .settings_updated_at()
            .await?
            .map(|ms| ms.div_euclid(1000)))
    }

    // 2 queries (count, rows); 1 when the page is past the end or the offset cap.
    async fn posts(&self, filter: Filter<'_>, offset: usize, limit: usize) -> Result<Slice> {
        let f = match filter {
            Filter::Home(l) => ReaderFilter::Home(l),
            Filter::Tag(t) => ReaderFilter::Tag(t.id),
            Filter::Year(y) => ReaderFilter::Year(y),
            Filter::Lang(l) => ReaderFilter::Lang(l),
        };
        let page = self.0.reader_posts(f, clamp(offset), clamp(limit)).await?;
        Ok(Slice {
            total: usize::try_from(page.total).unwrap_or(usize::MAX),
            posts: page
                .rows
                .into_iter()
                .map(|r| from_summary(r.post, r.tags))
                .collect(),
        })
    }

    // 1 query when there is no such post; else 3 (post, translations, neighbours); 2 for a
    // page, which has no neighbours.
    async fn post(&self, lang: &str, slug: &str) -> Result<Option<Neighbours>> {
        let Some(found) = self.0.reader_post(lang, slug).await? else {
            return Ok(None);
        };
        let group = found.post.translation_group;
        let is_post = found.post.kind == PostKind::Post;
        let mut post = from_full(found);
        post.translations = self
            .0
            .reader_translations(group)
            .await?
            .into_iter()
            .filter(|l| !(l.lang == post.lang && l.slug == post.slug))
            .map(|l| (l.lang, l.slug))
            .collect();
        let (older, newer) = if is_post {
            let n = self.0.reader_neighbours(post.id).await?;
            (n.older.map(from_link), n.newer.map(from_link))
        } else {
            (None, None)
        };
        Ok(Some(Neighbours { post, older, newer }))
    }

    // 1 query.
    async fn feed(&self, lang: &str, limit: usize) -> Result<Vec<StoredPost>> {
        Ok(self
            .0
            .reader_feed(lang, clamp(limit))
            .await?
            .into_iter()
            .map(from_full)
            .collect())
    }

    // 1 query.
    async fn sitemap_posts(&self, limit: usize) -> Result<Vec<SitemapPost>> {
        Ok(self
            .0
            .reader_sitemap(clamp(limit))
            .await?
            .into_iter()
            .map(|r| SitemapPost {
                lang: r.lang,
                slug: r.slug,
                updated_at: r.updated_at.div_euclid(1000),
                group: r.translation_group.0,
                is_page: r.is_page,
            })
            .collect())
    }

    // 1 query.
    async fn tag(&self, slug: &str) -> Result<Option<Tag>> {
        Ok(self.0.find_tag(slug).await?.map(|t| Tag {
            id: t.id,
            slug: t.slug,
            name: t.name,
        }))
    }

    // 1 query.
    async fn tags(&self) -> Result<Vec<Tag>> {
        Ok(self
            .0
            .reader_tags()
            .await?
            .into_iter()
            .map(|t| Tag {
                id: t.id,
                slug: t.slug,
                name: t.name,
            })
            .collect())
    }

    // 1 query.
    async fn years(&self) -> Result<Vec<i32>> {
        self.0.reader_years().await
    }

    // 1 query.
    async fn post_exists(&self, lang: &str, slug: &str) -> Result<bool> {
        self.0.reader_post_exists(lang, slug).await
    }

    // 1 query.
    async fn langs_with_posts(&self, langs: &[String]) -> Result<Vec<String>> {
        self.0.reader_langs_with_posts(langs).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    #[test]
    fn an_empty_site_has_defaults() {
        let s = parse_settings(&[]);
        assert_eq!(s.lang, "en");
        assert_eq!(s.languages, ["en"]);
        assert_eq!(s.per_page, 10);
        assert_eq!(s.title, "Untitled site");
        assert_eq!(s.description, "");
        assert!(s.origin.is_none());
    }

    #[test]
    fn stored_values_are_read_and_bad_ones_fall_back() {
        let s = parse_settings(&rows(&[
            (SITE_TITLE_KEY, "\"Plum\""),
            (SITE_LANG_KEY, "\"vi\""),
            (SITE_LANGUAGES_KEY, "[\"en\", \"vi\", \"EN!\", \"en\"]"),
            (SITE_PER_PAGE_KEY, "500"),
        ]));
        assert_eq!(s.title, "Plum");
        assert_eq!(s.lang, "vi");
        assert_eq!(
            s.languages,
            ["vi", "en"],
            "default first, bad and repeats dropped"
        );
        assert_eq!(s.per_page, 50, "clamped");
        let s = parse_settings(&rows(&[
            (SITE_LANG_KEY, "\"NOT A LANG\""),
            (SITE_PER_PAGE_KEY, "\"ten\""),
            (SITE_TITLE_KEY, "\"  \""),
        ]));
        assert_eq!((s.lang.as_str(), s.per_page), ("en", 10));
        assert_eq!(s.title, "Untitled site");
        assert_eq!(
            parse_settings(&rows(&[(SITE_PER_PAGE_KEY, "0")])).per_page,
            1
        );
    }
}
