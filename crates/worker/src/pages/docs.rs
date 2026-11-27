//! Feeds, sitemap, robots.txt and llms.txt: the same posts as the pages, in machine formats.
//! They go through the same versioned cache as pages (see `serve`).

use super::routes::{self, FeedFormat};
use super::source::{Filter, ReaderSource, Settings};
use super::{Pages, core_text, err, to_render, warn};
use crate::keys;
use plumblog_render::Post;
use plumblog_render::feeds::{self, FeedMeta, SitemapUrl};

pub const XML: &str = "application/xml; charset=utf-8";
pub const TEXT: &str = "text/plain; charset=utf-8";

/// Newest posts per feed; readers poll often, so feeds stay small.
const FEED_LIMIT: usize = 20;
/// The sitemap protocol's limit for one file, counting every URL; a bigger site needs a
/// sitemap index (not built yet: past this, the oldest URLs are left out and logged).
const SITEMAP_LIMIT: usize = 50_000;
/// llms.txt is a summary for language models, not a full index.
const LLMS_LIMIT: usize = 200;

pub fn robots(origin: &str) -> String {
    feeds::robots(origin)
}

impl<S: ReaderSource> Pages<'_, S> {
    fn meta<'m>(
        &'m self,
        s: &'m Settings,
        lang: Option<&'m str>,
        paths: &'m [String; 3],
    ) -> FeedMeta<'m> {
        FeedMeta {
            title: &s.title,
            description: &s.description,
            lang: lang.unwrap_or(&s.lang),
            origin: &self.info.origin,
            home_path: &self.site.home_url,
            atom_path: &paths[0],
            rss_path: &paths[1],
            json_path: &paths[2],
            author: self.site.author.as_deref(),
            updated: s.updated_at,
        }
    }

    /// `None` when the language has no feed: no posts in it, or it is the default language
    /// asked for under `/<lang>/` (its feeds live at the root, one URL each).
    pub(super) async fn feed(
        &self,
        s: &Settings,
        lang: Option<&str>,
        format: FeedFormat,
    ) -> std::result::Result<Option<String>, String> {
        if lang == Some(s.lang.as_str()) {
            return Ok(None);
        }
        let slice = self
            .src
            .posts(Filter::Lang(lang.unwrap_or(&s.lang)), 0, FEED_LIMIT)
            .await
            .map_err(err)?;
        if slice.posts.is_empty() && lang.is_some() {
            return Ok(None);
        }
        let posts: Vec<Post> = slice.posts.iter().map(to_render).collect();
        let refs: Vec<&Post> = posts.iter().collect();
        let paths =
            [FeedFormat::Atom, FeedFormat::Rss, FeedFormat::Json].map(|f| routes::feed(lang, f));
        let m = self.meta(s, lang, &paths);
        Ok(Some(match format {
            FeedFormat::Atom => feeds::atom(&m, &refs),
            FeedFormat::Rss => feeds::rss(&m, &refs),
            FeedFormat::Json => feeds::json_feed(&m, &refs),
        }))
    }

    /// Home, every post (with its language versions), tag and year pages.
    pub(super) async fn sitemap(&self, s: &Settings) -> std::result::Result<String, String> {
        let slice = self
            .src
            .posts(Filter::All, 0, SITEMAP_LIMIT)
            .await
            .map_err(err)?;
        let newest = slice.posts.iter().map(|p| p.published_at).max();
        let mut urls = vec![SitemapUrl {
            path: self.site.home_url.clone(),
            lastmod: newest,
            alternates: Vec::new(),
        }];
        for p in &slice.posts {
            let r = to_render(p);
            urls.push(SitemapUrl {
                alternates: feeds::alternates(&r, &s.lang),
                path: r.url,
                lastmod: Some(p.published_at),
            });
        }
        for t in self.src.tags().await.map_err(err)? {
            urls.push(SitemapUrl {
                path: routes::tag_page(&t.slug, 1),
                lastmod: None,
                alternates: Vec::new(),
            });
        }
        for y in self.src.years().await.map_err(err)? {
            urls.push(SitemapUrl {
                path: routes::archive(y, 1),
                lastmod: None,
                alternates: Vec::new(),
            });
        }
        let urls = capped(urls, slice.more, SITEMAP_LIMIT);
        Ok(feeds::sitemap(&self.info.origin, &urls))
    }

    pub(super) async fn llms(&self, s: &Settings) -> std::result::Result<String, String> {
        let slice = self
            .src
            .posts(Filter::Lang(&s.lang), 0, LLMS_LIMIT)
            .await
            .map_err(err)?;
        let posts: Vec<Post> = slice.posts.iter().map(to_render).collect();
        let refs: Vec<&Post> = posts.iter().collect();
        let paths =
            [FeedFormat::Atom, FeedFormat::Rss, FeedFormat::Json].map(|f| routes::feed(None, f));
        let heading = core_text(&s.lang, keys::LLMS_POSTS);
        Ok(feeds::llms(&self.meta(s, None, &paths), &heading, &refs))
    }
}

/// At most `limit` URLs; anything left out (including posts past the query limit, `more`) is
/// logged so the owner learns the site needs a sitemap index.
fn capped(mut urls: Vec<SitemapUrl>, more: bool, limit: usize) -> Vec<SitemapUrl> {
    if urls.len() > limit || more {
        let dropped = urls.len().saturating_sub(limit);
        urls.truncate(limit);
        warn(&format!(
            "sitemap.truncated limit={limit} dropped={dropped} more_posts={more}: a sitemap index is needed"
        ));
    }
    urls
}

#[cfg(test)]
mod tests {
    use super::{SitemapUrl, capped};

    #[test]
    fn sitemap_never_exceeds_the_limit() {
        let urls = |n: usize| {
            (0..n)
                .map(|i| SitemapUrl {
                    path: format!("/{i}"),
                    lastmod: None,
                    alternates: Vec::new(),
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(capped(urls(3), false, 3).len(), 3);
        let cut = capped(urls(5), false, 3);
        assert_eq!(
            cut.iter().map(|u| u.path.as_str()).collect::<Vec<_>>(),
            ["/0", "/1", "/2"],
            "home and newest posts are kept"
        );
        assert_eq!(capped(urls(3), true, 3).len(), 3);
    }
}
