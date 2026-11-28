//! Feeds, sitemap, robots.txt and llms.txt: the same posts as the pages, in machine formats.
//! They go through the same versioned cache as pages (see `serve`).

use super::routes::{self, FeedFormat};
use super::source::{Filter, ReaderSource, Settings, SitemapPost, TaxonPage};
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
        updated: Option<i64>,
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
            updated,
        }
    }

    /// The feed's date: its newest post, else the site's last real change (its newest setting
    /// write, one query), else none: the store has no site clock, and 1970 is not a date the
    /// site had. Fixed per content version, so cached bytes never drift.
    async fn feed_date(&self, newest: Option<i64>) -> std::result::Result<Option<i64>, String> {
        match newest {
            Some(t) => Ok(Some(t)),
            None => self.src.settings_updated().await.map_err(err),
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
        let updated = self
            .feed_date(stored.first().map(|p| p.published_at))
            .await?;
        let m = self.meta(s, lang, &paths, updated);
        Ok(Some(match format {
            FeedFormat::Atom => feeds::atom(&m, &refs),
            FeedFormat::Rss => feeds::rss(&m, &refs),
            FeedFormat::Json => feeds::json_feed(&m, &refs),
        }))
    }

    /// Home, every post and page (with its language versions), tag, series and year pages.
    pub(super) async fn sitemap(&self, s: &Settings) -> std::result::Result<String, String> {
        // One read for every post (a dedicated query, not the listing's 100-row pages).
        let posts = self.src.sitemap_posts(SITEMAP_LIMIT).await.map_err(err)?;
        let more = posts.len() >= SITEMAP_LIMIT;
        let url = |p: &SitemapPost| routes::post_in(&p.lang, &s.lang, &p.slug);
        let homes = lang_homes(&posts, s);
        let names: Vec<String> = homes.iter().map(|(l, _)| l.clone()).collect();
        let home_alternates = |lang: &str| super::home_alternates(s, lang, &names);
        let mut urls = vec![SitemapUrl {
            path: self.site.home_url.clone(),
            // None on a site with no posts: no 1970 date.
            lastmod: posts.iter().map(|p| p.updated_at).max(),
            alternates: home_alternates(&s.lang),
        }];
        for (lang, lastmod) in &homes {
            urls.push(SitemapUrl {
                path: routes::lang_home(lang, 1),
                lastmod: Some(*lastmod),
                alternates: home_alternates(lang),
            });
        }
        let alternates = sitemap_alternates(&posts, &s.lang);
        for (p, alternates) in posts.iter().zip(alternates) {
            urls.push(SitemapUrl {
                path: url(p),
                lastmod: Some(p.updated_at),
                alternates,
            });
        }
        let tags = self.src.tag_pages(&s.languages).await.map_err(err)?;
        taxon_urls(&mut urls, s, &tags, routes::tag_in);
        let series = self.src.series_pages(&s.languages).await.map_err(err)?;
        taxon_urls(&mut urls, s, &series, routes::series_in);
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
        let updated = self
            .feed_date(slice.posts.first().map(|p| p.published_at))
            .await?;
        Ok(feeds::llms(
            &self.meta(s, None, &paths, updated),
            &heading,
            &refs,
        ))
    }
}

/// One sitemap URL per tag or series page that exists (a language with posts), each with the
/// hreflang set of the languages that have it.
fn taxon_urls(
    urls: &mut Vec<SitemapUrl>,
    s: &Settings,
    pages: &[TaxonPage],
    url: fn(&str, &str, &str, usize) -> String,
) {
    let mut langs: HashMap<&str, Vec<String>> = HashMap::new();
    for p in pages {
        langs.entry(&p.slug).or_default().push(p.lang.clone());
    }
    for p in pages {
        urls.push(SitemapUrl {
            path: url(&p.lang, &s.lang, &p.slug, 1),
            lastmod: None,
            alternates: super::taxon_alternates(&s.lang, &p.lang, &langs[p.slug.as_str()], |l| {
                url(l, &s.lang, &p.slug, 1)
            }),
        });
    }
}

/// The site's non-default languages that have a published post (a static page makes no home),
/// in the order of `Settings::languages`, each with its newest change. Read off the rows the
/// sitemap already has, so a language home costs no query.
fn lang_homes(posts: &[SitemapPost], s: &Settings) -> Vec<(String, i64)> {
    s.languages
        .iter()
        .filter(|l| **l != s.lang)
        .filter_map(|l| {
            posts
                .iter()
                .filter(|p| !p.is_page && p.lang == *l)
                .map(|p| p.updated_at)
                .max()
                .map(|t| (l.clone(), t))
        })
        .collect()
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
            is_page: false,
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
