//! The JSON shapes `/api/v1` answers with, described once so the admin's TypeScript is
//! generated (see `ts.rs`) instead of hand-written. The Worker still writes the JSON by hand
//! (`crates/worker/src/api/v1/posts.rs`); keep both in step, key for key and in order.
//! The code lists mirror enums of crates that model cannot depend on; `crates/store` has a
//! test that they stay equal.

/// `PostStatus` in `crates/store`.
pub const POST_STATUSES: &[&str] = &["draft", "scheduled", "published", "trashed"];
/// `PostKind` in `crates/store`.
pub const POST_KINDS: &[&str] = &["post", "page"];
/// `plumblog_markdown::WARNING_CODES`.
pub const RENDER_WARNINGS: &[&str] = &["markdown.math_invalid"];

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
        constant: "RENDER_WARNINGS",
        ty: "RenderWarning",
        values: RENDER_WARNINGS,
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
    Bool,
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
        name: "conflict",
        extends: None,
        props: CONFLICT,
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
    /// `string`, `integer`, `boolean`, `code:<CONST>` or `shape:<name>`, then `[]` for a
    /// list, then `|null` for nullable.
    #[must_use]
    pub fn descriptor(&self) -> String {
        let mut d = match self.ty {
            Ty::Str => "string".to_owned(),
            Ty::Int => "integer".to_owned(),
            Ty::Bool => "boolean".to_owned(),
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
