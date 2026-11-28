//! The JSON shapes `/api/v1` answers with, described once so the admin's TypeScript is
//! generated (see `ts.rs`) instead of hand-written. The Worker still writes the JSON by hand
//! (`crates/worker/src/api/v1/posts.rs`); keep both in step, key for key and in order.
//! The code lists mirror enums of crates that model cannot depend on; `crates/store` has a
//! test that they stay equal.

/// `PostStatus` in `crates/store`.
pub const POST_STATUSES: &[&str] = &["draft", "scheduled", "published", "trashed"];
/// `PostKind` in `crates/store`.
pub const POST_KINDS: &[&str] = &["post", "page"];
/// `CommentStatus` in `crates/store`.
pub const COMMENT_STATUSES: &[&str] = &["pending", "approved", "spam", "trashed"];
/// `plumblog_markdown::WARNING_CODES`.
pub const RENDER_WARNINGS: &[&str] = &["markdown.math_invalid"];
/// `plumblog_render::options::FieldType` names, the admin's fixed field set.
pub const OPTION_TYPES: &[&str] = &[
    "short_text",
    "long_text",
    "number",
    "toggle",
    "choice",
    "color",
    "image",
    "list",
];
/// Where an installed theme comes from: built into the Worker, or `blog theme push`.
pub const THEME_SOURCES: &[&str] = &["embedded", "pushed"];

/// A named list of string codes and the TypeScript names it is emitted under.
#[derive(Clone, Copy, Debug)]
pub struct CodeList {
    /// The TS constant, e.g. `POST_STATUSES`; also what shape descriptors say (`code:<it>`).
    pub constant: &'static str,
    /// The TS union type, e.g. `PostStatus`.
    pub ty: &'static str,
    pub values: &'static [&'static str],
}

pub const CODE_LISTS: &[CodeList] = &[
    CodeList {
        constant: "POST_STATUSES",
        ty: "PostStatus",
        values: POST_STATUSES,
    },
    CodeList {
        constant: "POST_KINDS",
        ty: "PostKind",
        values: POST_KINDS,
    },
    CodeList {
        constant: "COMMENT_STATUSES",
        ty: "CommentStatus",
        values: COMMENT_STATUSES,
    },
    CodeList {
        constant: "RENDER_WARNINGS",
        ty: "RenderWarning",
        values: RENDER_WARNINGS,
    },
    CodeList {
        constant: "OPTION_TYPES",
        ty: "OptionType",
        values: OPTION_TYPES,
    },
    CodeList {
        constant: "THEME_SOURCES",
        ty: "ThemeSource",
        values: THEME_SOURCES,
    },
];

/// The code list named `constant`.
#[must_use]
pub fn code_list(constant: &str) -> Option<&'static CodeList> {
    CODE_LISTS.iter().find(|l| l.constant == constant)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ty {
    Str,
    Int,
    /// Any JSON number, fractions included (option limits).
    Num,
    Bool,
    /// Any JSON value (an option's default, whose type follows its field type).
    Json,
    /// A string from the code list with this constant name.
    Code(&'static str),
    /// Another shape, by name.
    Shape(&'static str),
}

#[derive(Clone, Copy, Debug)]
pub struct Prop {
    pub name: &'static str,
    pub ty: Ty,
    pub list: bool,
    pub null: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Shape {
    /// `snake_case`; the TS interface is its `PascalCase`.
    pub name: &'static str,
    /// A shape whose props come first.
    pub extends: Option<&'static str>,
    /// Own props, in the order the Worker writes them.
    pub props: &'static [Prop],
}

const fn p(name: &'static str, ty: Ty) -> Prop {
    Prop {
        name,
        ty,
        list: false,
        null: false,
    }
}
const fn nullable(name: &'static str, ty: Ty) -> Prop {
    Prop {
        name,
        ty,
        list: false,
        null: true,
    }
}
const fn list(name: &'static str, ty: Ty) -> Prop {
    Prop {
        name,
        ty,
        list: true,
        null: false,
    }
}

const SUMMARY: &[Prop] = &[
    p("id", Ty::Int),
    p("kind", Ty::Code("POST_KINDS")),
    p("translation_group", Ty::Int),
    p("lang", Ty::Str),
    p("slug", Ty::Str),
    p("status", Ty::Code("POST_STATUSES")),
    nullable("trashed_from", Ty::Code("POST_STATUSES")),
    nullable("published_at", Ty::Int),
    p("author_id", Ty::Int),
    p("title", Ty::Str),
    p("excerpt", Ty::Str),
    p("word_count", Ty::Int),
    p("reading_minutes", Ty::Int),
    p("pinned", Ty::Bool),
    p("comments_enabled", Ty::Bool),
    nullable("cover_media_id", Ty::Int),
    nullable("series_id", Ty::Int),
    nullable("series_position", Ty::Int),
    p("created_at", Ty::Int),
    p("updated_at", Ty::Int),
    p("version", Ty::Int),
    list("tags", Ty::Int),
];

const FULL: &[Prop] = &[
    p("markdown", Ty::Str),
    p("html", Ty::Str),
    list("media_ids", Ty::Int),
    list("warnings", Ty::Code("RENDER_WARNINGS")),
];

const PAGE: &[Prop] = &[
    list("posts", Ty::Shape("post_summary")),
    nullable("next_cursor", Ty::Str),
    nullable("counts", Ty::Shape("post_counts")),
];

/// Posts per status; every key is always there.
const COUNTS: &[Prop] = &[
    p("draft", Ty::Int),
    p("scheduled", Ty::Int),
    p("published", Ty::Int),
    p("trashed", Ty::Int),
];

/// Every comment, any status (`GET /api/v1/comments`, for export).
const COMMENT: &[Prop] = &[
    p("id", Ty::Int),
    p("post_id", Ty::Int),
    nullable("parent_id", Ty::Int),
    p("author_name", Ty::Str),
    p("body", Ty::Str),
    // `None` for rows from before migration 0004.
    nullable("body_html", Ty::Str),
    p("status", Ty::Code("COMMENT_STATUSES")),
    p("created_at", Ty::Int),
];

const COMMENT_PAGE: &[Prop] = &[
    list("comments", Ty::Shape("comment")),
    nullable("next_cursor", Ty::Str),
];

/// A user as `GET /api/v1/users` answers it: never the password hash, sessions, passkeys or
/// tokens. `role` is a `Role` code (`ROLES` in the TS), which is emitted from the enum, not
/// from a code list here.
const USER: &[Prop] = &[
    p("id", Ty::Int),
    p("login", Ty::Str),
    p("slug", Ty::Str),
    p("display_name", Ty::Str),
    p("bio", Ty::Str),
    p("role", Ty::Str),
    nullable("disabled_at", Ty::Int),
    p("created_at", Ty::Int),
    p("updated_at", Ty::Int),
];

const USER_PAGE: &[Prop] = &[
    list("users", Ty::Shape("user")),
    nullable("next_cursor", Ty::Str),
];

/// `GET /api/v1/themes`: the embedded default and the pushed theme, if one is bound.
const THEME_LIST: &[Prop] = &[list("themes", Ty::Shape("theme_info"))];

/// `available` is false only for a pushed theme that did not load (the default renders).
const THEME_INFO: &[Prop] = &[
    p("name", Ty::Str),
    nullable("version", Ty::Str),
    p("active", Ty::Bool),
    p("source", Ty::Code("THEME_SOURCES")),
    p("available", Ty::Bool),
];

/// A theme's or plugin's option declarations, serialised from `plumblog_render`'s own types
/// (`Schema::declarations`), with text in `lang`.
const DECLARATIONS: &[Prop] = &[
    p("name", Ty::Str),
    nullable("version", Ty::Str),
    p("lang", Ty::Str),
    list("groups", Ty::Shape("option_group")),
    list("options", Ty::Shape("option_declaration")),
];

const OPTION_GROUP: &[Prop] = &[
    p("key", Ty::Str),
    p("label", Ty::Str),
    p("label_key", Ty::Str),
    nullable("icon", Ty::Str),
];

/// Limits are the ones the write check applies (`max_len`, `max_items` filled in when the
/// declaration leaves them out); `null` where the type has none.
const OPTION_DECLARATION: &[Prop] = &[
    p("key", Ty::Str),
    p("label", Ty::Str),
    nullable("help", Ty::Str),
    p("type", Ty::Code("OPTION_TYPES")),
    p("label_key", Ty::Str),
    nullable("help_key", Ty::Str),
    nullable("group", Ty::Str),
    nullable("icon", Ty::Str),
    nullable("token", Ty::Str),
    nullable("min", Ty::Num),
    nullable("max", Ty::Num),
    nullable("step", Ty::Num),
    p("default", Ty::Json),
    list("choices", Ty::Shape("option_choice")),
    nullable("max_len", Ty::Int),
    nullable("max_items", Ty::Int),
];

/// An offered choice; drafts are left out.
const OPTION_CHOICE: &[Prop] = &[
    p("label", Ty::Str),
    p("value", Ty::Str),
    p("label_key", Ty::Str),
];

const CONFLICT: &[Prop] = &[
    p("error", Ty::Str),
    p("field", Ty::Str),
    p("version", Ty::Int),
];

pub const SHAPES: &[Shape] = &[
    Shape {
        name: "post_summary",
        extends: None,
        props: SUMMARY,
    },
    Shape {
        name: "post",
        extends: Some("post_summary"),
        props: FULL,
    },
    Shape {
        name: "post_saved",
        extends: Some("post_summary"),
        props: FULL,
    },
    Shape {
        name: "post_page",
        extends: None,
        props: PAGE,
    },
    Shape {
        name: "post_counts",
        extends: None,
        props: COUNTS,
    },
    Shape {
        name: "comment",
        extends: None,
        props: COMMENT,
    },
    Shape {
        name: "comment_page",
        extends: None,
        props: COMMENT_PAGE,
    },
    Shape {
        name: "user",
        extends: None,
        props: USER,
    },
    Shape {
        name: "user_page",
        extends: None,
        props: USER_PAGE,
    },
    Shape {
        name: "conflict",
        extends: None,
        props: CONFLICT,
    },
    Shape {
        name: "theme_list",
        extends: None,
        props: THEME_LIST,
    },
    Shape {
        name: "theme_info",
        extends: None,
        props: THEME_INFO,
    },
    Shape {
        name: "option_declarations",
        extends: None,
        props: DECLARATIONS,
    },
    Shape {
        name: "option_group",
        extends: None,
        props: OPTION_GROUP,
    },
    Shape {
        name: "option_declaration",
        extends: None,
        props: OPTION_DECLARATION,
    },
    Shape {
        name: "option_choice",
        extends: None,
        props: OPTION_CHOICE,
    },
];

#[must_use]
pub fn shape(name: &str) -> Option<&'static Shape> {
    SHAPES.iter().find(|s| s.name == name)
}

/// Every prop of a shape, inherited ones first.
#[must_use]
pub fn all_props(s: &Shape) -> Vec<Prop> {
    let mut out = s.extends.and_then(shape).map(all_props).unwrap_or_default();
    out.extend_from_slice(s.props);
    out
}

impl Prop {
    /// `string`, `integer`, `number`, `boolean`, `json` (any value), `code:<CONST>` or
    /// `shape:<name>`, then `[]` for a
    /// list, then `|null` for nullable.
    #[must_use]
    pub fn descriptor(&self) -> String {
        let mut d = match self.ty {
            Ty::Str => "string".to_owned(),
            Ty::Int => "integer".to_owned(),
            Ty::Num => "number".to_owned(),
            Ty::Bool => "boolean".to_owned(),
            Ty::Json => "json".to_owned(),
            Ty::Code(c) => format!("code:{c}"),
            Ty::Shape(s) => format!("shape:{s}"),
        };
        if self.list {
            d.push_str("[]");
        }
        if self.null {
            d.push_str("|null");
        }
        d
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{ROUTES, RouteId};

    #[test]
    fn post_routes_answer_with_a_defined_shape() {
        for r in ROUTES {
            let is_post = format!("{:?}", r.id).starts_with("Post");
            if is_post || shape(r.response).is_some() {
                assert!(
                    r.response == "none" || shape(r.response).is_some(),
                    "{:?} answers `{}`, which has no shape",
                    r.id,
                    r.response
                );
            }
        }
        assert!(ROUTES.iter().any(|r| r.id == RouteId::PostList));
    }

    #[test]
    fn shapes_reference_only_known_names() {
        for s in SHAPES {
            assert!(s.extends.is_none_or(|e| shape(e).is_some()), "{}", s.name);
            for prop in all_props(s) {
                match prop.ty {
                    Ty::Code(c) => assert!(code_list(c).is_some(), "{}.{}", s.name, prop.name),
                    Ty::Shape(n) => assert!(shape(n).is_some(), "{}.{}", s.name, prop.name),
                    _ => {}
                }
            }
        }
    }
}
