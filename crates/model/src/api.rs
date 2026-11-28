//! The `/api/v1` route table: the one source for the Worker's router, `GET /api/v1/reference`
//! and `blog guide api`, so the reference cannot drift from what is served. Pure data, no
//! prose: user-facing text belongs to locales and guides.

use crate::Permission;

/// Request body cap for most routes; auth and taxonomy bodies are a few hundred bytes.
pub const MAX_BODY: usize = 8 * 1024;
/// Request body cap for post routes: room for a ~20k-word post with code, math and images
/// (docs/ARCHITECTURE.md, Budgets).
pub const MAX_POST_BODY: usize = 160 * 1024;
/// Request body cap for a media upload (the original image bytes). Under the Workers
/// request-body limit of every plan (100 MB on Free) and well inside the 128 MB isolate
/// memory, since the Worker holds the whole body to check its header (docs/ARCHITECTURE.md,
/// Budgets).
pub const MAX_MEDIA_BODY: usize = 20 * 1024 * 1024;
/// Image types a media upload accepts (the upload's `mime` field).
pub const MEDIA_MIMES: &[&str] = &[
    "image/avif",
    "image/webp",
    "image/jpeg",
    "image/png",
    "image/gif",
];
/// The `response` of a route that answers with a media item's stored original: the raw
/// bytes, `Content-Type` the item's `mime`, not JSON (errors are JSON as everywhere).
pub const ORIGINAL: &str = "original";
/// Largest `limit` a list accepts.
pub const MAX_LIMIT: u32 = 100;
/// `limit` when none is given.
pub const DEFAULT_LIMIT: u32 = 20;
/// Settings only a signed-in session may write or clear; an API token gets 403
/// `auth.forbidden`. The origin is what browser writes and passkeys are checked against, so
/// changing it can lock the owner out, like the `/api/auth` routes tokens may not use. The
/// Turnstile site key guards sign-in: a leaked owner token must not be able to turn it off.
pub const SESSION_ONLY_SETTINGS: &[&str] = &["origin", "turnstile_site_key"];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl Method {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
        }
    }
}

/// Who may call a route.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Any signed-in user or token, a `read` token included.
    Read,
    /// The role or the token's effective scope must hold it.
    Require(Permission),
}

/// JSON type of a field. Query and path values arrive as text and are parsed to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Type {
    Str,
    /// An integer in the i64 range; never a float.
    Int,
    Bool,
    /// An integer or `null` (clears the value).
    IntOrNull,
    /// An array of integers.
    IntList,
    /// Any JSON value (setting and option values), stored as sent after a strict parse.
    Json,
}

impl Type {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Str => "string",
            Self::Int => "integer",
            Self::Bool => "boolean",
            Self::IntOrNull => "integer|null",
            Self::IntList => "integer[]",
            Self::Json => "json",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum In {
    Path,
    Query,
    Body,
}

impl In {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Path => "path",
            Self::Query => "query",
            Self::Body => "body",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field {
    pub name: &'static str,
    pub ty: Type,
    pub at: In,
    pub required: bool,
}

const fn path(name: &'static str) -> Field {
    Field {
        name,
        ty: Type::Int,
        at: In::Path,
        required: true,
    }
}
/// A path segment that is a name (setting key, theme or plugin), not an id.
const fn path_name(name: &'static str) -> Field {
    Field {
        name,
        ty: Type::Str,
        at: In::Path,
        required: true,
    }
}
const fn query_req(name: &'static str, ty: Type) -> Field {
    Field {
        name,
        ty,
        at: In::Query,
        required: true,
    }
}
const fn query(name: &'static str, ty: Type) -> Field {
    Field {
        name,
        ty,
        at: In::Query,
        required: false,
    }
}
const fn req(name: &'static str, ty: Type) -> Field {
    Field {
        name,
        ty,
        at: In::Body,
        required: true,
    }
}
const fn opt(name: &'static str, ty: Type) -> Field {
    Field {
        name,
        ty,
        at: In::Body,
        required: false,
    }
}

/// Every route, so the router's `match` is exhaustive: a route in the table without a
/// handler does not compile.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RouteId {
    Reference,
    Me,
    PostList,
    PostGet,
    PostCreate,
    PostUpdate,
    PostTrash,
    PostRestore,
    PostDelete,
    TagList,
    TagGet,
    TagCreate,
    TagUpdate,
    TagDelete,
    SeriesList,
    SeriesGet,
    SeriesCreate,
    SeriesUpdate,
    SeriesDelete,
    MediaList,
    MediaGet,
    MediaUpload,
    MediaUpdate,
    MediaDelete,
    MediaOriginal,
    CommentList,
    UserList,
    RedirectList,
    RedirectPut,
    RedirectDelete,
    SettingList,
    SettingPut,
    SettingDelete,
    ThemeOptions,
    ThemeOptionPut,
    ThemeOptionsDelete,
    PluginList,
    PluginPut,
    PluginOptions,
    PluginOptionPut,
    PluginOptionsDelete,
}

/// What a request body is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Body {
    /// A strict JSON object of the body fields.
    Json,
    /// The raw bytes of an original image; its metadata comes in query fields.
    Original,
}

impl Body {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Original => "original",
        }
    }
}

/// Who makes a resource's derived files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Variants {
    /// The admin's in-browser AVIF encoder (issue #13); the API stores the original only.
    AdminEncoder,
}

impl Variants {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AdminEncoder => "admin_encoder",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Route {
    pub id: RouteId,
    pub method: Method,
    /// `{name}` segments are path fields.
    pub path: &'static str,
    pub access: Access,
    /// Checked too when the input calls for them (publishing, another author's post).
    pub also: &'static [Permission],
    /// Values of the `key` path field only a session may use (see [`SESSION_ONLY_SETTINGS`]).
    pub session_only_keys: &'static [&'static str],
    pub fields: &'static [Field],
    pub max_body: usize,
    pub body: Body,
    /// Set where derived files (image sizes) are not made by the API.
    pub variants: Option<Variants>,
    pub status: u16,
    /// Name of the response shape; `none` for an empty 204; [`ORIGINAL`] for the stored bytes
    /// themselves.
    pub response: &'static str,
}

const WRITE: Access = Access::Require(Permission::WritePost);
const PUBLISH_OR_ANY: &[Permission] = &[Permission::PublishPost, Permission::EditAnyPost];
const DELETE_ANY: &[Permission] = &[Permission::DeleteAnyPost];

const POST_FILTERS: &[Field] = &[
    query("kind", Type::Str),
    query("status", Type::Str),
    query("lang", Type::Str),
    query("tag", Type::Int),
    query("series", Type::Int),
    query("author", Type::Int),
    query("order", Type::Str),
    query("cursor", Type::Str),
    query("limit", Type::Int),
];

const PAGE_FILTERS: &[Field] = &[query("cursor", Type::Str), query("limit", Type::Int)];

const POST_CREATE: &[Field] = &[
    req("lang", Type::Str),
    req("slug", Type::Str),
    req("title", Type::Str),
    req("markdown", Type::Str),
    opt("kind", Type::Str),
    opt("status", Type::Str),
    opt("published_at", Type::IntOrNull),
    opt("excerpt", Type::Str),
    opt("pinned", Type::Bool),
    opt("comments_enabled", Type::Bool),
    opt("cover_media_id", Type::IntOrNull),
    opt("series_id", Type::IntOrNull),
    opt("series_position", Type::IntOrNull),
    opt("tags", Type::IntList),
    opt("translation_of", Type::Int),
];

const POST_UPDATE: &[Field] = &[
    path("id"),
    req("expected_version", Type::Int),
    opt("lang", Type::Str),
    opt("slug", Type::Str),
    opt("title", Type::Str),
    opt("markdown", Type::Str),
    opt("kind", Type::Str),
    opt("status", Type::Str),
    opt("published_at", Type::IntOrNull),
    opt("excerpt", Type::Str),
    opt("pinned", Type::Bool),
    opt("comments_enabled", Type::Bool),
    opt("cover_media_id", Type::IntOrNull),
    opt("series_id", Type::IntOrNull),
    opt("series_position", Type::IntOrNull),
    opt("tags", Type::IntList),
];

const ID: &[Field] = &[path("id")];

const MEDIA: Access = Access::Require(Permission::UploadMedia);
const SETTINGS: Access = Access::Require(Permission::ManageSettings);
const THEMES: Access = Access::Require(Permission::ManageThemes);
const PLUGINS: Access = Access::Require(Permission::ManagePlugins);

const MEDIA_FILTERS: &[Field] = &[
    query("missing_alt", Type::Bool),
    query("unused", Type::Bool),
    query("cursor", Type::Str),
    query("limit", Type::Int),
];
const MEDIA_UPLOAD: &[Field] = &[
    query_req("name", Type::Str),
    query_req("mime", Type::Str),
    query_req("width", Type::Int),
    query_req("height", Type::Int),
    query("alt", Type::Str),
];
/// An id-ordered listing: `cursor` is the last id of the previous page.
const BY_ID: &[Field] = &[query("cursor", Type::Str), query("limit", Type::Int)];
const VALUE: &[Field] = &[path_name("key"), req("value", Type::Json)];
const OWNER: &[Field] = &[path_name("name")];
const OWNER_VALUE: &[Field] = &[
    path_name("name"),
    path_name("key"),
    req("value", Type::Json),
];

const fn route(
    id: RouteId,
    method: Method,
    path: &'static str,
    access: Access,
    fields: &'static [Field],
    status: u16,
    response: &'static str,
) -> Route {
    Route {
        id,
        method,
        path,
        access,
        also: &[],
        session_only_keys: &[],
        fields,
        max_body: MAX_BODY,
        body: Body::Json,
        variants: None,
        status,
        response,
    }
}

/// The whole `/api/v1` surface.
pub const ROUTES: &[Route] = &[
    route(
        RouteId::Reference,
        Method::Get,
        "/api/v1/reference",
        Access::Read,
        &[],
        200,
        "reference",
    ),
    route(
        RouteId::Me,
        Method::Get,
        "/api/v1/me",
        Access::Read,
        &[],
        200,
        "me",
    ),
    route(
        RouteId::PostList,
        Method::Get,
        "/api/v1/posts",
        Access::Read,
        POST_FILTERS,
        200,
        "post_page",
    ),
    route(
        RouteId::PostGet,
        Method::Get,
        "/api/v1/posts/{id}",
        Access::Read,
        ID,
        200,
        "post",
    ),
    Route {
        also: PUBLISH_OR_ANY,
        max_body: MAX_POST_BODY,
        ..route(
            RouteId::PostCreate,
            Method::Post,
            "/api/v1/posts",
            WRITE,
            POST_CREATE,
            201,
            "post_saved",
        )
    },
    Route {
        also: PUBLISH_OR_ANY,
        max_body: MAX_POST_BODY,
        ..route(
            RouteId::PostUpdate,
            Method::Patch,
            "/api/v1/posts/{id}",
            WRITE,
            POST_UPDATE,
            200,
            "post_saved",
        )
    },
    Route {
        also: DELETE_ANY,
        ..route(
            RouteId::PostTrash,
            Method::Post,
            "/api/v1/posts/{id}/trash",
            WRITE,
            ID,
            200,
            "post",
        )
    },
    Route {
        also: DELETE_ANY,
        ..route(
            RouteId::PostRestore,
            Method::Post,
            "/api/v1/posts/{id}/restore",
            WRITE,
            ID,
            200,
            "post",
        )
    },
    Route {
        also: DELETE_ANY,
        ..route(
            RouteId::PostDelete,
            Method::Delete,
            "/api/v1/posts/{id}",
            WRITE,
            ID,
            204,
            "none",
        )
    },
    route(
        RouteId::TagList,
        Method::Get,
        "/api/v1/tags",
        Access::Read,
        PAGE_FILTERS,
        200,
        "tag_list",
    ),
    route(
        RouteId::TagGet,
        Method::Get,
        "/api/v1/tags/{id}",
        Access::Read,
        ID,
        200,
        "tag",
    ),
    route(
        RouteId::TagCreate,
        Method::Post,
        "/api/v1/tags",
        WRITE,
        &[req("slug", Type::Str), req("name", Type::Str)],
        201,
        "tag",
    ),
    route(
        RouteId::TagUpdate,
        Method::Patch,
        "/api/v1/tags/{id}",
        WRITE,
        &[path("id"), opt("slug", Type::Str), opt("name", Type::Str)],
        200,
        "tag",
    ),
    route(
        RouteId::TagDelete,
        Method::Delete,
        "/api/v1/tags/{id}",
        WRITE,
        ID,
        204,
        "none",
    ),
    route(
        RouteId::SeriesList,
        Method::Get,
        "/api/v1/series",
        Access::Read,
        PAGE_FILTERS,
        200,
        "series_list",
    ),
    route(
        RouteId::SeriesGet,
        Method::Get,
        "/api/v1/series/{id}",
        Access::Read,
        ID,
        200,
        "series",
    ),
    route(
        RouteId::SeriesCreate,
        Method::Post,
        "/api/v1/series",
        WRITE,
        &[
            req("slug", Type::Str),
            req("title", Type::Str),
            opt("description", Type::Str),
        ],
        201,
        "series",
    ),
    route(
        RouteId::SeriesUpdate,
        Method::Patch,
        "/api/v1/series/{id}",
        WRITE,
        &[
            path("id"),
            opt("slug", Type::Str),
            opt("title", Type::Str),
            opt("description", Type::Str),
        ],
        200,
        "series",
    ),
    route(
        RouteId::SeriesDelete,
        Method::Delete,
        "/api/v1/series/{id}",
        WRITE,
        ID,
        204,
        "none",
    ),
    route(
        RouteId::MediaList,
        Method::Get,
        "/api/v1/media",
        Access::Read,
        MEDIA_FILTERS,
        200,
        "media_page",
    ),
    route(
        RouteId::MediaGet,
        Method::Get,
        "/api/v1/media/{id}",
        Access::Read,
        ID,
        200,
        "media",
    ),
    Route {
        max_body: MAX_MEDIA_BODY,
        body: Body::Original,
        variants: Some(Variants::AdminEncoder),
        ..route(
            RouteId::MediaUpload,
            Method::Post,
            "/api/v1/media",
            MEDIA,
            MEDIA_UPLOAD,
            201,
            "media",
        )
    },
    route(
        RouteId::MediaUpdate,
        Method::Patch,
        "/api/v1/media/{id}",
        MEDIA,
        &[path("id"), req("alt", Type::Str)],
        200,
        "media",
    ),
    route(
        RouteId::MediaDelete,
        Method::Delete,
        "/api/v1/media/{id}",
        MEDIA,
        ID,
        204,
        "none",
    ),
    // A read token sees drafts and the media list already, so it may read originals too.
    route(
        RouteId::MediaOriginal,
        Method::Get,
        "/api/v1/media/{id}/original",
        Access::Read,
        ID,
        200,
        ORIGINAL,
    ),
    // Every status, so spam and trash included: moderation data, not for a read token.
    route(
        RouteId::CommentList,
        Method::Get,
        "/api/v1/comments",
        Access::Require(Permission::ModerateComments),
        BY_ID,
        200,
        "comment_page",
    ),
    route(
        RouteId::UserList,
        Method::Get,
        "/api/v1/users",
        Access::Require(Permission::ManageUsers),
        BY_ID,
        200,
        "user_page",
    ),
    route(
        RouteId::RedirectList,
        Method::Get,
        "/api/v1/redirects",
        Access::Read,
        &[],
        200,
        "redirect_list",
    ),
    route(
        RouteId::RedirectPut,
        Method::Put,
        "/api/v1/redirects",
        SETTINGS,
        &[req("from", Type::Str), req("to", Type::Str)],
        204,
        "none",
    ),
    route(
        RouteId::RedirectDelete,
        Method::Delete,
        "/api/v1/redirects",
        SETTINGS,
        &[query_req("from", Type::Str)],
        204,
        "none",
    ),
    route(
        RouteId::SettingList,
        Method::Get,
        "/api/v1/settings",
        Access::Read,
        &[],
        200,
        "values",
    ),
    Route {
        session_only_keys: SESSION_ONLY_SETTINGS,
        ..route(
            RouteId::SettingPut,
            Method::Put,
            "/api/v1/settings/{key}",
            SETTINGS,
            VALUE,
            204,
            "none",
        )
    },
    Route {
        session_only_keys: SESSION_ONLY_SETTINGS,
        ..route(
            RouteId::SettingDelete,
            Method::Delete,
            "/api/v1/settings/{key}",
            SETTINGS,
            &[path_name("key")],
            204,
            "none",
        )
    },
    route(
        RouteId::ThemeOptions,
        Method::Get,
        "/api/v1/themes/{name}/options",
        Access::Read,
        OWNER,
        200,
        "values",
    ),
    route(
        RouteId::ThemeOptionPut,
        Method::Put,
        "/api/v1/themes/{name}/options/{key}",
        THEMES,
        OWNER_VALUE,
        204,
        "none",
    ),
    route(
        RouteId::ThemeOptionsDelete,
        Method::Delete,
        "/api/v1/themes/{name}/options",
        THEMES,
        OWNER,
        204,
        "none",
    ),
    route(
        RouteId::PluginList,
        Method::Get,
        "/api/v1/plugins",
        Access::Read,
        &[],
        200,
        "plugin_list",
    ),
    route(
        RouteId::PluginPut,
        Method::Put,
        "/api/v1/plugins/{name}",
        PLUGINS,
        &[path_name("name"), req("enabled", Type::Bool)],
        204,
        "none",
    ),
    // Plugin options may hold a plugin's secrets: read like they are written.
    route(
        RouteId::PluginOptions,
        Method::Get,
        "/api/v1/plugins/{name}/options",
        PLUGINS,
        OWNER,
        200,
        "values",
    ),
    route(
        RouteId::PluginOptionPut,
        Method::Put,
        "/api/v1/plugins/{name}/options/{key}",
        PLUGINS,
        OWNER_VALUE,
        204,
        "none",
    ),
    route(
        RouteId::PluginOptionsDelete,
        Method::Delete,
        "/api/v1/plugins/{name}/options",
        PLUGINS,
        OWNER,
        204,
        "none",
    ),
];

/// Path field values of a matched route, in pattern order.
pub type PathValues<'a> = Vec<(&'static str, &'a str)>;

/// The values of `pattern`'s `{name}` segments in `path`, if it matches. A path value is one
/// non-empty segment.
#[must_use]
pub fn match_path<'a>(pattern: &'static str, path: &'a str) -> Option<PathValues<'a>> {
    let mut values = Vec::new();
    let mut want = pattern.split('/');
    let mut got = path.split('/');
    loop {
        match (want.next(), got.next()) {
            (None, None) => return Some(values),
            (Some(w), Some(g)) => {
                if let Some(name) = w.strip_prefix('{').and_then(|w| w.strip_suffix('}')) {
                    if g.is_empty() {
                        return None;
                    }
                    values.push((name, g));
                } else if w != g {
                    return None;
                }
            }
            _ => return None,
        }
    }
}

/// What a request to `method path` is: its route and path values; `Err(true)` when the path
/// exists with other methods, `Err(false)` when it does not exist at all.
///
/// # Errors
///
/// As above.
pub fn find(method: Method, path: &str) -> Result<(&'static Route, PathValues<'_>), bool> {
    let mut path_known = false;
    for r in ROUTES {
        if let Some(v) = match_path(r.path, path) {
            if r.method == method {
                return Ok((r, v));
            }
            path_known = true;
        }
    }
    Err(path_known)
}

#[cfg(test)]
mod tests {
    use super::{Access, Body, In, MAX_POST_BODY, Method, ORIGINAL, ROUTES, find, match_path};

    #[test]
    fn table_is_consistent() {
        for (i, r) in ROUTES.iter().enumerate() {
            assert!(r.path.starts_with("/api/v1/"), "{}", r.path);
            // No two routes answer the same request.
            for other in &ROUTES[i + 1..] {
                assert!(
                    !(r.method == other.method && r.path == other.path),
                    "{:?} {}",
                    r.method,
                    r.path
                );
                assert_ne!(r.id, other.id);
            }
            // Path fields are exactly the pattern's segments; reads take no body.
            let segs: Vec<_> = r
                .path
                .split('/')
                .filter_map(|s| s.strip_prefix('{').and_then(|s| s.strip_suffix('}')))
                .collect();
            let fields: Vec<_> = r
                .fields
                .iter()
                .filter(|f| f.at == In::Path)
                .map(|f| f.name)
                .collect();
            assert_eq!(segs, fields, "{}", r.path);
            if matches!(r.method, Method::Get | Method::Delete) {
                assert!(r.fields.iter().all(|f| f.at != In::Body), "{}", r.path);
            } else if r.body == Body::Json {
                assert!(r.fields.iter().all(|f| f.at != In::Query), "{}", r.path);
            } else {
                // A raw body carries no JSON fields; its metadata is in the query.
                assert!(r.fields.iter().all(|f| f.at != In::Body), "{}", r.path);
            }
            // Only the media upload takes more than the post cap, and only as raw bytes.
            assert!(
                r.max_body <= MAX_POST_BODY || r.body == Body::Original,
                "{}",
                r.path
            );
            if r.access == Access::Read || r.response == ORIGINAL {
                assert_eq!(r.method, Method::Get, "{}", r.path);
            }
            let names: Vec<_> = r.fields.iter().map(|f| f.name).collect();
            let mut dedup = names.clone();
            dedup.sort_unstable();
            dedup.dedup();
            assert_eq!(dedup.len(), names.len(), "{}", r.path);
            // Session-only keys name values of a `key` path field.
            assert!(
                r.session_only_keys.is_empty() || segs.contains(&"key"),
                "{}",
                r.path
            );
        }
    }

    #[test]
    fn matching() {
        assert_eq!(
            match_path("/api/v1/posts/{id}/trash", "/api/v1/posts/7/trash"),
            Some(vec![("id", "7")])
        );
        assert_eq!(match_path("/api/v1/posts/{id}", "/api/v1/posts/"), None);
        assert_eq!(match_path("/api/v1/posts/{id}", "/api/v1/posts/7/x"), None);
        assert_eq!(match_path("/api/v1/posts", "/api/v1/posts/"), None);
        assert!(find(Method::Get, "/api/v1/posts/3").is_ok());
        assert_eq!(find(Method::Post, "/api/v1/posts/3").err(), Some(true));
        assert_eq!(find(Method::Get, "/api/v1/nope").err(), Some(false));
    }
}
