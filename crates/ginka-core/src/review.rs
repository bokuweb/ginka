//! Comments left on a diff, and the message they become.
//!
//! Orca's idea, and the reason the diff carries a line number on every line:
//! re-prompting an agent from scratch after reading its work throws away the
//! reading. Marking the three places that are wrong and sending those is a
//! better feedback loop, and it is cheap on top of a diff view.
//!
//! Comments are anchored to a file and a line rather than to a diff, because
//! the diff is regenerated on every read and its hunks move while "line 42 of
//! `src/main.rs`" stays what the reader meant. They belong to the workspace
//! rather than the session: an agent can finish, the user can read the diff
//! over lunch, and the comments are still about the same worktree.

use anyhow::Result;
use ginka_protocol::WorkspaceId;
use ginka_protocol::model::{DiffSide, ReviewComment};
use rusqlite::Connection;

/// The stored value for a side.
fn side_str(side: DiffSide) -> &'static str {
    match side {
        DiffSide::Old => "old",
        DiffSide::New => "new",
    }
}

/// Read a stored side, defaulting to the new one.
///
/// A comment is almost always about the file as it now is, and a row this
/// build cannot read is better shown against the current file than dropped.
fn side_from(value: &str) -> DiffSide {
    match value {
        "old" => DiffSide::Old,
        _ => DiffSide::New,
    }
}

/// Leave a comment.
pub fn add(
    conn: &Connection,
    workspace: &WorkspaceId,
    path: &str,
    line: Option<u32>,
    side: DiffSide,
    text: &str,
    now: i64,
) -> Result<ReviewComment> {
    let comment = ReviewComment {
        id: uuid::Uuid::new_v4().simple().to_string(),
        workspace: workspace.clone(),
        path: path.to_string(),
        line,
        side,
        text: text.trim().to_string(),
        created_at: now,
    };
    conn.execute(
        "INSERT INTO review_comments (id, workspace_id, path, line, side, text, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            comment.id,
            comment.workspace.0,
            comment.path,
            comment.line,
            side_str(comment.side),
            comment.text,
            comment.created_at,
        ],
    )?;
    Ok(comment)
}

/// Every comment waiting in a workspace, in reading order.
///
/// By file and then by line, which is the order someone reads a diff in and
/// therefore the order the agent should be given them in.
pub fn list(conn: &Connection, workspace: &WorkspaceId) -> Result<Vec<ReviewComment>> {
    let mut statement = conn.prepare(
        "SELECT id, workspace_id, path, line, side, text, created_at
           FROM review_comments
          WHERE workspace_id = ?1
          ORDER BY path, line, created_at",
    )?;
    let rows = statement.query_map([&workspace.0], |row| {
        Ok(ReviewComment {
            id: row.get(0)?,
            workspace: WorkspaceId(row.get(1)?),
            path: row.get(2)?,
            line: row.get(3)?,
            side: side_from(&row.get::<_, String>(4)?),
            text: row.get(5)?,
            created_at: row.get(6)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Take one comment back.
pub fn remove(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn.execute("DELETE FROM review_comments WHERE id = ?1", [id])? > 0)
}

/// Forget every comment in a workspace, once they have been sent.
pub fn clear(conn: &Connection, workspace: &WorkspaceId) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM review_comments WHERE workspace_id = ?1",
        [&workspace.0],
    )?)
}

/// The message a batch of comments becomes.
///
/// One message rather than one per comment: an agent given them together can
/// see that three of them are the same mistake, and a turn per comment is
/// three times the context and three times the cost.
///
/// The file and line are written the way a person would write them, because
/// that is what every agent has been trained to read — `src/main.rs:42` is
/// unambiguous to a model and to the human reading the transcript later.
pub fn compose(comments: &[ReviewComment]) -> String {
    if comments.is_empty() {
        return String::new();
    }

    let mut message = String::from(
        "I read the diff and left comments. Please address each one, and say what you \
         changed for each:\n\n",
    );
    let mut current = "";
    for comment in comments {
        if comment.path != current {
            current = &comment.path;
            message.push_str(&format!("{}\n", comment.path));
        }
        match comment.line {
            Some(line) => {
                message.push_str(&format!("  {}:{line} — {}\n", comment.path, comment.text))
            }
            None => message.push_str(&format!("  {} — {}\n", comment.path, comment.text)),
        }
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    fn workspace() -> WorkspaceId {
        WorkspaceId("comet/harbor".into())
    }

    fn comment(path: &str, line: Option<u32>, text: &str) -> ReviewComment {
        ReviewComment {
            id: format!("{path}:{line:?}"),
            workspace: workspace(),
            path: path.into(),
            line,
            side: DiffSide::New,
            text: text.into(),
            created_at: 0,
        }
    }

    #[test]
    fn comments_come_back_in_the_order_a_diff_is_read_in() {
        // By file and then by line, which is also the order the agent should
        // be given them in.
        let conn = db::open_in_memory().unwrap();
        add(
            &conn,
            &workspace(),
            "src/main.rs",
            Some(80),
            DiffSide::New,
            "later",
            2,
        )
        .unwrap();
        add(
            &conn,
            &workspace(),
            "src/main.rs",
            Some(12),
            DiffSide::New,
            "earlier",
            1,
        )
        .unwrap();
        add(
            &conn,
            &workspace(),
            "README.md",
            Some(1),
            DiffSide::New,
            "first file",
            3,
        )
        .unwrap();

        let listed = list(&conn, &workspace()).unwrap();
        let order: Vec<(&str, Option<u32>)> = listed
            .iter()
            .map(|comment| (comment.path.as_str(), comment.line))
            .collect();
        assert_eq!(
            order,
            vec![
                ("README.md", Some(1)),
                ("src/main.rs", Some(12)),
                ("src/main.rs", Some(80))
            ]
        );
    }

    #[test]
    fn a_comment_on_a_whole_file_is_allowed() {
        // People write "this file should not exist".
        let conn = db::open_in_memory().unwrap();
        add(
            &conn,
            &workspace(),
            "junk.rs",
            None,
            DiffSide::New,
            "delete this",
            1,
        )
        .unwrap();
        assert_eq!(list(&conn, &workspace()).unwrap()[0].line, None);
    }

    #[test]
    fn comments_belong_to_their_own_workspace() {
        let conn = db::open_in_memory().unwrap();
        add(
            &conn,
            &workspace(),
            "a.rs",
            Some(1),
            DiffSide::New,
            "mine",
            1,
        )
        .unwrap();
        add(
            &conn,
            &WorkspaceId("comet/other".into()),
            "b.rs",
            Some(1),
            DiffSide::New,
            "theirs",
            1,
        )
        .unwrap();
        assert_eq!(list(&conn, &workspace()).unwrap().len(), 1);
    }

    #[test]
    fn a_comment_can_be_taken_back_and_a_batch_cleared() {
        let conn = db::open_in_memory().unwrap();
        let kept = add(
            &conn,
            &workspace(),
            "a.rs",
            Some(1),
            DiffSide::New,
            "keep",
            1,
        )
        .unwrap();
        let regretted = add(
            &conn,
            &workspace(),
            "a.rs",
            Some(2),
            DiffSide::New,
            "oops",
            1,
        )
        .unwrap();

        assert!(remove(&conn, &regretted.id).unwrap());
        assert!(!remove(&conn, "never-existed").unwrap());
        assert_eq!(list(&conn, &workspace()).unwrap(), vec![kept]);

        assert_eq!(clear(&conn, &workspace()).unwrap(), 1);
        assert!(list(&conn, &workspace()).unwrap().is_empty());
    }

    #[test]
    fn a_batch_becomes_one_message_grouped_by_file() {
        // One message rather than one per comment: an agent given them
        // together can see that three of them are the same mistake.
        let message = compose(&[
            comment("src/main.rs", Some(12), "this unwrap can panic"),
            comment("src/main.rs", Some(48), "same here"),
            comment("README.md", None, "out of date"),
        ]);

        assert!(
            message.contains("src/main.rs:12 — this unwrap can panic"),
            "{message}"
        );
        assert!(message.contains("src/main.rs:48 — same here"), "{message}");
        assert!(message.contains("README.md — out of date"), "{message}");
        assert_eq!(
            message.matches("src/main.rs\n").count(),
            1,
            "the file is named once as a heading: {message}"
        );
    }

    #[test]
    fn nothing_to_say_is_no_message_at_all() {
        assert_eq!(compose(&[]), "");
    }
}
