//! Persistence for agent sessions and their transcripts.
//!
//! The transcript is stored as the normalized event stream rather than as
//! rendered messages: the UI folds deltas into paragraphs, and re-folding on
//! read is cheaper than losing the tool calls and reasoning that sat between
//! them. `seq` is both the order and the pagination cursor.

use anyhow::Result;
use ginka_protocol::model::{Session, SessionState, TranscriptEntry, TranscriptPayload};
use ginka_protocol::{SessionId, WorkspaceId};
use rusqlite::Connection;

/// Store a new session.
pub fn insert(conn: &Connection, session: &Session) -> Result<()> {
    conn.execute(
        "INSERT INTO sessions
            (id, workspace_id, agent, model, state, summary, vendor_session_id,
             created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            session.id.0,
            session.workspace.0,
            session.agent,
            session.model,
            session.state.as_str(),
            session.summary,
            session.vendor_session_id,
            session.created_at,
            session.updated_at,
        ],
    )?;
    Ok(())
}

/// One session by id.
pub fn get(conn: &Connection, id: &SessionId) -> Result<Option<Session>> {
    let mut statement = conn.prepare(SELECT)?;
    let mut rows = statement.query_map([&id.0], read)?;
    Ok(rows.next().transpose()?)
}

/// Sessions, most recently active first. `workspace` limits the list.
pub fn list(conn: &Connection, workspace: Option<&WorkspaceId>) -> Result<Vec<Session>> {
    match workspace {
        Some(workspace) => {
            let mut statement =
                conn.prepare(&format!("{SELECT_ALL} WHERE workspace_id = ?1 {ORDER}"))?;
            let rows = statement.query_map([&workspace.0], read)?;
            Ok(rows.collect::<Result<_, _>>()?)
        }
        None => {
            let mut statement = conn.prepare(&format!("{SELECT_ALL} {ORDER}"))?;
            let rows = statement.query_map([], read)?;
            Ok(rows.collect::<Result<_, _>>()?)
        }
    }
}

/// The session a workspace's sidebar row should show, if it has one.
pub fn latest_for_workspace(conn: &Connection, workspace: &WorkspaceId) -> Result<Option<Session>> {
    Ok(list(conn, Some(workspace))?.into_iter().next())
}

/// Move a session to a new state, touching its activity time.
///
/// `summary` is only written when it is `Some`, so a state change does not
/// erase the line the sidebar is showing.
pub fn update_state(
    conn: &Connection,
    id: &SessionId,
    state: SessionState,
    summary: Option<&str>,
    now: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE sessions
            SET state = ?1,
                summary = coalesce(?2, summary),
                updated_at = ?3
          WHERE id = ?4",
        rusqlite::params![state.as_str(), summary, now, id.0],
    )?;
    Ok(())
}

/// Record the vendor's own session id, which is what a resume is built from.
pub fn set_vendor_session_id(conn: &Connection, id: &SessionId, vendor: &str) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET vendor_session_id = ?1 WHERE id = ?2",
        rusqlite::params![vendor, id.0],
    )?;
    Ok(())
}

/// Append to a transcript, returning the position it landed at.
///
/// The sequence number is allocated from the rows already stored rather than
/// held in memory, so a daemon restart continues the numbering instead of
/// overwriting the transcript from one.
pub fn append(
    conn: &Connection,
    id: &SessionId,
    payload: &TranscriptPayload,
    at: i64,
) -> Result<u64> {
    let next: u64 = conn.query_row(
        "SELECT coalesce(max(seq), 0) + 1 FROM session_events WHERE session_id = ?1",
        [&id.0],
        |row| row.get(0),
    )?;
    conn.execute(
        "INSERT INTO session_events (session_id, seq, at, payload) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![id.0, next, at, serde_json::to_string(payload)?],
    )?;
    conn.execute(
        "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
        rusqlite::params![at, id.0],
    )?;
    Ok(next)
}

/// A page of a transcript, oldest first.
///
/// `after` pages forward from a sequence number; `limit` bounds the page. The
/// UI asks for the tail of a long transcript and pages backwards into it, so
/// an unbounded read is never on the path of opening a workspace.
pub fn transcript(
    conn: &Connection,
    id: &SessionId,
    after: Option<u64>,
    limit: Option<u32>,
) -> Result<Vec<TranscriptEntry>> {
    let mut statement = conn.prepare(
        "SELECT seq, at, payload
           FROM session_events
          WHERE session_id = ?1 AND seq > ?2
          ORDER BY seq
          LIMIT ?3",
    )?;
    let rows = statement.query_map(
        rusqlite::params![id.0, after.unwrap_or(0), limit.map(i64::from).unwrap_or(-1),],
        |row| {
            let payload: String = row.get(2)?;
            Ok(TranscriptEntry {
                seq: row.get(0)?,
                at: row.get(1)?,
                payload: serde_json::from_str(&payload).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })?,
            })
        },
    )?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Sessions the daemon thought were live when it stopped.
///
/// Their processes died with it, so on startup they are marked failed rather
/// than left looking like agents that are still working.
pub fn mark_orphans_failed(conn: &Connection, now: i64) -> Result<usize> {
    Ok(conn.execute(
        "UPDATE sessions
            SET state = 'failed',
                summary = coalesce(summary, 'the daemon stopped while this session was running'),
                updated_at = ?1
          WHERE state IN ('starting', 'running', 'awaiting_input')",
        [now],
    )?)
}

const SELECT_ALL: &str = "SELECT id, workspace_id, agent, model, state, summary, \
     vendor_session_id, created_at, updated_at FROM sessions";
const ORDER: &str = "ORDER BY updated_at DESC, created_at DESC";
const SELECT: &str = "SELECT id, workspace_id, agent, model, state, summary, \
     vendor_session_id, created_at, updated_at FROM sessions WHERE id = ?1";

fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        id: SessionId(row.get(0)?),
        workspace: WorkspaceId(row.get(1)?),
        agent: row.get(2)?,
        model: row.get(3)?,
        state: SessionState::parse(&row.get::<_, String>(4)?),
        summary: row.get(5)?,
        vendor_session_id: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use ginka_protocol::AgentEvent;

    fn session(id: &str, workspace: &str, at: i64) -> Session {
        Session {
            id: SessionId(id.into()),
            workspace: WorkspaceId(workspace.into()),
            agent: "claude".into(),
            model: Some("opus".into()),
            state: SessionState::Starting,
            summary: None,
            vendor_session_id: None,
            created_at: at,
            updated_at: at,
        }
    }

    #[test]
    fn a_session_round_trips() {
        let conn = db::open_in_memory().unwrap();
        let stored = session("s-1", "comet/harbor", 100);
        insert(&conn, &stored).unwrap();
        assert_eq!(get(&conn, &stored.id).unwrap(), Some(stored));
    }

    #[test]
    fn sessions_list_most_recently_active_first() {
        let conn = db::open_in_memory().unwrap();
        insert(&conn, &session("old", "comet/harbor", 100)).unwrap();
        insert(&conn, &session("new", "comet/harbor", 200)).unwrap();
        let listed = list(&conn, None).unwrap();
        assert_eq!(listed[0].id.0, "new");
        assert_eq!(
            latest_for_workspace(&conn, &WorkspaceId("comet/harbor".into()))
                .unwrap()
                .unwrap()
                .id
                .0,
            "new"
        );
    }

    #[test]
    fn listing_can_be_limited_to_one_workspace() {
        let conn = db::open_in_memory().unwrap();
        insert(&conn, &session("a", "comet/harbor", 100)).unwrap();
        insert(&conn, &session("b", "comet/other", 200)).unwrap();
        let listed = list(&conn, Some(&WorkspaceId("comet/harbor".into()))).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id.0, "a");
    }

    #[test]
    fn a_state_change_does_not_erase_the_summary_the_sidebar_is_showing() {
        let conn = db::open_in_memory().unwrap();
        insert(&conn, &session("s", "comet/harbor", 100)).unwrap();
        let id = SessionId("s".into());
        update_state(
            &conn,
            &id,
            SessionState::Running,
            Some("reading files"),
            110,
        )
        .unwrap();
        update_state(&conn, &id, SessionState::Idle, None, 120).unwrap();

        let stored = get(&conn, &id).unwrap().unwrap();
        assert_eq!(stored.state, SessionState::Idle);
        assert_eq!(stored.summary.as_deref(), Some("reading files"));
        assert_eq!(stored.updated_at, 120);
    }

    #[test]
    fn transcript_entries_are_numbered_from_one_and_paged_by_cursor() {
        let conn = db::open_in_memory().unwrap();
        insert(&conn, &session("s", "comet/harbor", 100)).unwrap();
        let id = SessionId("s".into());

        let first = append(
            &conn,
            &id,
            &TranscriptPayload::User {
                text: "write the test first".into(),
            },
            101,
        )
        .unwrap();
        let second = append(
            &conn,
            &id,
            &TranscriptPayload::Agent {
                event: AgentEvent::TextDelta {
                    text: "on it".into(),
                },
            },
            102,
        )
        .unwrap();
        assert_eq!((first, second), (1, 2));

        let all = transcript(&conn, &id, None, None).unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].seq, 1);
        assert_eq!(
            all[0].payload,
            TranscriptPayload::User {
                text: "write the test first".into()
            }
        );

        let after_first = transcript(&conn, &id, Some(1), None).unwrap();
        assert_eq!(after_first.len(), 1);
        assert_eq!(after_first[0].seq, 2);
    }

    #[test]
    fn a_transcript_page_can_be_bounded() {
        let conn = db::open_in_memory().unwrap();
        insert(&conn, &session("s", "comet/harbor", 100)).unwrap();
        let id = SessionId("s".into());
        for index in 0..10 {
            append(
                &conn,
                &id,
                &TranscriptPayload::User {
                    text: index.to_string(),
                },
                100 + index,
            )
            .unwrap();
        }
        assert_eq!(transcript(&conn, &id, None, Some(3)).unwrap().len(), 3);
    }

    #[test]
    fn appending_moves_the_sessions_activity_time() {
        // The sidebar sorts on this, so an agent that is producing output must
        // not look idle.
        let conn = db::open_in_memory().unwrap();
        insert(&conn, &session("s", "comet/harbor", 100)).unwrap();
        let id = SessionId("s".into());
        append(
            &conn,
            &id,
            &TranscriptPayload::Agent {
                event: AgentEvent::TextDelta { text: "…".into() },
            },
            500,
        )
        .unwrap();
        assert_eq!(get(&conn, &id).unwrap().unwrap().updated_at, 500);
    }

    #[test]
    fn a_transcript_survives_the_numbering_being_reloaded_from_storage() {
        // The daemon can restart mid-session; the next append must continue
        // the numbering rather than overwrite from one.
        let conn = db::open_in_memory().unwrap();
        insert(&conn, &session("s", "comet/harbor", 100)).unwrap();
        let id = SessionId("s".into());
        append(&conn, &id, &TranscriptPayload::User { text: "a".into() }, 1).unwrap();
        append(&conn, &id, &TranscriptPayload::User { text: "b".into() }, 2).unwrap();
        // Nothing is cached between calls, so this is the restart case.
        assert_eq!(
            append(&conn, &id, &TranscriptPayload::User { text: "c".into() }, 3).unwrap(),
            3
        );
    }

    #[test]
    fn removing_a_session_takes_its_transcript_with_it() {
        let conn = db::open_in_memory().unwrap();
        insert(&conn, &session("s", "comet/harbor", 100)).unwrap();
        let id = SessionId("s".into());
        append(&conn, &id, &TranscriptPayload::User { text: "a".into() }, 1).unwrap();
        conn.execute("DELETE FROM sessions WHERE id = 's'", [])
            .unwrap();
        let remaining: i64 = conn
            .query_row("SELECT count(*) FROM session_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(remaining, 0);
    }

    #[test]
    fn sessions_that_outlived_their_daemon_are_marked_failed_not_left_running() {
        // Their processes died with the daemon; showing them as working would
        // be a lie the user acts on.
        let conn = db::open_in_memory().unwrap();
        let mut running = session("running", "comet/harbor", 100);
        running.state = SessionState::Running;
        insert(&conn, &running).unwrap();
        let mut finished = session("finished", "comet/harbor", 100);
        finished.state = SessionState::Finished;
        insert(&conn, &finished).unwrap();

        assert_eq!(mark_orphans_failed(&conn, 300).unwrap(), 1);
        assert_eq!(
            get(&conn, &SessionId("running".into()))
                .unwrap()
                .unwrap()
                .state,
            SessionState::Failed
        );
        assert_eq!(
            get(&conn, &SessionId("finished".into()))
                .unwrap()
                .unwrap()
                .state,
            SessionState::Finished,
            "a session that had already ended is not touched"
        );
    }
}
