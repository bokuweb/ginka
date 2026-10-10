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

pub mod turns;
pub mod undo;

use anyhow::Result;
use ginka_protocol::model::Checkpoint;
use ginka_protocol::{CheckpointId, SessionId, WorkspaceId};
use rusqlite::Connection;
use std::path::Path;
pub use turns::{Checkpoints, TurnId, TurnStart};

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

/// Capture the state a turn is about to be handed.
///
/// Called before the agent runs, which is the whole point: a file the user
/// edited in the terminal between turns is part of what the agent was *given*,
/// not part of what it did, and only a snapshot taken at this moment can tell
/// the two apart (`docs/roadmap.md` §3.3 N8). Failing to take it is logged by
/// the caller and the turn carries on — losing the ability to attribute a diff
/// is not a reason to stop an agent.
pub fn begin(workspace_path: &Path, session: &SessionId, turn: u32) -> Result<TurnStart> {
    Checkpoints::new(workspace_path).capture_turn_start(&turn_id(session, turn))
}

/// The turn a checkpoint belongs to, in the ref namespace's own terms.
pub fn turn_id(session: &SessionId, turn: u32) -> TurnId {
    TurnId::new(&session.0, turn as usize)
}

/// Which turn of which session, in which workspace, a checkpoint belongs to.
///
/// Grouped rather than passed as three positional arguments: they travel
/// together everywhere and a call site reads better naming them than counting
/// them.
#[derive(Debug, Clone, Copy)]
pub struct TurnRef<'a> {
    /// The workspace whose worktree was snapshotted.
    pub workspace: &'a WorkspaceId,
    /// The Ginka session the turn belongs to.
    pub session: &'a SessionId,
    /// The turn's number within the session.
    pub turn: u32,
}

/// Snapshot a worktree and record the checkpoint.
pub fn take(
    conn: &Connection,
    workspace_path: &Path,
    at: TurnRef<'_>,
    label: &str,
    start: Option<&TurnStart>,
    now: i64,
) -> Result<Checkpoint> {
    let TurnRef {
        workspace,
        session,
        turn,
    } = at;
    let mut checkpoint = snapshot(
        workspace_path,
        TurnRef {
            workspace,
            session,
            turn,
        },
        label,
        now,
    )?;
    checkpoint.has_turn_start = start.is_some();
    insert(conn, &checkpoint)?;
    if let Some(start) = start {
        // Written beside the ending commit rather than instead of it: the pair
        // is what makes "what did this turn change" answerable without
        // attributing a hand edit made between turns to the agent (§3.3 N8).
        conn.execute(
            "UPDATE checkpoints SET start_commit = ?2, base_commit = ?3 WHERE id = ?1",
            rusqlite::params![checkpoint.id.0, start.commit, start.base_commit],
        )?;
        if let Some(state) =
            Checkpoints::new(workspace_path).capture_undo(start, &checkpoint.commit)?
        {
            Checkpoints::new(workspace_path).keep_undo(&undo_reference(&checkpoint.id), &state)?;
            conn.execute(
                "UPDATE checkpoints SET undo_state = ?2 WHERE id = ?1",
                rusqlite::params![checkpoint.id.0, serde_json::to_string(&state)?],
            )?;
            checkpoint.can_undo = true;
        }
    }
    Ok(checkpoint)
}

/// The extra reachability root retaining an undo's staged snapshots.
pub fn undo_reference(id: &CheckpointId) -> String {
    format!("refs/ginka/undo/{}", id.0)
}

/// Durable undo evidence, absent for older or unsupported checkpoints.
pub fn undo_state(conn: &Connection, id: &CheckpointId) -> Result<Option<undo::UndoState>> {
    let json: Option<String> = conn.query_row(
        "SELECT undo_state FROM checkpoints WHERE id = ?1",
        [&id.0],
        |row| row.get(0),
    )?;
    json.map(|json| serde_json::from_str(&json))
        .transpose()
        .map_err(Into::into)
}

/// Snapshot the worktree into a checkpoint without storing it: the git half
/// of [`take`], which can run without the database (and so without the
/// daemon's service held). [`insert`] stores it afterwards.
pub fn snapshot(
    workspace_path: &Path,
    at: TurnRef<'_>,
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
    Ok(Checkpoint {
        id,
        session: at.session.clone(),
        workspace: at.workspace.clone(),
        turn: at.turn,
        commit,
        label,
        has_turn_start: false,
        can_undo: false,
        created_at: now,
    })
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

/// The start and end snapshots of one completed turn, if its start was saved.
/// Older checkpoints without a start cannot identify the turn's own changes.
pub fn turn_commits(conn: &Connection, id: &CheckpointId) -> Result<Option<(String, String)>> {
    let pair = conn.query_row(
        "SELECT start_commit, commit_id FROM checkpoints WHERE id = ?1",
        [&id.0],
        |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?)),
    );
    match pair {
        Ok((Some(start), end)) => Ok(Some((start, end))),
        Ok((None, _)) | Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Drop everything but the newest `keep` checkpoints in a workspace.
///
/// Every checkpoint holds a commit alive through a ref, so a workspace an
/// agent has worked in for a week keeps a week of trees that git would
/// otherwise collect. Rewinding is something done to recent work; past this
/// the reader is reading history rather than undoing it.
///
/// The ref goes first and the row second: a row with no ref is a rewind that
/// fails, and a ref with no row is an object nothing will ever collect. Both
/// are bad, and only the second is invisible.
pub fn prune(
    conn: &Connection,
    workspace_path: &Path,
    workspace: &WorkspaceId,
    keep: u32,
) -> Result<usize> {
    let all = list(conn, workspace)?;
    let stale = all.into_iter().skip(keep as usize);
    let mut dropped = 0;
    for checkpoint in stale {
        // Best-effort: a ref that is already gone -- a worktree removed by
        // hand, a repository re-cloned -- must not stop the row going with it.
        if let Err(error) = git::drop_snapshot(workspace_path, &reference(&checkpoint.id)) {
            tracing::debug!(%error, id = checkpoint.id.0, "no ref to drop for this checkpoint");
        }
        let _ = git::drop_snapshot(workspace_path, &undo_reference(&checkpoint.id));
        conn.execute("DELETE FROM checkpoints WHERE id = ?1", [&checkpoint.id.0])?;
        dropped += 1;
    }
    Ok(dropped)
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

const SELECT: &str = "SELECT id, session_id, workspace_id, turn, commit_id, label, created_at, start_commit IS NOT NULL, undo_state IS NOT NULL \
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
        has_turn_start: row.get(7)?,
        can_undo: row.get(8)?,
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
            account: ginka_protocol::AccountId("claude".into()),
            model: None,
            reasoning_effort: None,
            service_tier: None,
            state: SessionState::Running,
            title: None,
            summary: None,
            vendor_session_id: None,
            access_mode: Default::default(),
            origin: None,
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
            has_turn_start: false,
            can_undo: false,
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
    fn a_turn_diff_requires_its_saved_start() {
        let conn = db::open_in_memory().unwrap();
        stored_session(&conn);
        let id = CheckpointId("a".into());
        insert(&conn, &checkpoint("a", 1, 100)).unwrap();
        assert!(!list(&conn, &WorkspaceId("comet/harbor".into())).unwrap()[0].has_turn_start);
        assert_eq!(turn_commits(&conn, &id).unwrap(), None);
        conn.execute(
            "UPDATE checkpoints SET start_commit = 'start-a' WHERE id = 'a'",
            [],
        )
        .unwrap();
        assert_eq!(
            turn_commits(&conn, &id).unwrap(),
            Some(("start-a".into(), "commit-a".into()))
        );
        assert!(list(&conn, &WorkspaceId("comet/harbor".into())).unwrap()[0].has_turn_start);
        assert_eq!(
            turn_commits(&conn, &CheckpointId("missing".into())).unwrap(),
            None
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

    #[test]
    fn pruning_keeps_the_newest_and_drops_the_refs_of_the_rest() {
        // Every checkpoint holds a commit alive, so a workspace worked in for
        // a week keeps a week of trees git would otherwise collect.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("repo");
        crate::git::tests::repository(&root);
        let conn = db::open_in_memory().unwrap();
        stored_session(&conn);
        let workspace = WorkspaceId("comet/harbor".into());

        for (index, id) in ["a", "b", "c"].iter().enumerate() {
            std::fs::write(root.join("work.txt"), format!("turn {index}\n")).unwrap();
            let taken = take(
                &conn,
                &root,
                TurnRef {
                    workspace: &workspace,
                    session: &SessionId("s-1".into()),
                    turn: index as u32,
                },
                id,
                None,
                100 + index as i64,
            )
            .unwrap();
            assert!(crate::git::head_commit(&root).is_some());
            assert_ne!(taken.commit, "");
        }

        assert_eq!(prune(&conn, &root, &workspace, 2).unwrap(), 1);
        let left = list(&conn, &workspace).unwrap();
        assert_eq!(left.len(), 2, "the newest two are what a rewind reaches");
        assert_eq!(left[0].label, "c");
        assert_eq!(left[1].label, "b");
    }

    #[test]
    fn pruning_a_workspace_that_is_within_its_limit_does_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let conn = db::open_in_memory().unwrap();
        stored_session(&conn);
        insert(&conn, &checkpoint("a", 1, 100)).unwrap();

        let workspace = WorkspaceId("comet/harbor".into());
        assert_eq!(prune(&conn, dir.path(), &workspace, 10).unwrap(), 0);
        assert_eq!(list(&conn, &workspace).unwrap().len(), 1);
    }
}
