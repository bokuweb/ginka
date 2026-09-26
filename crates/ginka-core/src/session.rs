//! Persistence for agent sessions and their transcripts.
//!
//! The transcript is stored as the normalized event stream rather than as
//! rendered messages: the UI folds deltas into paragraphs, and re-folding on
//! read is cheaper than losing the tool calls and reasoning that sat between
//! them. `seq` is both the order and the pagination cursor.

use anyhow::Result;
use ginka_protocol::model::{
    Session, SessionMatch, SessionOrigin, SessionState, TranscriptEntry, TranscriptPayload,
};
use ginka_protocol::provider::AccessMode;
use ginka_protocol::{SessionId, WorkspaceId};
use rusqlite::{Connection, OptionalExtension as _};

/// Store a new session.
pub fn insert(conn: &Connection, session: &Session) -> Result<()> {
    conn.execute(
        "INSERT INTO sessions
            (id, workspace_id, provider, account_id, model, reasoning_effort,
             service_tier, state, agent_title,
             agent_title_is_placeholder, summary, vendor_session_id,
             created_at, updated_at, access_mode,
             origin_connector, origin_channel, origin_thread)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
        rusqlite::params![
            session.id.0,
            session.workspace.0,
            session.agent,
            session.account.0,
            session.model,
            session.reasoning_effort,
            session.service_tier,
            session.state.as_str(),
            session.title,
            session.summary,
            session.vendor_session_id,
            session.created_at,
            session.updated_at,
            access_mode_str(session.access_mode),
            session
                .origin
                .as_ref()
                .map(|origin| origin.connector.clone()),
            session.origin.as_ref().map(|origin| origin.channel.clone()),
            session.origin.as_ref().map(|origin| origin.thread.clone()),
        ],
    )?;
    Ok(())
}

/// Replace the provider options used by later turns of a session.
pub fn update_provider_options(
    conn: &Connection,
    id: &SessionId,
    model: Option<&str>,
    reasoning_effort: Option<&str>,
    service_tier: Option<&str>,
    now: i64,
) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE sessions SET model = ?2, reasoning_effort = ?3, service_tier = ?4, updated_at = ?5 WHERE id = ?1",
        rusqlite::params![id.0, model, reasoning_effort, service_tier, now],
    )? > 0)
}

/// The session still answering a thread, if any.
///
/// Only an *active* origin counts: a thread told to start fresh keeps its
/// old session, chip and all, but a reply there finds the new one.
pub fn find_by_origin(conn: &Connection, origin: &SessionOrigin) -> Result<Option<Session>> {
    let mut statement = conn.prepare(&format!(
        "{SELECT_ALL} WHERE origin_connector = ?1 AND origin_channel = ?2
                        AND origin_thread = ?3 AND origin_active = 1"
    ))?;
    let mut rows = statement.query_map(
        rusqlite::params![origin.connector, origin.channel, origin.thread],
        read,
    )?;
    Ok(rows.next().transpose()?)
}

/// Stop a session answering the thread that started it.
///
/// The origin stays on the record — it is where the conversation came from,
/// and the sidebar still says so — but the next message in that thread
/// starts a new session. Answers whether there was such a session.
pub fn close_origin(conn: &Connection, id: &SessionId) -> Result<bool> {
    let updated = conn.execute(
        "UPDATE sessions SET origin_active = 0 WHERE id = ?1 AND origin_connector IS NOT NULL",
        [&id.0],
    )?;
    Ok(updated > 0)
}

/// The access mode as it is stored.
fn access_mode_str(mode: AccessMode) -> &'static str {
    match mode {
        AccessMode::ReadOnly => "read_only",
        AccessMode::Ask => "ask",
        AccessMode::Auto => "auto",
    }
}

/// An access mode read back. Anything unrecognised is the safe default.
fn parse_access_mode(text: &str) -> AccessMode {
    match text {
        "read_only" => AccessMode::ReadOnly,
        "auto" => AccessMode::Auto,
        _ => AccessMode::Ask,
    }
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
    let mut statement = conn.prepare(&format!(
        "{SELECT_ALL} WHERE workspace_id = ?1 ORDER BY created_at DESC, updated_at DESC LIMIT 1"
    ))?;
    let mut rows = statement.query_map([&workspace.0], read)?;
    Ok(rows.next().transpose()?)
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

/// Mark a session active now, whatever its transcript's dates say — an
/// imported transcript carries the vendor's older times.
pub fn touch(conn: &Connection, id: &SessionId, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
        rusqlite::params![now, id.0],
    )?;
    Ok(())
}

/// Every vendor session id a session holds: the conversations Ginka already
/// has, whether it started them or adopted them from a CLI.
pub fn vendor_ids(conn: &Connection) -> Result<std::collections::HashSet<String>> {
    let mut statement =
        conn.prepare("SELECT vendor_session_id FROM sessions WHERE vendor_session_id IS NOT NULL")?;
    let ids = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

/// Record the vendor's own session id, which is what a resume is built from.
pub fn set_vendor_session_id(conn: &Connection, id: &SessionId, vendor: &str) -> Result<()> {
    // A thread of its own is what a handoff was for: once the agent has one,
    // the digest has been read and is not sent again.
    conn.execute(
        "UPDATE sessions SET vendor_session_id = ?1, handoff = NULL WHERE id = ?2",
        rusqlite::params![vendor, id.0],
    )?;
    Ok(())
}

/// Keep what a session moved from another agent has to be told first.
///
/// Read back by [`handoff`] when its first turn starts, and cleared by
/// [`set_vendor_session_id`] once the agent has a thread of its own — so a
/// first turn that failed before connecting is retried with the digest intact.
pub fn set_handoff(conn: &Connection, id: &SessionId, digest: &str) -> Result<()> {
    conn.execute(
        "UPDATE sessions SET handoff = ?1 WHERE id = ?2",
        rusqlite::params![digest, id.0],
    )?;
    Ok(())
}

/// The digest a session still owes its agent, if any.
pub fn handoff(conn: &Connection, id: &SessionId) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT handoff FROM sessions WHERE id = ?1",
            [&id.0],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
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
/// The last `limit` entries before position `before` (or the end), oldest
/// first: a page read backwards from where the reader is.
pub fn transcript_tail(
    conn: &Connection,
    id: &SessionId,
    before: Option<u64>,
    limit: u32,
) -> Result<Vec<TranscriptEntry>> {
    let mut statement = conn.prepare(
        "SELECT seq, at, payload
           FROM session_events
          WHERE session_id = ?1 AND seq < ?2
          ORDER BY seq DESC
          LIMIT ?3",
    )?;
    let rows = statement.query_map(
        rusqlite::params![id.0, before.map_or(i64::MAX, |before| before as i64), limit],
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
    let mut entries = rows.collect::<rusqlite::Result<Vec<_>>>()?;
    entries.reverse();
    Ok(entries)
}

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

/// Rename a session. Returns whether there was one to rename.
/// Rename a session.
///
/// Writes the *user's* title column, which is the one that wins: an agent
/// naming its own session later must not overwrite a name the user typed
/// (`docs/roadmap.md` §3.3 N5).
pub fn rename(conn: &Connection, id: &SessionId, title: &str) -> Result<bool> {
    let title = title.trim();
    let updated = conn.execute(
        "UPDATE sessions SET user_title = ?1 WHERE id = ?2",
        rusqlite::params![(!title.is_empty()).then_some(title), id.0],
    )?;
    Ok(updated > 0)
}

/// Forget a session and everything hanging off it.
pub fn remove(conn: &Connection, id: &SessionId) -> Result<bool> {
    let removed = conn.execute("DELETE FROM sessions WHERE id = ?1", [&id.0])?;
    Ok(removed > 0)
}

/// Copy a session's transcript up to `after` into `into`.
///
/// This is what forking a conversation is: the record up to a point, kept, so
/// the same work can be taken somewhere else without losing where it came
/// from. The positions are renumbered from one, because a transcript's `seq`
/// is its own and a client pages by it.
pub fn copy_transcript(
    conn: &Connection,
    from: &SessionId,
    into: &SessionId,
    after: Option<u64>,
) -> Result<u64> {
    let entries = transcript(conn, from, None, None)?;
    let mut copied = 0;
    for entry in entries {
        if after.is_some_and(|limit| entry.seq > limit) {
            break;
        }
        append(conn, into, &entry.payload, entry.at)?;
        copied += 1;
    }
    Ok(copied)
}

/// Find transcript entries containing `query`.
///
/// A `LIKE` over the stored JSON rather than a full-text index: the payload is
/// one JSON object per entry, the corpus is one user's own conversations, and
/// an index that has to be kept in step with an append-only log is a second
/// thing to get wrong. If this becomes slow, it becomes FTS5.
pub fn search(
    conn: &Connection,
    workspace: Option<&WorkspaceId>,
    query: &str,
    limit: u32,
) -> Result<Vec<SessionMatch>> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let pattern = format!("%{}%", query.replace('%', "\\%").replace('_', "\\_"));

    let mut statement = conn.prepare(
        "SELECT e.session_id, s.workspace_id, COALESCE(s.user_title, s.agent_title),
                  e.seq, e.at, e.payload
           FROM session_events e
           JOIN sessions s ON s.id = e.session_id
          WHERE e.payload LIKE ?1 ESCAPE '\\'
            AND (?2 IS NULL OR s.workspace_id = ?2)
          ORDER BY e.at DESC
          LIMIT ?3",
    )?;
    let rows = statement.query_map(
        rusqlite::params![pattern, workspace.map(|id| id.0.clone()), limit],
        |row| {
            let payload: String = row.get(5)?;
            Ok(SessionMatch {
                session: SessionId(row.get(0)?),
                workspace: WorkspaceId(row.get(1)?),
                title: row.get(2)?,
                seq: row.get(3)?,
                at: row.get(4)?,
                excerpt: String::new(),
            })
            .map(|mut found: SessionMatch| {
                found.excerpt = excerpt(&payload, query);
                found
            })
        },
    )?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// The part of a stored entry worth showing beside a match.
///
/// The text of the entry, not the JSON it is stored in: a result reading
/// `{"source":"agent","event":{"kind":"text_delta"` tells the reader nothing
/// about what was said.
fn excerpt(payload: &str, query: &str) -> String {
    let text = serde_json::from_str::<TranscriptPayload>(payload)
        .ok()
        .map(|payload| match payload {
            TranscriptPayload::User { text } => text,
            TranscriptPayload::Response { text, .. } => text,
            TranscriptPayload::Agent { event } => match event {
                ginka_protocol::AgentEvent::TextDelta { text }
                | ginka_protocol::AgentEvent::Reasoning { text } => text,
                ginka_protocol::AgentEvent::ToolCall { activity }
                | ginka_protocol::AgentEvent::ToolResult { activity }
                    if activity.tasks.is_some() =>
                {
                    activity
                        .tasks
                        .as_deref()
                        .unwrap_or_default()
                        .iter()
                        .map(|task| task.label.as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                }
                ginka_protocol::AgentEvent::ToolResult { activity } => {
                    activity.detail.clone().unwrap_or_default()
                }
                other => format!("{other:?}"),
            },
        })
        .unwrap_or_else(|| payload.to_string());

    // Centred on the match where there is one, so the reader sees why this is
    // a result rather than the first sentence of something long.
    let found = text.to_lowercase().find(&query.to_lowercase()).unwrap_or(0);
    let start = text[..found]
        .char_indices()
        .rev()
        .nth(40)
        .map(|(at, _)| at)
        .unwrap_or(0);
    let kept: String = text[start..].chars().take(120).collect();
    let head = if start > 0 { "…" } else { "" };
    let tail = if text[start..].chars().count() > 120 {
        "…"
    } else {
        ""
    };
    format!("{head}{}{tail}", kept.replace('\n', " "))
}

/// What the user was in the middle of typing in a workspace.
pub fn draft(conn: &Connection, workspace: &WorkspaceId) -> Result<String> {
    let text: Option<String> = conn
        .query_row(
            "SELECT text FROM composer_drafts WHERE workspace_id = ?1",
            [&workspace.0],
            |row| row.get(0),
        )
        .optional()?;
    Ok(text.unwrap_or_default())
}

/// Keep a draft, or forget it once it has been sent.
///
/// An empty draft is a deletion rather than an empty row: "nothing is being
/// written here" and "a draft of nothing" are the same state, and storing both
/// invites them to disagree.
pub fn set_draft(conn: &Connection, workspace: &WorkspaceId, text: &str, now: i64) -> Result<()> {
    if text.trim().is_empty() {
        conn.execute(
            "DELETE FROM composer_drafts WHERE workspace_id = ?1",
            [&workspace.0],
        )?;
        return Ok(());
    }
    conn.execute(
        "INSERT INTO composer_drafts (workspace_id, text, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(workspace_id) DO UPDATE SET text = excluded.text,
                                                 updated_at = excluded.updated_at",
        rusqlite::params![workspace.0, text, now],
    )?;
    Ok(())
}

/// How many turns a session has already completed.
///
/// Each turn is its own process, so a driver's own counter restarts every
/// time; the session's count is the one the transcript and the checkpoints are
/// numbered by. Counted from the stored events rather than held in memory,
/// because a resume may be the first thing a restarted daemon does.
pub fn turns_completed(conn: &Connection, id: &SessionId) -> Result<u32> {
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM session_events
          WHERE session_id = ?1 AND json_extract(payload, '$.event.kind') = 'turn_end'",
        [&id.0],
        |row| row.get(0),
    )?;
    Ok(count as u32)
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

const SELECT_ALL: &str = "SELECT id, workspace_id, provider, model, reasoning_effort, service_tier, state, \
     COALESCE(user_title, agent_title), summary, \
     vendor_session_id, created_at, updated_at, account_id, access_mode, \
     origin_connector, origin_channel, origin_thread FROM sessions";
const ORDER: &str = "ORDER BY updated_at DESC, created_at DESC";
const SELECT: &str = "SELECT id, workspace_id, provider, model, reasoning_effort, service_tier, state, \
     COALESCE(user_title, agent_title), summary, \
     vendor_session_id, created_at, updated_at, account_id, access_mode, \
     origin_connector, origin_channel, origin_thread FROM sessions WHERE id = ?1";

fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    let origin = match row.get::<_, Option<String>>(14)? {
        Some(connector) => Some(SessionOrigin {
            connector,
            channel: row.get::<_, Option<String>>(15)?.unwrap_or_default(),
            thread: row.get::<_, Option<String>>(16)?.unwrap_or_default(),
        }),
        None => None,
    };
    Ok(Session {
        id: SessionId(row.get(0)?),
        workspace: WorkspaceId(row.get(1)?),
        agent: row.get(2)?,
        account: ginka_protocol::AccountId(row.get(12)?),
        model: row.get(3)?,
        reasoning_effort: row.get(4)?,
        service_tier: row.get(5)?,
        state: SessionState::parse(&row.get::<_, String>(6)?),
        title: row.get(7)?,
        summary: row.get(8)?,
        vendor_session_id: row.get(9)?,
        access_mode: parse_access_mode(&row.get::<_, String>(13)?),
        origin,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use ginka_protocol::AgentEvent;
    use ginka_protocol::event::ActivityItem;

    fn session(id: &str, workspace: &str, at: i64) -> Session {
        Session {
            id: SessionId(id.into()),
            workspace: WorkspaceId(workspace.into()),
            agent: "claude".into(),
            account: ginka_protocol::AccountId("claude".into()),
            model: Some("opus".into()),
            reasoning_effort: None,
            service_tier: None,
            state: SessionState::Starting,
            title: None,
            summary: None,
            vendor_session_id: None,
            access_mode: AccessMode::Ask,
            origin: None,
            created_at: at,
            updated_at: at,
        }
    }

    fn origin(thread: &str) -> SessionOrigin {
        SessionOrigin {
            connector: "slack".into(),
            channel: "C1".into(),
            thread: thread.into(),
        }
    }

    #[test]
    fn a_thread_finds_its_session_until_it_is_told_to_start_fresh() {
        let conn = db::open_in_memory().unwrap();
        let mut from_slack = session("s-1", "comet/harbor", 100);
        from_slack.origin = Some(origin("1.0"));
        from_slack.access_mode = AccessMode::Auto;
        insert(&conn, &from_slack).unwrap();
        insert(&conn, &session("s-2", "comet/harbor", 100)).unwrap();

        let found = find_by_origin(&conn, &origin("1.0"))
            .unwrap()
            .expect("found");
        assert_eq!(found.id.0, "s-1");
        assert_eq!(found.access_mode, AccessMode::Auto);
        assert_eq!(found.origin, Some(origin("1.0")));
        assert!(find_by_origin(&conn, &origin("2.0")).unwrap().is_none());

        // Two sessions cannot both answer one thread.
        let mut twin = session("s-3", "comet/harbor", 100);
        twin.origin = Some(origin("1.0"));
        assert!(insert(&conn, &twin).is_err());

        assert!(close_origin(&conn, &SessionId("s-1".into())).unwrap());
        assert!(find_by_origin(&conn, &origin("1.0")).unwrap().is_none());
        // The record still says where it came from.
        assert_eq!(
            get(&conn, &SessionId("s-1".into()))
                .unwrap()
                .unwrap()
                .origin,
            Some(origin("1.0"))
        );
        // And the thread can be answered by a new session now.
        insert(&conn, &twin).unwrap();
        assert!(
            !close_origin(&conn, &SessionId("s-2".into())).unwrap(),
            "a session with no origin has nothing to close"
        );
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
    fn a_workspace_keeps_showing_its_newest_conversation_when_an_old_one_finishes_late() {
        let conn = db::open_in_memory().unwrap();
        insert(&conn, &session("old", "comet/harbor", 100)).unwrap();
        insert(&conn, &session("replacement", "comet/harbor", 200)).unwrap();
        update_state(
            &conn,
            &SessionId("old".into()),
            SessionState::Cancelled,
            None,
            300,
        )
        .unwrap();

        assert_eq!(list(&conn, None).unwrap()[0].id.0, "old");
        assert_eq!(
            latest_for_workspace(&conn, &WorkspaceId("comet/harbor".into()))
                .unwrap()
                .unwrap()
                .id
                .0,
            "replacement"
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
    fn persisted_task_labels_are_searchable_with_a_readable_excerpt() {
        let conn = db::open_in_memory().unwrap();
        insert(&conn, &session("s", "comet/harbor", 100)).unwrap();
        let id = SessionId("s".into());
        let mut activity = ActivityItem::from_tool(
            Some("tasks-1".into()),
            "TodoWrite",
            &serde_json::json!({
                "todos": [
                    {"content": "Inspect transport", "status": "completed"},
                    {"content": "Verify websocket reconnect", "status": "in_progress"}
                ]
            }),
        );
        activity.complete_with("", false);
        append(
            &conn,
            &id,
            &TranscriptPayload::Agent {
                event: AgentEvent::ToolResult { activity },
            },
            101,
        )
        .unwrap();

        let matches = search(&conn, None, "websocket reconnect", 10).unwrap();

        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].seq, 1);
        assert!(
            matches[0].excerpt.contains("Verify websocket reconnect"),
            "task labels, not empty tool output, explain the match: {}",
            matches[0].excerpt
        );
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
    fn a_half_written_prompt_survives_being_left() {
        // Losing it to a restart is the kind of small betrayal that stops
        // people trusting a tool with anything long.
        let conn = db::open_in_memory().unwrap();
        let workspace = WorkspaceId("comet/harbor".into());
        assert_eq!(draft(&conn, &workspace).unwrap(), "");

        set_draft(&conn, &workspace, "the beginning of a thought", 100).unwrap();
        assert_eq!(
            draft(&conn, &workspace).unwrap(),
            "the beginning of a thought"
        );

        set_draft(&conn, &workspace, "changed my mind", 200).unwrap();
        assert_eq!(draft(&conn, &workspace).unwrap(), "changed my mind");
    }

    #[test]
    fn sending_what_was_drafted_leaves_nothing_behind() {
        // "Nothing is being written here" and "a draft of nothing" are the
        // same state; storing both invites them to disagree.
        let conn = db::open_in_memory().unwrap();
        let workspace = WorkspaceId("comet/harbor".into());
        set_draft(&conn, &workspace, "sent", 100).unwrap();
        set_draft(&conn, &workspace, "   ", 200).unwrap();
        assert_eq!(draft(&conn, &workspace).unwrap(), "");

        let rows: i64 = conn
            .query_row("SELECT count(*) FROM composer_drafts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn drafts_are_kept_apart_by_workspace() {
        let conn = db::open_in_memory().unwrap();
        let one = WorkspaceId("comet/one".into());
        let two = WorkspaceId("comet/two".into());
        set_draft(&conn, &one, "for one", 1).unwrap();
        set_draft(&conn, &two, "for two", 1).unwrap();
        assert_eq!(draft(&conn, &one).unwrap(), "for one");
        assert_eq!(draft(&conn, &two).unwrap(), "for two");
    }

    #[test]
    fn completed_turns_are_counted_from_the_transcript() {
        let conn = db::open_in_memory().unwrap();
        insert(&conn, &session("s", "comet/harbor", 100)).unwrap();
        let id = SessionId("s".into());
        assert_eq!(turns_completed(&conn, &id).unwrap(), 0);

        for (at, event) in [
            (1, AgentEvent::TextDelta { text: "hi".into() }),
            (2, AgentEvent::TurnEnd { turn: 1 }),
            (
                3,
                AgentEvent::TextDelta {
                    text: "again".into(),
                },
            ),
            (4, AgentEvent::TurnEnd { turn: 1 }),
        ] {
            append(&conn, &id, &TranscriptPayload::Agent { event }, at).unwrap();
        }
        // Two turns ran, even though the second process numbered its own as 1.
        assert_eq!(turns_completed(&conn, &id).unwrap(), 2);
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
