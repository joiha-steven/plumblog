//! Test and dev builds only: copies the sample site (settings, posts, tags, translations)
//! into a store, so reader pages and search read the same site the sample pages show. Posts
//! are saved through `save_post`, the same path the editor will use, so the search index is
//! built (and d-stroke folded) for real.

use super::reader_db::{
    SITE_DESCRIPTION_KEY, SITE_FOOTER_KEY, SITE_LANG_KEY, SITE_LANGUAGES_KEY, SITE_PER_PAGE_KEY,
    SITE_TITLE_KEY,
};
use super::sample::{SAMPLE_SITE, SampleSource};
use super::source::{Filter, ReaderSource};
use crate::api::body::quote;
use plumblog_model::{PostId, Role};
use plumblog_store::{
    NewUser, ORIGIN_KEY, PostKind, PostStatus, PostStore, SavePost, SettingsStore, TaxonomyStore,
    UserStore,
};
use std::collections::HashMap;

/// Writes the sample site's settings, as `PUT /api/v1/settings/<key>` would.
async fn sample_settings<S: SettingsStore>(db: &S) -> Result<(), String> {
    let languages: Vec<String> = SAMPLE_SITE.languages.iter().map(|l| quote(l)).collect();
    let rows = [
        (SITE_TITLE_KEY, quote(SAMPLE_SITE.title)),
        (SITE_DESCRIPTION_KEY, quote(SAMPLE_SITE.description)),
        (SITE_FOOTER_KEY, quote(SAMPLE_SITE.footer)),
        (SITE_LANG_KEY, quote(SAMPLE_SITE.lang)),
        (SITE_LANGUAGES_KEY, format!("[{}]", languages.join(","))),
        (SITE_PER_PAGE_KEY, SAMPLE_SITE.per_page.to_string()),
        (ORIGIN_KEY, quote(SAMPLE_SITE.origin)),
    ];
    for (key, value) in rows {
        db.put_setting(key, &value, 0)
            .await
            .map_err(|e| format!("put_setting {key}: {e}"))?;
    }
    Ok(())
}

/// Saves the sample site's settings and every sample post as published, by one owner, with
/// its tags and translation links.
///
/// # Errors
/// The store call that failed, as a detail for the log.
pub async fn sample_posts<S: UserStore + PostStore + SettingsStore + TaxonomyStore>(
    db: &S,
) -> Result<usize, String> {
    sample_settings(db).await?;
    let src = SampleSource::get();
    let posts = src
        .posts(Filter::All, 0, usize::MAX)
        .await
        .map_err(|e| e.to_string())?
        .posts;
    let owner = db
        .create_user(NewUser {
            login: "sample".to_owned(),
            slug: "sample".to_owned(),
            display_name: "Sample".to_owned(),
            bio: String::new(),
            role: Role::Owner,
            password_hash: None,
            created_at: 0,
        })
        .await
        .map_err(|e| format!("create_user: {e}"))?;
    let mut saved: HashMap<(String, String), PostId> = HashMap::new();
    for p in &posts {
        // The store keeps milliseconds; sample dates are seconds.
        let at = p.published_at * 1000;
        let mut tag_ids = Vec::new();
        for t in &p.tags {
            tag_ids.push(
                db.ensure_tag(&t.slug, &t.name)
                    .await
                    .map_err(|e| format!("ensure_tag {}: {e}", t.slug))?,
            );
        }
        // Joins the group of a translation saved earlier; the first of a pair starts it.
        let group = p
            .translations
            .iter()
            .find_map(|key| saved.get(key))
            .copied();
        let id = db
            .save_post(SavePost {
                id: None,
                expected_version: None,
                translation_group: group,
                kind: PostKind::Post,
                lang: p.lang.clone(),
                slug: p.slug.clone(),
                status: PostStatus::Published,
                published_at: Some(at),
                author_id: owner,
                title: p.title.clone(),
                markdown: String::new(),
                html: p.body.html.clone(),
                toc_json: "[]".to_owned(),
                excerpt: p.excerpt.clone().unwrap_or_else(|| p.body.excerpt.clone()),
                word_count: 0,
                reading_minutes: i64::from(p.body.reading_minutes),
                pinned: p.pinned,
                comments_enabled: false,
                cover_media_id: None,
                series_id: None,
                series_position: None,
                tag_ids,
                media_ids: Vec::new(),
                search_text: text_of(&p.body.html),
                revision: None,
                now: at,
            })
            .await
            .map_err(|e| format!("save_post {}: {e}", p.slug))?;
        saved.insert((p.lang.clone(), p.slug.clone()), id);
    }
    Ok(posts.len())
}

/// Visible text of rendered post HTML: tags dropped, the escapes our renderer writes undone.
fn text_of(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        match c {
            '<' => in_tag = true,
            '>' if in_tag => {
                in_tag = false;
                out.push(' ');
            }
            c if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&")
}

/// `n` more published posts whose text is `word`, for tests of the result cap.
///
/// # Errors
/// The store call that failed.
#[cfg(test)]
pub async fn copies<S: UserStore + PostStore>(db: &S, word: &str, n: usize) -> Result<(), String> {
    let owner = db
        .list_users()
        .await
        .map_err(|e| e.to_string())?
        .first()
        .map(|u| u.id)
        .ok_or("no user")?;
    for i in 0..n {
        let at = 1_000_000 + i64::try_from(i).unwrap_or(0);
        db.save_post(SavePost {
            id: None,
            expected_version: None,
            translation_group: None,
            kind: PostKind::Post,
            lang: "en".to_owned(),
            slug: format!("{word}-{i}"),
            status: PostStatus::Published,
            published_at: Some(at),
            author_id: owner,
            title: format!("{word} {i}"),
            markdown: String::new(),
            html: String::new(),
            toc_json: "[]".to_owned(),
            excerpt: String::new(),
            word_count: 0,
            reading_minutes: 1,
            pinned: false,
            comments_enabled: false,
            cover_media_id: None,
            series_id: None,
            series_position: None,
            tag_ids: Vec::new(),
            media_ids: Vec::new(),
            search_text: word.to_owned(),
            revision: None,
            now: at,
        })
        .await
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}
