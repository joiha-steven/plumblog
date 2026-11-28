//! The id-ordered listings a full export pages through: every comment and every user.

use plumblog_model::{CommentId, Role, UserId};

use super::kit::{ADMIN, Check, T0, eq, new_user, ok, post, save, user};
use super::posts::comment;
use crate::{Comment, CommentStatus, MAX_PAGE, PostStatus, Store, User};

fn comment_ids(rows: &[Comment]) -> Vec<CommentId> {
    rows.iter().map(|c| c.id).collect()
}

fn user_ids(rows: &[User]) -> Vec<UserId> {
    rows.iter().map(|u| u.id).collect()
}

pub async fn comments_by_id_pages_every_status<S: Store>(db: &S) -> Check {
    let (rows, more) = ok("comments_by_id", db.comments_by_id(None, 10).await)?;
    eq!(
        "empty store: no rows, no more",
        (rows.len(), more),
        (0, false)
    )?;

    let u = user(db, "alice").await?;
    let p = save(db, post(u, "en", "p", PostStatus::Published)).await?;
    let q = save(db, post(u, "en", "q", PostStatus::Draft)).await?;
    // Created out of id order and over two posts: the listing follows the id alone.
    let mut ids = Vec::new();
    for (i, (on, status)) in [
        (p, CommentStatus::Approved),
        (q, CommentStatus::Pending),
        (p, CommentStatus::Spam),
        (p, CommentStatus::Trashed),
        (q, CommentStatus::Approved),
    ]
    .into_iter()
    .enumerate()
    {
        let mut c = comment(on, None, status);
        c.created_at = T0 + 100 - i64::try_from(i).unwrap_or(0) * 10;
        if i == 2 {
            c.parent_id = Some(ids[0]);
        }
        ids.push(ok("add_comment", db.add_comment(c).await)?);
    }

    let (first, more) = ok("comments_by_id", db.comments_by_id(None, 2).await)?;
    eq!("page 1", comment_ids(&first), ids[..2].to_vec())?;
    eq!("page 1 has more", more, true)?;
    let whole = ok("get_comment", db.get_comment(ids[0]).await)?;
    eq!("rows are whole", first[0], whole)?;
    let (second, more) = ok("comments_by_id", db.comments_by_id(Some(ids[1]), 2).await)?;
    eq!("page 2", comment_ids(&second), ids[2..4].to_vec())?;
    eq!("page 2 has more", more, true)?;
    eq!("reply keeps its parent", second[0].parent_id, Some(ids[0]))?;
    let (last, more) = ok("comments_by_id", db.comments_by_id(Some(ids[3]), 2).await)?;
    eq!("page 3", comment_ids(&last), ids[4..].to_vec())?;
    eq!("page 3 is the last", more, false)?;
    let (exact, more) = ok("comments_by_id", db.comments_by_id(None, 5).await)?;
    eq!("a full page", comment_ids(&exact), ids.clone())?;
    eq!("a full page that is the last", more, false)?;
    let (past, more) = ok("comments_by_id", db.comments_by_id(Some(ids[4]), 2).await)?;
    eq!("past the end", (past.len(), more), (0, false))?;
    Ok(())
}

pub async fn users_by_id_pages<S: Store>(db: &S) -> Check {
    let mut owner = new_user("Olga");
    owner.role = Role::Owner;
    owner.password_hash = Some("hash".into());
    let a = ok("create_user", db.create_user(owner).await)?;
    let b = user(db, "bob").await?;
    let c = user(db, "carol").await?;
    ok(
        "set_disabled",
        db.set_disabled(&ADMIN, c, Some(T0 + 5), T0 + 5).await,
    )?;

    let (first, more) = ok("users_by_id", db.users_by_id(None, 2).await)?;
    eq!("page 1", user_ids(&first), vec![a, b])?;
    eq!("page 1 has more", more, true)?;
    let whole = ok("get_user", db.get_user(a).await)?;
    eq!("rows are whole", first[0], whole)?;
    let (last, more) = ok("users_by_id", db.users_by_id(Some(b), 2).await)?;
    eq!("page 2", user_ids(&last), vec![c])?;
    eq!("page 2 is the last", more, false)?;
    eq!(
        "disabled users are listed",
        last[0].disabled_at,
        Some(T0 + 5)
    )?;
    let (past, more) = ok("users_by_id", db.users_by_id(Some(c), 2).await)?;
    eq!("past the end", (past.len(), more), (0, false))?;
    Ok(())
}

/// A full page of [`MAX_PAGE`] still says whether one more row follows: the store reads one
/// row past the cap itself, so `limit = 100` with 101 rows is not reported as the last page.
pub async fn export_pages_past_max_page<S: Store>(db: &S) -> Check {
    let u = user(db, "alice").await?;
    let p = save(db, post(u, "en", "p", PostStatus::Published)).await?;
    let over = MAX_PAGE + 1;
    for i in 0..over {
        ok(
            "add_comment",
            db.add_comment(comment(p, None, CommentStatus::Pending))
                .await,
        )?;
        // `alice` is user 1 of 101.
        if i + 1 < over {
            user(db, &format!("u{i}")).await?;
        }
    }
    let (first, more) = ok("comments_by_id", db.comments_by_id(None, MAX_PAGE).await)?;
    eq!("comments page 1", (first.len(), more), (100, true))?;
    let last = first.last().map(|c| c.id);
    let (rest, more) = ok("comments_by_id", db.comments_by_id(last, MAX_PAGE).await)?;
    eq!("comments page 2", (rest.len(), more), (1, false))?;
    let (first, more) = ok("users_by_id", db.users_by_id(None, MAX_PAGE).await)?;
    eq!("users page 1", (first.len(), more), (100, true))?;
    let last = first.last().map(|u| u.id);
    let (rest, more) = ok("users_by_id", db.users_by_id(last, MAX_PAGE).await)?;
    eq!("users page 2", (rest.len(), more), (1, false))?;
    Ok(())
}
