//! Search and social data for a page: Open Graph facts, hreflang alternates and JSON-LD
//! (schema.org). Templates decide which tags to print; this builds the values.

use crate::data::{Alternate, Body, PageInfo, Post, Site};
use crate::feeds::{abs, alternates, rfc3339};
use crate::json::J;
use serde::Serialize;

#[derive(Serialize)]
pub struct Seo {
    /// `og:title`: the post title, or the page's own title for lists.
    pub title: String,
    /// `og:type`: `article` for posts, `website` otherwise.
    pub og_type: &'static str,
    /// Absolute URL: the post cover, else the site default image.
    pub image: Option<String>,
    /// RFC 3339, posts only.
    pub published: Option<String>,
    /// Every language version, this page included, plus `x-default`; empty when there is
    /// only one.
    pub alternates: Vec<Alternate>,
    /// `og:locale` in `language_TERRITORY` form (`pt_BR`); `None` when the language has no
    /// known territory, since a wrong value is worse than none.
    pub locale: Option<String>,
    /// A complete JSON document, safe to print inside `<script type="application/ld+json">`.
    pub json_ld: String,
}

/// Absolute URLs and site facts shared by the JSON-LD pieces of one page.
struct Ctx<'a> {
    site: &'a Site,
    page: &'a PageInfo,
    home: String,
    description: &'a str,
}

impl Ctx<'_> {
    fn publisher(&self) -> J {
        J::obj(vec![
            ("@type", Some(J::s("Organization"))),
            ("name", Some(J::s(self.site.title.clone()))),
            ("url", Some(J::s(self.home.clone()))),
        ])
    }

    /// Home first, this page last.
    fn breadcrumbs(&self, name: &str) -> J {
        let items = [
            (self.site.title.clone(), self.home.clone()),
            (name.to_owned(), self.page.canonical.clone()),
        ];
        let list = items
            .into_iter()
            .zip(1..)
            .map(|((name, url), position)| {
                J::obj(vec![
                    ("@type", Some(J::s("ListItem"))),
                    ("position", Some(J::Int(position))),
                    ("name", Some(J::s(name))),
                    ("item", Some(J::s(url))),
                ])
            })
            .collect();
        J::obj(vec![
            ("@type", Some(J::s("BreadcrumbList"))),
            ("itemListElement", Some(J::Arr(list))),
        ])
    }

    fn described(&self) -> Option<J> {
        (!self.description.is_empty()).then(|| J::s(self.description))
    }

    fn article(&self, post: &Post, image: Option<&String>, date: &str) -> J {
        let author = match &self.site.author {
            Some(a) => J::obj(vec![
                ("@type", Some(J::s("Person"))),
                ("name", Some(J::s(a.clone()))),
            ]),
            None => self.publisher(),
        };
        let keywords = post
            .tags
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>();
        J::obj(vec![
            ("@type", Some(J::s("BlogPosting"))),
            ("headline", Some(J::s(post.title.clone()))),
            ("description", self.described()),
            ("url", Some(J::s(self.page.canonical.clone()))),
            ("mainEntityOfPage", Some(J::s(self.page.canonical.clone()))),
            ("datePublished", Some(J::s(date))),
            ("dateModified", Some(J::s(date))),
            ("inLanguage", Some(J::s(post.lang.clone()))),
            ("image", image.map(|i| J::Arr(vec![J::s(i.clone())]))),
            ("author", Some(author)),
            ("publisher", Some(self.publisher())),
            (
                "keywords",
                (!keywords.is_empty()).then(|| J::s(keywords.join(", "))),
            ),
        ])
    }

    fn blog(&self) -> J {
        J::obj(vec![
            ("@type", Some(J::s("Blog"))),
            ("name", Some(J::s(self.site.title.clone()))),
            ("url", Some(J::s(self.home.clone()))),
            ("description", self.described()),
            ("inLanguage", Some(J::s(self.page.lang.clone()))),
            ("publisher", Some(self.publisher())),
        ])
    }

    fn collection(&self, name: &str) -> J {
        J::obj(vec![
            ("@type", Some(J::s("CollectionPage"))),
            ("name", Some(J::s(name))),
            ("url", Some(J::s(self.page.canonical.clone()))),
            ("inLanguage", Some(J::s(self.page.lang.clone()))),
            (
                "isPartOf",
                Some(J::obj(vec![
                    ("@type", Some(J::s("Blog"))),
                    ("url", Some(J::s(self.home.clone()))),
                ])),
            ),
        ])
    }
}

/// `list_title` is the visible title of a tag or archive page (from the theme locale).
pub fn build(
    site: &Site,
    page: &PageInfo,
    body: &Body<'_>,
    list_title: Option<&str>,
    description: &str,
) -> Seo {
    let o = page.origin.as_str();
    let c = Ctx {
        site,
        page,
        home: abs(o, &site.home_url),
        description,
    };
    let site_image = site.image_url.as_deref().map(|i| abs(o, i));
    let mut seo = Seo {
        title: site.title.clone(),
        og_type: "website",
        image: site_image,
        published: None,
        alternates: Vec::new(),
        locale: og_locale(&page.lang),
        json_ld: String::new(),
    };
    let graph = match body {
        Body::Post { post, .. } => {
            if let Some(cover) = &post.cover {
                seo.image = Some(abs(o, &cover.src));
            }
            let date = rfc3339(post.published_at);
            seo.title.clone_from(&post.title);
            seo.og_type = "article";
            seo.alternates = alternates(post, &site.default_lang)
                .into_iter()
                .map(|a| Alternate {
                    lang: a.lang,
                    url: abs(o, &a.url),
                })
                .collect();
            let graph = vec![
                c.article(post, seo.image.as_ref(), &date),
                c.breadcrumbs(&post.title),
            ];
            seo.published = Some(date);
            graph
        }
        Body::Home { .. } => vec![c.blog()],
        Body::Tag { .. } | Body::Archive { .. } => {
            let name = list_title.unwrap_or(&site.title);
            name.clone_into(&mut seo.title);
            vec![c.collection(name), c.breadcrumbs(name)]
        }
        Body::NotFound { .. } => Vec::new(),
    };
    if !graph.is_empty() {
        seo.json_ld = J::obj(vec![
            ("@context", Some(J::s("https://schema.org"))),
            ("@graph", Some(J::Arr(graph))),
        ])
        .to_string();
    }
    seo
}

/// Territory for a tag without one: the language's most common Open Graph locale. Keyed by
/// the lowercase tag, so script tags (`zh-hant`) can pick their own territory.
const DEFAULT_TERRITORY: &[(&str, &str)] = &[
    ("ar", "AR"),
    ("cs", "CZ"),
    ("da", "DK"),
    ("de", "DE"),
    ("el", "GR"),
    ("en", "US"),
    ("es", "ES"),
    ("fa", "IR"),
    ("fi", "FI"),
    ("fr", "FR"),
    ("he", "IL"),
    ("hi", "IN"),
    ("hu", "HU"),
    ("id", "ID"),
    ("it", "IT"),
    ("ja", "JP"),
    ("ko", "KR"),
    ("ms", "MY"),
    ("nb", "NO"),
    ("nl", "NL"),
    ("pl", "PL"),
    ("pt", "PT"),
    ("ro", "RO"),
    ("ru", "RU"),
    ("sv", "SE"),
    ("th", "TH"),
    ("tr", "TR"),
    ("uk", "UA"),
    ("vi", "VN"),
    ("zh", "CN"),
    ("zh-hans", "CN"),
    ("zh-hant", "TW"),
];

/// BCP 47 (`pt-br`, `zh-Hant-TW`, `vi`) to Open Graph's `language_TERRITORY` (`pt_BR`,
/// `zh_TW`, `vi_VN`). A two-letter region wins; otherwise the table decides, and a language
/// it does not know (or a numeric region like `es-419`) gets no tag.
fn og_locale(lang: &str) -> Option<String> {
    let lower = lang.to_ascii_lowercase().replace('_', "-");
    let mut parts = lower.split('-');
    let language = parts.next().filter(|l| !l.is_empty())?;
    let rest: Vec<&str> = parts.collect();
    if rest
        .iter()
        .any(|p| p.len() == 3 && p.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let region = rest
        .iter()
        .find(|p| p.len() == 2 && p.bytes().all(|b| b.is_ascii_alphabetic()))
        .map(|r| r.to_ascii_uppercase())
        .or_else(|| {
            let script = rest.iter().find(|p| p.len() == 4);
            let key = script.map_or_else(|| language.to_owned(), |s| format!("{language}-{s}"));
            DEFAULT_TERRITORY
                .iter()
                .find(|(l, _)| *l == key)
                .map(|(_, t)| (*t).to_owned())
        })?;
    Some(format!("{language}_{region}"))
}

#[cfg(test)]
mod tests {
    use super::og_locale;

    #[test]
    fn og_locale_is_language_underscore_territory() {
        let cases = [
            ("pt-br", Some("pt_BR")),
            ("EN-us", Some("en_US")),
            ("zh-Hant-TW", Some("zh_TW")),
            ("zh-hant", Some("zh_TW")),
            ("vi", Some("vi_VN")),
            ("en", Some("en_US")),
            ("es-419", None),
            ("tlh", None),
            ("", None),
        ];
        for (lang, want) in cases {
            assert_eq!(og_locale(lang).as_deref(), want, "{lang}");
        }
    }
}
