//! Git-backed checkpoints: a transcript position that maps to a working tree.
//!
//! An agent's work is a sequence of turns, and the interesting question after a
//! bad one is "put it back to how it was three turns ago". A commit on no
//! branch, held alive by a ref under `refs/ginka/checkpoints/`, answers that
//! without touching the user's history — see [`crate::git::snapshot`].
//!
//! Checkpoints are taken by the supervisor at turn boundaries. Restoring is
//! destructive, so it takes one of the current state first: a rewind must
//! never be the thing that loses work.

use crate::git;
use anyhow::Result;
use ginka_protocol::model::Checkpoint;
use ginka_protocol::{CheckpointId, SessionId, WorkspaceId};
use rusqlite::Connection;
use std::path::Path;

/// The longest label kept for the rewind menu.
const LABEL_LIMIT: usize = 80;

/// The ref that holds a checkpoint's commit alive.
///
/// Under `refs/ginka/` rather than `refs/heads/` so it is invisible to
/// `git branch`, `git log` and the user's own tooling, while still being a
/// reachable root that garbage collection will not take.
pub fn reference(id: &CheckpointId) -> String {
    format!("refs/ginka/checkpoints/{}", id.0)
}

/// Snapshot a worktree and record the checkpoint.
pub fn take(
    conn: &Connection,
    workspace_path: &Path,
    workspace: &WorkspaceId,
    session: &SessionId,
    turn: u32,
    label: &str,
    now: i64,
) -> Result<Checkpoint> {
    let id = CheckpointId(uuid::Uuid::new_v4().simple().to_string());
    let label = trim_label(label);
    let commit = git::snapshot(
        workspace_path,
        &reference(&id),
        &format!("ginka checkpoint: {label}"),
    )?;

    let checkpoint = Checkpoint {
        id,
        session: session.clone(),
        workspace: workspace.clone(),
        turn,
        commit,
        label,
        created_at: now,
    };
    insert(conn, &checkpoint)?;
    Ok(checkpoint)
}

/// Store a checkpoint that has already been taken.
pub fn insert(conn: &Connection, checkpoint: &Checkpoint) -> Result<()> {
    conn.execute(
        "INSERT INTO checkpoints
            (id, session_id, workspace_id, turn, commit_id, label, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            checkpoint.id.0,
            checkpoint.session.0,
            checkpoint.workspace.0,
            checkpoint.turn,
            checkpoint.commit,
            checkpoint.label,
            checkpoint.created_at,
        ],
    )?;
    Ok(())
}

/// Every checkpoint taken in a workspace, newest first.
pub fn list(conn: &Connection, workspace: &WorkspaceId) -> Result<Vec<Checkpoint>> {
    let mut statement = conn.prepare(&format!("{SELECT} WHERE workspace_id = ?1 {ORDER}"))?;
    let rows = statement.query_map([&workspace.0], read)?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// One checkpoint by id.
pub fn get(conn: &Connection, id: &CheckpointId) -> Result<Option<Checkpoint>> {
    let mut statement = conn.prepare(&format!("{SELECT} WHERE id = ?1"))?;
    let mut rows = statement.query_map([&id.0], read)?;
    Ok(rows.next().transpose()?)
}

/// Cut a label down to something a menu can show.
fn trim_label(label: &str) -> String {
    let single_line: String = label
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or_default()
        .trim()
        .to_string();
    if single_line.chars().count() <= LABEL_LIMIT {
        return single_line;
    }
    let kept: String = single_line.chars().take(LABEL_LIMIT - 1).collect();
    format!("{kept}…")
}

const SELECT: &str = "SELECT id, session_id, workspace_id, turn, commit_id, label, created_at \
     FROM checkpoints";
const ORDER: &str = "ORDER BY created_at DESC, turn DESC";

fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Checkpoint> {
    Ok(Checkpoint {
        id: CheckpointId(row.get(0)?),
        session: SessionId(row.get(1)?),
        workspace: WorkspaceId(row.get(2)?),
        turn: row.get(3)?,
        commit: row.get(4)?,
        label: row.get(5)?,
        created_at: row.get(6)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::session;
    use ginka_protocol::model::{Session, SessionState};

    fn stored_session(conn: &Connection) -> SessionId {
        let session = Session {
            id: SessionId("s-1".into()),
            workspace: WorkspaceId("comet/harbor".into()),
            agent: "claude".into(),
            model: None,
            state: SessionState::Running,
            summary: None,
            vendor_session_id: None,
            created_at: 0,
            updated_at: 0,
        };
        session::insert(conn, &session).unwrap();
        session.id
    }

    fn checkpoint(id: &str, turn: u32, at: i64) -> Checkpoint {
        Checkpoint {
            id: CheckpointId(id.into()),
            session: SessionId("s-1".into()),
            workspace: WorkspaceId("comet/harbor".into()),
            turn,
            commit: format!("commit-{id}"),
            label: format!("turn {turn}"),
            created_at: at,
        }
    }

    #[test]
    fn checkpoints_list_newest_first() {
        let conn = db::open_in_memory().unwrap();
        stored_session(&conn);
        insert(&conn, &checkpoint("a", 1, 100)).unwrap();
        insert(&conn, &checkpoint("b", 2, 200)).unwrap();

        let listed = list(&conn, &WorkspaceId("comet/harbor".into())).unwrap();
        assert_eq!(listed[0].id.0, "b");
        assert_eq!(
            get(&conn, &CheckpointId("a".into())).unwrap().unwrap().turn,
            1
        );
    }

    #[test]
    fn a_checkpoints_ref_is_invisible_to_the_users_branch_listing() {
        let reference = reference(&CheckpointId("abc".into()));
        assert_eq!(reference, "refs/ginka/checkpoints/abc");
        assert!(!reference.starts_with("refs/heads/"));
    }

    #[test]
    fn a_label_is_one_trimmed_line() {
        assert_eq!(
            trim_label("  Fixed the parser\nand then some  "),
            "Fixed the parser"
        );
        assert_eq!(
            trim_label("\n\nsecond line is the first real one"),
            "second line is the first real one"
        );
    }

    #[test]
    fn a_long_label_is_cut_to_something_a_menu_can_show() {
        let label = trim_label(&"x".repeat(500));
        assert_eq!(label.chars().count(), LABEL_LIMIT);
        assert!(label.ends_with('…'));
    }

    #[test]
    fn removing_a_session_takes_its_checkpoints_with_it() {
        let conn = db::open_in_memory().unwrap();
        stored_session(&conn);
        insert(&conn, &checkpoint("a", 1, 100)).unwrap();
        conn.execute("DELETE FROM sessions WHERE id = 's-1'", [])
            .unwrap();
        assert!(
            list(&conn, &WorkspaceId("comet/harbor".into()))
                .unwrap()
                .is_empty()
        );
    }
}
