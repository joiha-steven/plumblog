//! Feeds, sitemap, robots.txt and llms.txt: the same posts as the pages, in machine formats.
//! They go through the same versioned cache as pages (see `serve`).

use super::routes::{self, FeedFormat};
use super::source::{Filter, ReaderSource, Settings, SitemapPost};
use super::{Pages, core_text, err, warn};
use crate::keys;
use plumblog_render::feeds::{self, FeedMeta, SitemapUrl};
use plumblog_render::{Alternate, Post};
use std::collections::HashMap;

pub const XML: &str = "application/xml; charset=utf-8";
pub const TEXT: &str = "text/plain; charset=utf-8";

/// Newest posts per feed; readers poll often, so feeds stay small.
const FEED_LIMIT: usize = 20;
/// The sitemap protocol's limit for one file, counting every URL; a bigger site needs a
/// sitemap index (not built yet: past this, the oldest URLs are left out and logged).
const SITEMAP_LIMIT: usize = 50_000;
/// llms.txt is a summary for language models, not a full index; one store page.
const LLMS_LIMIT: usize = 100;

pub fn robots(origin: &str) -> String {
    feeds::robots(origin)
}

impl<S: ReaderSource> Pages<'_, S> {
    fn meta<'m>(
        &'m self,
        s: &'m Settings,
        lang: Option<&'m str>,
        paths: &'m [String; 3],
        newest: Option<i64>,
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
            // No site clock exists: the newest post dates the feed (a fixed date, so the same
            // content version always gives the same bytes), the settings' date when empty.
            updated: newest.unwrap_or(s.updated_at),
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
        let stored = self
            .src
            .feed(lang.unwrap_or(&s.lang), FEED_LIMIT)
            .await
            .map_err(err)?;
        if stored.is_empty() && lang.is_some() {
            return Ok(None);
        }
        let posts = self.render_posts(&stored);
        let refs: Vec<&Post> = posts.iter().collect();
        let paths =
            [FeedFormat::Atom, FeedFormat::Rss, FeedFormat::Json].map(|f| routes::feed(lang, f));
        let m = self.meta(s, lang, &paths, stored.first().map(|p| p.published_at));
        Ok(Some(match format {
            FeedFormat::Atom => feeds::atom(&m, &refs),
            FeedFormat::Rss => feeds::rss(&m, &refs),
            FeedFormat::Json => feeds::json_feed(&m, &refs),
        }))
    }

    /// Home, every post and page (with its language versions), tag and year pages.
    pub(super) async fn sitemap(&self, s: &Settings) -> std::result::Result<String, String> {
        // One read for every post (a dedicated query, not the listing's 100-row pages).
        let posts = self.src.sitemap_posts(SITEMAP_LIMIT).await.map_err(err)?;
        let more = posts.len() >= SITEMAP_LIMIT;
        let url = |p: &SitemapPost| routes::post_in(&p.lang, &s.lang, &p.slug);
        let mut urls = vec![SitemapUrl {
            path: self.site.home_url.clone(),
            lastmod: posts
                .iter()
                .map(|p| p.updated_at)
                .max()
                .or(Some(s.updated_at)),
            alternates: Vec::new(),
        }];
        let alternates = sitemap_alternates(&posts, &s.lang);
        for (p, alternates) in posts.iter().zip(alternates) {
            urls.push(SitemapUrl {
                path: url(p),
                lastmod: Some(p.updated_at),
                alternates,
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
        let urls = capped(urls, more, SITEMAP_LIMIT);
        Ok(feeds::sitemap(&self.info.origin, &urls))
    }

    pub(super) async fn llms(&self, s: &Settings) -> std::result::Result<String, String> {
        let slice = self
            .src
            .posts(Filter::Lang(&s.lang), 0, LLMS_LIMIT)
            .await
            .map_err(err)?;
        let posts = self.render_posts(&slice.posts);
        let refs: Vec<&Post> = posts.iter().collect();
        let paths =
            [FeedFormat::Atom, FeedFormat::Rss, FeedFormat::Json].map(|f| routes::feed(None, f));
        let heading = core_text(&s.lang, keys::LLMS_POSTS);
        let newest = slice.posts.first().map(|p| p.published_at);
        Ok(feeds::llms(
            &self.meta(s, None, &paths, newest),
            &heading,
            &refs,
        ))
    }
}

/// The hreflang set of each post (empty when it has no translation): its group's versions,
/// itself included, plus `x-default` for the default language's. Groups are indexed once, so
/// the cost is linear in the number of posts.
fn sitemap_alternates(posts: &[SitemapPost], default_lang: &str) -> Vec<Vec<Alternate>> {
    let mut groups: HashMap<i64, Vec<&SitemapPost>> = HashMap::new();
    for p in posts {
        groups.entry(p.group).or_default().push(p);
    }
    posts
        .iter()
        .map(|p| {
            let versions = &groups[&p.group];
            if versions.len() < 2 {
                return Vec::new();
            }
            let url = |q: &SitemapPost| routes::post_in(&q.lang, default_lang, &q.slug);
            // This post first, then the others in list order.
            let mut all = vec![Alternate {
                lang: p.lang.clone(),
                url: url(p),
            }];
            all.extend(
                versions
                    .iter()
                    .filter(|q| q.lang != p.lang || q.slug != p.slug)
                    .map(|q| Alternate {
                        lang: q.lang.clone(),
                        url: url(q),
                    }),
            );
            if let Some(d) = all.iter().find(|a| a.lang == default_lang) {
                let x_default = Alternate {
                    lang: "x-default".to_owned(),
                    url: d.url.clone(),
                };
                all.push(x_default);
            }
            all
        })
        .collect()
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
    use super::{SitemapPost, SitemapUrl, capped, sitemap_alternates};

    fn row(lang: &str, slug: &str, group: i64) -> SitemapPost {
        SitemapPost {
            lang: lang.into(),
            slug: slug.into(),
            updated_at: 0,
            group,
        }
    }

    #[test]
    fn sitemap_alternates_link_translations_both_ways() {
        let posts = [row("en", "a", 1), row("vi", "b", 1), row("en", "solo", 2)];
        let alt = sitemap_alternates(&posts, "en");
        let set = |i: usize| -> Vec<(String, String)> {
            alt[i]
                .iter()
                .map(|a| (a.lang.clone(), a.url.clone()))
                .collect()
        };
        let want = |first: (&str, &str), second: (&str, &str)| {
            [first, second, ("x-default", "/a")].map(|(l, u)| (l.to_owned(), u.to_owned()))
        };
        assert_eq!(set(0), want(("en", "/a"), ("vi", "/vi/b")));
        assert_eq!(set(1), want(("vi", "/vi/b"), ("en", "/a")));
        assert!(alt[2].is_empty(), "no translation, no alternates");
    }

    #[test]
    fn sitemap_alternates_stay_linear_at_the_sitemap_limit() {
        // Pairs of translations. Gate on the 4x-input ratio, never on wall-clock (#57): linear
        // work grows about 4x, a per-post scan of the list 16x. At the 50,000 limit that scan
        // would be 2.5e9 steps.
        let posts = |n: i64| -> Vec<SitemapPost> {
            (0..n)
                .map(|i| {
                    row(
                        if i % 2 == 0 { "en" } else { "vi" },
                        &format!("p{i}"),
                        i / 2,
                    )
                })
                .collect()
        };
        let fastest = |list: &[SitemapPost]| {
            (0..5)
                .map(|_| {
                    let t = std::time::Instant::now();
                    let alt = sitemap_alternates(std::hint::black_box(list), "en");
                    assert!(alt.iter().all(|a| a.len() == 3));
                    t.elapsed()
                })
                .min()
                .unwrap_or_default()
        };
        let (small, large) = (posts(12_500), posts(50_000));
        let ratio = fastest(&large).as_secs_f64() / fastest(&small).as_secs_f64();
        assert!(ratio < 8.0, "4x the posts took {ratio:.1}x the time");
    }

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
