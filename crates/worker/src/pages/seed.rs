//! Test and dev builds only: copies the sample posts into a store, so search (which reads the
//! store) finds the same posts the sample pages show. Saved through `save_post`, the same
//! path the editor will use, so the search index is built (and d-stroke folded) for real.

use super::source::{Filter, ReaderSource, SampleSource};
use plumblog_model::Role;
use plumblog_store::{NewUser, PostKind, PostStatus, PostStore, SavePost, UserStore};

/// Saves every sample post as published, by one owner.
///
/// # Errors
/// The store call that failed, as a detail for the log.
pub async fn sample_posts<S: UserStore + PostStore>(db: &S) -> Result<usize, String> {
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
    for p in &posts {
        // The store keeps milliseconds; sample dates are seconds.
        let at = p.published_at * 1000;
        db.save_post(SavePost {
            id: None,
            expected_version: None,
            translation_group: None,
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
            tag_ids: Vec::new(),
            media_ids: Vec::new(),
            search_text: text_of(&p.body.html),
            revision: None,
            now: at,
        })
        .await
        .map_err(|e| format!("save_post {}: {e}", p.slug))?;
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
