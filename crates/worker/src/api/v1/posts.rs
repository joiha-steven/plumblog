//! `/api/v1/posts`: list, read, create, update (optimistic `expected_version`), trash,
//! restore and delete. Markdown is rendered here on save; the store keeps the result.

use plumblog_auth::base64url;
use plumblog_model::api::{DEFAULT_LIMIT, MAX_LIMIT};
use plumblog_model::{
    Error, ErrorCode, MediaId, Permission, PostId, Result, SeriesId, TagId, UserId,
};
use plumblog_store::{
    NewRevision, Post, PostCursor, PostKind, PostOrder, PostQuery, PostStatus, PostStore,
    PostSummary, SavePost, Saved, SettingsStore, valid_client_id,
};
use plumblog_syslog::EventKind;

use super::post_body;
use super::{Input, Nullable, Principal};
use crate::api::body::{Out, json_string, object, quote};
use crate::api::{Ctx, Reply};
use crate::pages::reader_db::SITE_LANG_KEY;
use crate::pages::routes::RESERVED;

const MAX_SLUG: usize = 100;
const MAX_TITLE: usize = 300;
const MAX_EXCERPT: usize = 1000;
const MAX_LANG: usize = 16;

fn invalid(field: &str, why: &str) -> Error {
    Error::new(ErrorCode::InvalidInput, format!("{field}: {why}")).at(field)
}

fn plain(field: &str, v: &str, max: usize, multiline: bool) -> Result<()> {
    if v.chars().count() > max {
        return Err(invalid(field, "too long"));
    }
    let bad = |c: char| c.is_control() && !(multiline && (c == '\n' || c == '\t'));
    if v.chars().any(bad) {
        return Err(invalid(field, "control character"));
    }
    Ok(())
}

/// `a-z 0-9 -`, 1-100, no hyphen at either end: it is the URL.
fn slug(v: &str) -> Result<()> {
    let ok = (1..=MAX_SLUG).contains(&v.len())
        && v.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !v.starts_with('-')
        && !v.ends_with('-');
    if ok {
        Ok(())
    } else {
        Err(invalid("slug", "1-100 of a-z, 0-9 and inner hyphens"))
    }
}

/// Refuses a slug the router owns (`pages::routes::RESERVED`) in the site's default language,
/// where posts live at `/<slug>`; another language's posts are at `/<lang>/<slug>`. `search`
/// is refused in every language: `/<lang>/search` is that language's search page. Reads the
/// default language only for such a slug.
async fn not_reserved(ctx: &Ctx, post_lang: &str, post_slug: &str) -> Result<()> {
    if !RESERVED.contains(&post_slug) {
        return Ok(());
    }
    if post_slug == "search" {
        return Err(invalid(
            "slug",
            "reserved: the site uses this address for something else",
        ));
    }
    let default = ctx
        .store
        .get_setting(SITE_LANG_KEY)
        .await?
        .and_then(|v| json_string(&v))
        .unwrap_or_else(|| "en".to_owned());
    if post_lang == default {
        return Err(invalid(
            "slug",
            "reserved: the site uses this address for something else",
        ));
    }
    Ok(())
}

/// A language tag as used in URLs: `en`, `vi`, `pt-br`, `zh-hant`.
fn lang(v: &str) -> Result<()> {
    let mut parts = v.split('-');
    let primary = parts.next().unwrap_or("");
    let ok = v.len() <= MAX_LANG
        && (2..=3).contains(&primary.len())
        && primary.bytes().all(|b| b.is_ascii_lowercase())
        && parts.all(|p| {
            (2..=8).contains(&p.len())
                && p.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        });
    if ok {
        Ok(())
    } else {
        Err(invalid("lang", "language tag like en or pt-br"))
    }
}

fn positive(field: &str, v: Option<i64>) -> Result<Option<i64>> {
    match v {
        Some(n) if n <= 0 => Err(invalid(field, "positive id")),
        v => Ok(v),
    }
}

fn kind(input: &Input) -> Result<Option<PostKind>> {
    input
        .str("kind")
        .map(|k| PostKind::parse(k).ok_or_else(|| invalid("kind", "post or page")))
        .transpose()
}

/// A status a save may set: never `trashed` (that is the trash route).
fn status(input: &Input) -> Result<Option<PostStatus>> {
    input
        .str("status")
        .map(|s| match PostStatus::parse(s) {
            Some(PostStatus::Trashed) | None => {
                Err(invalid("status", "draft, scheduled or published"))
            }
            Some(s) => Ok(s),
        })
        .transpose()
}

/// `published_at` for a save: none for a draft, now when publishing without one (a future
/// time is refused: that is `scheduled`), a future time for scheduling. `checked` is false for an update that leaves both alone, so an old
/// scheduled post can still be edited after its time passed.
fn published_at(
    status: PostStatus,
    at: Option<i64>,
    now: i64,
    checked: bool,
) -> Result<Option<i64>> {
    match status {
        PostStatus::Draft if at.is_some() && checked => {
            Err(invalid("published_at", "drafts have none"))
        }
        PostStatus::Draft | PostStatus::Trashed => Ok(None),
        PostStatus::Published => match at {
            // A future date on a published post would hide it while the API says "published".
            Some(t) if checked && t > now => Err(Error::new(
                ErrorCode::PostPublishedInFuture,
                "published_at: in the future; use status scheduled",
            )
            .at("published_at")),
            t => Ok(Some(t.unwrap_or(now))),
        },
        PostStatus::Scheduled => match at {
            None => Err(invalid("published_at", "required to schedule")),
            Some(t) if checked && t <= now => Err(invalid("published_at", "in the past")),
            t => Ok(t),
        },
    }
}

fn ids_json(ids: impl Iterator<Item = i64>) -> Out {
    Out::Raw(format!(
        "[{}]",
        ids.map(|i| i.to_string()).collect::<Vec<_>>().join(",")
    ))
}

fn opt_int(v: Option<i64>) -> Out {
    v.map_or(Out::Null, Out::Int)
}

fn summary_fields(p: &PostSummary) -> Vec<(&'static str, Out)> {
    vec![
        ("id", Out::Int(p.id.0)),
        ("kind", Out::Str(p.kind.as_str().to_owned())),
        ("translation_group", Out::Int(p.translation_group.0)),
        ("lang", Out::Str(p.lang.clone())),
        ("slug", Out::Str(p.slug.clone())),
        ("status", Out::Str(p.status.as_str().to_owned())),
        (
            "trashed_from",
            p.trashed_from
                .map_or(Out::Null, |s| Out::Str(s.as_str().to_owned())),
        ),
        ("published_at", opt_int(p.published_at)),
        ("author_id", Out::Int(p.author_id.0)),
        ("title", Out::Str(p.title.clone())),
        ("excerpt", Out::Str(p.excerpt.clone())),
        ("word_count", Out::Int(p.word_count)),
        ("reading_minutes", Out::Int(p.reading_minutes)),
        ("pinned", Out::Bool(p.pinned)),
        ("comments_enabled", Out::Bool(p.comments_enabled)),
        ("cover_media_id", opt_int(p.cover_media_id.map(|m| m.0))),
        ("series_id", opt_int(p.series_id.map(|s| s.0))),
        ("series_position", opt_int(p.series_position)),
        ("created_at", Out::Int(p.created_at)),
        ("updated_at", Out::Int(p.updated_at)),
        ("version", Out::Int(p.version)),
        ("tags", ids_json(p.tag_ids.iter().map(|t| t.0))),
    ]
}

/// A whole post: the summary plus its Markdown, HTML and the media its body uses.
fn post_json(p: &Post, warnings: &[&str]) -> String {
    let mut out = summary_fields(&PostSummary::from(p));
    out.push(("markdown", Out::Str(p.markdown.clone())));
    out.push(("html", Out::Str(p.html.clone())));
    out.push((
        "media_ids",
        ids_json(post_body::media_ids(&p.html).into_iter()),
    ));
    out.push((
        "warnings",
        Out::Raw(format!(
            "[{}]",
            warnings
                .iter()
                .map(|w| quote(w))
                .collect::<Vec<_>>()
                .join(",")
        )),
    ));
    object(&out)
}

fn order_code(o: PostOrder) -> &'static str {
    match o {
        PostOrder::PublishedDesc => "published",
        PostOrder::UpdatedDesc => "updated",
        PostOrder::SeriesAsc => "series",
    }
}

/// Opaque to clients: base64url of `<order>.<key or ->.<id>`.
fn encode_cursor(order: PostOrder, c: PostCursor) -> String {
    let key = c.key.map_or_else(|| "-".to_owned(), |k| k.to_string());
    base64url::encode(format!("{}.{key}.{}", order_code(order), c.id.0).as_bytes())
}

fn decode_cursor(order: PostOrder, text: &str) -> Result<PostCursor> {
    let bad = || invalid("cursor", "not a cursor of this listing");
    let raw = base64url::decode(text).ok_or_else(bad)?;
    let raw = String::from_utf8(raw).map_err(|_| bad())?;
    let mut parts = raw.split('.');
    let (Some(o), Some(k), Some(id), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(bad());
    };
    if o != order_code(order) {
        return Err(bad());
    }
    let key = if k == "-" {
        None
    } else {
        Some(k.parse::<i64>().map_err(|_| bad())?)
    };
    let id: i64 = id.parse().map_err(|_| bad())?;
    if id <= 0 || (key.is_none() && order != PostOrder::SeriesAsc) {
        return Err(bad());
    }
    Ok(PostCursor {
        key,
        id: PostId(id),
    })
}

/// `GET /api/v1/posts`: one page, any status (drafts and trash included: every caller here
/// is signed in), keyset-paged. `next_cursor` is `null` on the last page.
///
/// `counts` (posts per status, for the admin's filter pills) is sent on the first page only
/// (no `cursor`), `null` after. It follows `kind` and `lang` like the list, and nothing
/// else: `status`, `tag`, `series` and `author` do not narrow it, because the pills show
/// every status. It needs no filter of its own: the list already shows every status to any
/// caller this route admits.
pub async fn list(ctx: &Ctx, input: &Input) -> Result<Reply> {
    let order = match input.str("order").unwrap_or("updated") {
        "updated" => PostOrder::UpdatedDesc,
        "published" => PostOrder::PublishedDesc,
        "series" => PostOrder::SeriesAsc,
        _ => return Err(invalid("order", "updated, published or series")),
    };
    let limit = input.int("limit").unwrap_or(i64::from(DEFAULT_LIMIT));
    let limit = u32::try_from(limit)
        .ok()
        .filter(|l| (1..=MAX_LIMIT).contains(l))
        .ok_or_else(|| invalid("limit", "1-100"))?;
    // One more row than asked says whether another page exists without an empty last page.
    let mut q = PostQuery::new(order, limit + 1);
    q.kind = kind(input)?;
    q.status = input
        .str("status")
        .map(|s| PostStatus::parse(s).ok_or_else(|| invalid("status", "a post status")))
        .transpose()?;
    if let Some(l) = input.str("lang") {
        lang(l)?;
        q.lang = Some(l.to_owned());
    }
    q.tag = positive("tag", input.int("tag"))?.map(TagId);
    q.series = positive("series", input.int("series"))?.map(SeriesId);
    q.author = positive("author", input.int("author"))?.map(UserId);
    q.after = input
        .str("cursor")
        .map(|c| decode_cursor(order, c))
        .transpose()?;
    let counts = if q.after.is_none() {
        let c = ctx
            .store
            .count_posts_by_status(q.kind, q.lang.as_deref())
            .await?;
        Out::Raw(object(&[
            ("draft", Out::Int(c.draft)),
            ("scheduled", Out::Int(c.scheduled)),
            ("published", Out::Int(c.published)),
            ("trashed", Out::Int(c.trashed)),
        ]))
    } else {
        Out::Null
    };
    let mut rows = ctx.store.list_posts(&q).await?;
    let more = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    let next = match rows.last() {
        Some(last) if more => Out::Str(encode_cursor(order, order.cursor(last))),
        _ => Out::Null,
    };
    let items: Vec<String> = rows.iter().map(|p| object(&summary_fields(p))).collect();
    Ok(Reply::json(
        200,
        object(&[
            ("posts", Out::Raw(format!("[{}]", items.join(",")))),
            ("next_cursor", next),
            ("counts", counts),
        ]),
    ))
}

/// `GET /api/v1/posts/<id>`, any status.
pub async fn get(ctx: &Ctx, input: &Input) -> Result<Reply> {
    let post = ctx.store.get_post(PostId(input.id())).await?;
    Ok(Reply::json(200, post_json(&post, &[])))
}

/// The fields a create and an update share, checked.
struct Common {
    title: Option<String>,
    excerpt: Option<String>,
    tags: Option<Vec<TagId>>,
    cover: Nullable,
    series: Nullable,
    series_position: Nullable,
}

fn common(input: &Input) -> Result<Common> {
    if let Some(l) = input.str("lang") {
        lang(l)?;
    }
    if let Some(s) = input.str("slug") {
        slug(s)?;
    }
    if let Some(t) = input.str("title") {
        plain("title", t, MAX_TITLE, false)?;
    }
    if let Some(e) = input.str("excerpt") {
        plain("excerpt", e, MAX_EXCERPT, true)?;
    }
    let tags = input
        .ints("tags")
        .map(|ids| {
            ids.into_iter()
                .map(|i| positive("tags", Some(i)).map(|_| TagId(i)))
                .collect::<Result<Vec<_>>>()
        })
        .transpose()?;
    let id_or_null = |name: &str| -> Result<Nullable> {
        let v = input.int_or_null(name);
        if let Nullable::Value(n) = v {
            positive(name, Some(n))?;
        }
        Ok(v)
    };
    Ok(Common {
        title: input.str("title").map(str::to_owned),
        excerpt: input.str("excerpt").map(str::to_owned),
        tags,
        cover: id_or_null("cover_media_id")?,
        series: id_or_null("series_id")?,
        series_position: input.int_or_null("series_position"),
    })
}

fn render(markdown: &str) -> plumblog_markdown::Rendered {
    plumblog_markdown::render(markdown, plumblog_markdown::Options::default())
}

/// Saves and maps a refused save: `Conflict` names `slug` or `lang` (the pair another post
/// holds), and a bad reference (tag, series, cover, body media) stays `InvalidInput`.
async fn save(ctx: &Ctx, post: SavePost) -> Result<Saved> {
    let (lang_v, slug_v, id) = (post.lang.clone(), post.slug.clone(), post.id);
    match ctx.store.save_post_outcome(post).await {
        Err(e) if e.code == ErrorCode::Conflict => {
            let other = ctx.store.find_post(&lang_v, &slug_v).await.ok().flatten();
            let field = match other {
                Some(o) if Some(o.id) != id => "slug",
                _ => "lang",
            };
            Err(e.at(field))
        }
        r => r,
    }
}

fn warning_codes(r: &plumblog_markdown::Rendered) -> Vec<&'static str> {
    r.warnings.iter().map(|w| w.code).collect()
}

async fn log(ctx: &Ctx, p: &Principal, action: &str, post: &Post) {
    ctx.syslog(
        p.event(ctx, EventKind::ContentChanged)
            .subject(&post.title)
            .field("action", action)
            .field("post", &post.id.0.to_string())
            .field("status", post.status.as_str()),
    )
    .await;
}

/// `POST /api/v1/posts`: 201 with the saved post and the renderer's warnings. With a
/// `client_id` this user already created a post with, the same 201 with that post as stored
/// now, whatever the body says, and nothing written or logged: the client retried a create
/// whose answer it lost, and may have edited the text since (its next PATCH sends that).
pub async fn create(ctx: &Ctx, p: &Principal, input: &Input) -> Result<Reply> {
    let user = p.require(Permission::WritePost)?;
    let status = status(input)?.unwrap_or(PostStatus::Draft);
    if status != PostStatus::Draft {
        p.require(Permission::PublishPost)?;
    }
    let client_id = input.str("client_id");
    if let Some(k) = client_id {
        if !valid_client_id(k) {
            return Err(invalid("client_id", "1-64 of A-Z, a-z, 0-9, - and _"));
        }
        if let Some(id) = ctx.store.find_client_post(user.id, k).await? {
            return replay(ctx, id, input.str("title")).await;
        }
    }
    let c = common(input)?;
    let at = published_at(
        status,
        input.int_or_null("published_at").or(None),
        ctx.now,
        true,
    )?;
    let translation_group = match positive("translation_of", input.int("translation_of"))? {
        Some(id) => Some(
            ctx.store
                .get_post(PostId(id))
                .await
                .map_err(|e| {
                    if e.code == ErrorCode::PostNotFound {
                        invalid("translation_of", "no such post")
                    } else {
                        e
                    }
                })?
                .translation_group,
        ),
        None => None,
    };
    let markdown = input.str("markdown").unwrap_or_default().to_owned();
    let r = render(&markdown);
    let text = post_body::plain_text(&r.html);
    let excerpt = c
        .excerpt
        .filter(|e| !e.is_empty())
        .unwrap_or_else(|| r.excerpt.clone());
    not_reserved(
        ctx,
        input.str("lang").unwrap_or_default(),
        input.str("slug").unwrap_or_default(),
    )
    .await?;
    let saved = save(
        ctx,
        SavePost {
            id: None,
            expected_version: None,
            translation_group,
            kind: kind(input)?.unwrap_or(PostKind::Post),
            lang: input.str("lang").unwrap_or_default().to_owned(),
            slug: input.str("slug").unwrap_or_default().to_owned(),
            status,
            published_at: at,
            author_id: user.id,
            title: c.title.unwrap_or_default(),
            markdown,
            toc_json: post_body::toc_json(&r.toc),
            excerpt,
            word_count: post_body::word_count(&text),
            reading_minutes: i64::from(r.reading_minutes),
            pinned: input.bool("pinned").unwrap_or(false),
            comments_enabled: input.bool("comments_enabled").unwrap_or(false),
            cover_media_id: c.cover.or(None).map(MediaId),
            series_id: c.series.or(None).map(SeriesId),
            series_position: c.series_position.or(None),
            tag_ids: c.tags.unwrap_or_default(),
            media_ids: post_body::media_ids(&r.html)
                .into_iter()
                .map(MediaId)
                .collect(),
            search_text: text,
            html: r.html.clone(),
            revision: Some(NewRevision {
                author_id: Some(user.id),
                created_at: ctx.now,
            }),
            client_id: client_id.map(str::to_owned),
            now: ctx.now,
        },
    )
    .await?;
    // Not inserted by this request: it lost a race to a create with the same client id, and
    // the store returned the winner.
    if !saved.wrote {
        return replay(ctx, saved.id, input.str("title")).await;
    }
    let post = ctx.store.get_post(saved.id).await?;
    log(ctx, p, "post.create", &post).await;
    Ok(Reply::json(201, post_json(&post, &warning_codes(&r))))
}

/// The answer to a repeated `client_id`: the post as the first create's 201 would show it
/// now, its warnings from its stored Markdown. `title`: the request's, to spot a client that
/// reused an id for another post (it gets the old post back, never a new one).
async fn replay(ctx: &Ctx, id: PostId, title: Option<&str>) -> Result<Reply> {
    let post = ctx.store.get_post(id).await?;
    if title.is_some_and(|t| t != post.title) {
        worker::console_warn!(
            "[{}] POST post: client_id repeated with another title; answering post {} \
             (version {}) as stored. A client reusing an id for a new post gets this one back",
            ctx.request_id.as_str(),
            id.0,
            post.version
        );
    } else {
        worker::console_log!(
            "[{}] POST post: client_id repeated, answering post {} (version {})",
            ctx.request_id.as_str(),
            id.0,
            post.version
        );
    }
    let r = render(&post.markdown);
    Ok(Reply::json(201, post_json(&post, &warning_codes(&r))))
}

/// The derived body of an updated post.
struct Body {
    html: String,
    toc_json: String,
    word_count: i64,
    reading_minutes: i64,
    excerpt: String,
    warnings: Vec<&'static str>,
}

/// A new Markdown body is rendered; without one the stored rendering stays (it may not come
/// from this renderer, e.g. imported HTML). A written excerpt is kept until replaced; `""`
/// or a new body without one gets the automatic excerpt.
fn body_for_update(old: &Post, markdown: Option<&str>, excerpt: Option<&str>) -> Body {
    match markdown {
        Some(md) => {
            let r = render(md);
            Body {
                toc_json: post_body::toc_json(&r.toc),
                word_count: post_body::word_count(&post_body::plain_text(&r.html)),
                reading_minutes: i64::from(r.reading_minutes),
                excerpt: match excerpt {
                    Some(e) if !e.is_empty() => e.to_owned(),
                    _ => r.excerpt.clone(),
                },
                warnings: warning_codes(&r),
                html: r.html,
            }
        }
        None => Body {
            html: old.html.clone(),
            toc_json: old.toc_json.clone(),
            word_count: old.word_count,
            reading_minutes: old.reading_minutes,
            excerpt: match excerpt {
                Some("") => render(&old.markdown).excerpt,
                Some(e) => e.to_owned(),
                None => old.excerpt.clone(),
            },
            warnings: Vec::new(),
        },
    }
}

/// [`not_reserved`] for an update that changes the address; an old post stays editable.
async fn not_reserved_if_moved(ctx: &Ctx, input: &Input, old: &Post) -> Result<()> {
    if !(input.has("lang") || input.has("slug")) {
        return Ok(());
    }
    let lang = input.str("lang").unwrap_or(&old.lang);
    let slug = input.str("slug").unwrap_or(&old.slug);
    not_reserved(ctx, lang, slug).await
}

/// The `published_at` an update saves. Publishing a scheduled post without a date means now,
/// not its scheduled time.
fn update_published_at(
    input: &Input,
    old: &Post,
    status: PostStatus,
    status_sent: bool,
    now: i64,
) -> Result<Option<i64>> {
    let publish_now = status == PostStatus::Published
        && old.status == PostStatus::Scheduled
        && !input.has("published_at");
    let at = input
        .int_or_null("published_at")
        .or(old.published_at.filter(|_| !publish_now));
    let checked = status_sent || input.has("published_at");
    published_at(status, at, now, checked)
}

/// `PATCH /api/v1/posts/<id>`: changes only the fields given. `expected_version` must be
/// the stored one, else 409 `store.conflict` with the current `version`, so a client never
/// overwrites changes it did not see. Another author's post needs `post.edit_any`; making
/// or keeping a post live needs `post.publish`.
pub async fn update(ctx: &Ctx, p: &Principal, input: &Input) -> Result<Reply> {
    let user = p.require(Permission::WritePost)?;
    let c = common(input)?;
    let id = PostId(input.id());
    let expected = input.int("expected_version").unwrap_or(0);
    let old = ctx.store.get_post(id).await?;
    if old.author_id != user.id {
        p.require(Permission::EditAnyPost)?;
    }
    let new_status = status(input)?;
    let status = new_status.unwrap_or(old.status);
    if status != PostStatus::Draft
        || matches!(old.status, PostStatus::Published | PostStatus::Scheduled)
    {
        p.require(Permission::PublishPost)?;
    }
    let at = update_published_at(input, &old, status, new_status.is_some(), ctx.now)?;
    not_reserved_if_moved(ctx, input, &old).await?;
    let new_markdown = input.str("markdown");
    let Body {
        html,
        toc_json,
        word_count,
        reading_minutes,
        excerpt,
        warnings,
    } = body_for_update(&old, new_markdown, c.excerpt.as_deref());
    let title = c.title.unwrap_or_else(|| old.title.clone());
    let markdown = new_markdown.map_or_else(|| old.markdown.clone(), str::to_owned);
    let revised = title != old.title || markdown != old.markdown;
    let text = post_body::plain_text(&html);
    let save_post = SavePost {
        id: Some(id),
        expected_version: Some(expected),
        translation_group: None,
        kind: kind(input)?.unwrap_or(old.kind),
        lang: input
            .str("lang")
            .map_or_else(|| old.lang.clone(), str::to_owned),
        slug: input
            .str("slug")
            .map_or_else(|| old.slug.clone(), str::to_owned),
        status,
        published_at: at,
        author_id: old.author_id,
        title,
        markdown,
        media_ids: post_body::media_ids(&html)
            .into_iter()
            .map(MediaId)
            .collect(),
        search_text: text,
        html,
        toc_json,
        excerpt,
        word_count,
        reading_minutes,
        pinned: input.bool("pinned").unwrap_or(old.pinned),
        comments_enabled: input
            .bool("comments_enabled")
            .unwrap_or(old.comments_enabled),
        cover_media_id: c.cover.or(old.cover_media_id.map(|m| m.0)).map(MediaId),
        series_id: c.series.or(old.series_id.map(|s| s.0)).map(SeriesId),
        series_position: c.series_position.or(old.series_position),
        tag_ids: c.tags.unwrap_or(old.tag_ids),
        revision: revised.then_some(NewRevision {
            author_id: Some(user.id),
            created_at: ctx.now,
        }),
        client_id: None,
        now: ctx.now,
    };
    match save(ctx, save_post).await {
        // Stale but equal: this same update landed before and its answer was lost (or another
        // save of the same content won meanwhile); the store wrote nothing, so nothing to log.
        Ok(s) if !s.wrote => {
            worker::console_log!(
                "[{}] PATCH post {}: version {expected} is stale, but the stored post already \
                 holds this update",
                ctx.request_id.as_str(),
                id.0
            );
            let post = ctx.store.get_post(id).await?;
            return Ok(Reply::json(200, post_json(&post, &warnings)));
        }
        Ok(_) => {}
        Err(e) if e.code == ErrorCode::Conflict => return conflict(ctx, id, expected, e).await,
        Err(e) => return Err(e),
    }
    let post = ctx.store.get_post(id).await?;
    log(ctx, p, "post.update", &post).await;
    Ok(Reply::json(200, post_json(&post, &warnings)))
}

/// A refused update: a stale version (another save won, maybe at the same moment) is a 409
/// with the version to merge against; otherwise the slug or lang is taken (`e`).
async fn conflict(ctx: &Ctx, id: PostId, expected: i64, e: Error) -> Result<Reply> {
    let current = ctx.store.get_post(id).await?;
    if current.version == expected {
        return Err(e);
    }
    worker::console_warn!(
        "[{}] PATCH post {}: version {expected} is stale, stored {}",
        ctx.request_id.as_str(),
        id.0,
        current.version
    );
    Ok(Reply::json(
        409,
        object(&[
            ("error", Out::Str(ErrorCode::Conflict.as_str().to_owned())),
            ("field", Out::Str("expected_version".to_owned())),
            ("version", Out::Int(current.version)),
        ]),
    ))
}

/// The post a trash, restore or delete acts on; another author's needs `post.delete_any`.
async fn owned_or_any(ctx: &Ctx, p: &Principal, input: &Input) -> Result<Post> {
    let user = p.require(Permission::WritePost)?;
    let post = ctx.store.get_post(PostId(input.id())).await?;
    if post.author_id != user.id {
        p.require(Permission::DeleteAnyPost)?;
    }
    Ok(post)
}

/// `POST /api/v1/posts/<id>/trash` and `.../restore`: the post afterwards.
pub async fn trash(ctx: &Ctx, p: &Principal, input: &Input, to_trash: bool) -> Result<Reply> {
    let post = owned_or_any(ctx, p, input).await?;
    if to_trash {
        ctx.store.trash_post(post.id, ctx.now).await?;
    } else {
        ctx.store.restore_post(post.id, ctx.now).await?;
    }
    let post = ctx.store.get_post(post.id).await?;
    let action = if to_trash {
        "post.trash"
    } else {
        "post.restore"
    };
    log(ctx, p, action, &post).await;
    Ok(Reply::json(200, post_json(&post, &[])))
}

/// `DELETE /api/v1/posts/<id>`: permanent, only from the trash.
pub async fn delete(ctx: &Ctx, p: &Principal, input: &Input) -> Result<Reply> {
    let post = owned_or_any(ctx, p, input).await?;
    if post.status != PostStatus::Trashed {
        return Err(invalid("status", "only a trashed post can be deleted"));
    }
    ctx.store.delete_post(post.id).await?;
    log(ctx, p, "post.delete", &post).await;
    Ok(Reply::empty())
}

#[cfg(test)]
mod tests {
    use super::{decode_cursor, encode_cursor, lang, published_at, slug};
    use plumblog_model::ErrorCode;
    use plumblog_model::PostId;
    use plumblog_store::{PostCursor, PostOrder, PostStatus};

    #[test]
    fn cursors_round_trip_and_refuse_others() {
        for (o, key) in [
            (PostOrder::UpdatedDesc, Some(5)),
            (PostOrder::PublishedDesc, Some(-1)),
            (PostOrder::SeriesAsc, None),
        ] {
            let c = PostCursor { key, id: PostId(9) };
            assert_eq!(decode_cursor(o, &encode_cursor(o, c)).unwrap(), c);
        }
        let upd = encode_cursor(
            PostOrder::UpdatedDesc,
            PostCursor {
                key: Some(1),
                id: PostId(2),
            },
        );
        let e = decode_cursor(PostOrder::PublishedDesc, &upd).unwrap_err();
        assert_eq!(e.field.as_deref(), Some("cursor"));
        for bad in ["", "!!", "dXBkYXRlZA", "dXBkYXRlZC4tLjE"] {
            assert!(decode_cursor(PostOrder::UpdatedDesc, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn field_rules() {
        assert!(slug("2026-11-hello").is_ok());
        assert!(slug("Hello").is_err());
        for ok in ["en", "vi", "pt-br", "zh-hant"] {
            assert!(lang(ok).is_ok(), "{ok}");
        }
        for bad in ["", "e", "EN", "en-", "en_us", "english"] {
            assert!(lang(bad).is_err(), "{bad}");
        }
        assert_eq!(
            published_at(PostStatus::Published, None, 7, true).unwrap(),
            Some(7)
        );
        // Published in the future is refused with its own code, not silently scheduled.
        let early = published_at(PostStatus::Published, Some(9), 7, true).unwrap_err();
        assert_eq!(early.code, ErrorCode::PostPublishedInFuture);
        assert_eq!(early.field.as_deref(), Some("published_at"));
        assert_eq!(
            published_at(PostStatus::Published, Some(7), 7, true).unwrap(),
            Some(7)
        );
        assert!(published_at(PostStatus::Scheduled, Some(5), 7, true).is_err());
        assert_eq!(
            published_at(PostStatus::Scheduled, Some(5), 7, false).unwrap(),
            Some(5)
        );
        assert!(published_at(PostStatus::Draft, Some(5), 7, true).is_err());
        assert_eq!(
            published_at(PostStatus::Draft, Some(5), 7, false).unwrap(),
            None
        );
    }
}
