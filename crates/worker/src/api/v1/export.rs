//! Id-ordered listings a full export pages through (`guides/export.md`): every comment, any
//! status, and every user without any secret. Keyset-paged by the primary key.

use plumblog_auth::base64url;
use plumblog_model::api::{DEFAULT_LIMIT, MAX_LIMIT};
use plumblog_model::{CommentId, Error, ErrorCode, Result, UserId};
use plumblog_store::{Comment, CommentStore, User, UserStore};

use super::Input;
use crate::api::body::{Out, object};
use crate::api::{Ctx, Reply};

fn invalid(field: &str, why: &str) -> Error {
    Error::new(ErrorCode::InvalidInput, format!("{field}: {why}")).at(field)
}

fn limit(input: &Input) -> Result<u32> {
    let limit = input.int("limit").unwrap_or(i64::from(DEFAULT_LIMIT));
    u32::try_from(limit)
        .ok()
        .filter(|l| (1..=MAX_LIMIT).contains(l))
        .ok_or_else(|| invalid("limit", "1-100"))
}

/// The last id of the previous page, from `cursor`.
fn after(input: &Input) -> Result<Option<i64>> {
    let Some(text) = input.str("cursor") else {
        return Ok(None);
    };
    let bad = || invalid("cursor", "not a cursor of this listing");
    let raw = String::from_utf8(base64url::decode(text).ok_or_else(bad)?).map_err(|_| bad())?;
    let id: i64 = raw.parse().map_err(|_| bad())?;
    if id <= 0 {
        return Err(bad());
    }
    Ok(Some(id))
}

fn page(key: &str, items: &[String], last: Option<i64>, more: bool) -> Reply {
    let next = match last {
        Some(id) if more => Out::Str(base64url::encode(id.to_string().as_bytes())),
        _ => Out::Null,
    };
    Reply::json(
        200,
        object(&[
            (key, Out::Raw(format!("[{}]", items.join(",")))),
            ("next_cursor", next),
        ]),
    )
}

fn opt_str(v: Option<&String>) -> Out {
    v.map_or(Out::Null, |s| Out::Str(s.clone()))
}

/// Key for key as `shape::COMMENT`.
fn comment_json(c: &Comment) -> String {
    object(&[
        ("id", Out::Int(c.id.0)),
        ("post_id", Out::Int(c.post_id.0)),
        (
            "parent_id",
            c.parent_id.map_or(Out::Null, |p| Out::Int(p.0)),
        ),
        ("author_name", Out::Str(c.author_name.clone())),
        ("body", Out::Str(c.body.clone())),
        ("body_html", opt_str(c.body_html.as_ref())),
        ("status", Out::Str(c.status.as_str().to_owned())),
        ("created_at", Out::Int(c.created_at)),
    ])
}

/// Key for key as `shape::USER`. Fields are named one by one so a secret added to [`User`]
/// never reaches the answer by default.
fn user_json(u: &User) -> String {
    object(&[
        ("id", Out::Int(u.id.0)),
        ("login", Out::Str(u.login.clone())),
        ("slug", Out::Str(u.slug.clone())),
        ("display_name", Out::Str(u.display_name.clone())),
        ("bio", Out::Str(u.bio.clone())),
        ("role", Out::Str(u.role.as_str().to_owned())),
        ("disabled_at", u.disabled_at.map_or(Out::Null, Out::Int)),
        ("created_at", Out::Int(u.created_at)),
        ("updated_at", Out::Int(u.updated_at)),
    ])
}

/// `GET /api/v1/comments`: every comment by id, any status.
pub async fn comments(ctx: &Ctx, input: &Input) -> Result<Reply> {
    let n = limit(input)?;
    let after = after(input)?.map(CommentId);
    let (rows, more) = ctx.store.comments_by_id(after, n).await?;
    let items: Vec<String> = rows.iter().map(comment_json).collect();
    Ok(page("comments", &items, rows.last().map(|c| c.id.0), more))
}

/// `GET /api/v1/users`: every user by id, disabled ones included.
pub async fn users(ctx: &Ctx, input: &Input) -> Result<Reply> {
    let n = limit(input)?;
    let after = after(input)?.map(UserId);
    let (rows, more) = ctx.store.users_by_id(after, n).await?;
    let items: Vec<String> = rows.iter().map(user_json).collect();
    Ok(page("users", &items, rows.last().map(|u| u.id.0), more))
}

#[cfg(test)]
mod tests {
    use super::user_json;
    use plumblog_model::{Role, UserId};
    use plumblog_store::User;

    #[test]
    fn user_json_holds_no_password_hash() {
        let u = User {
            id: UserId(1),
            login: "olga".into(),
            slug: "olga".into(),
            display_name: "Olga".into(),
            bio: String::new(),
            role: Role::Owner,
            password_hash: Some("pbkdf2$1000$salt$secret-hash".into()),
            disabled_at: None,
            created_at: 1,
            updated_at: 2,
        };
        let j = user_json(&u);
        assert!(!j.contains("password"), "{j}");
        assert!(!j.contains("secret-hash"), "{j}");
        assert_eq!(
            j,
            r#"{"id":1,"login":"olga","slug":"olga","display_name":"Olga","bio":"","role":"owner","disabled_at":null,"created_at":1,"updated_at":2}"#
        );
    }
}
