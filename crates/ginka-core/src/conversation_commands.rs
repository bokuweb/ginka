//! Ginka-owned conversation commands and their durable state.
//!
//! These commands are interpreted before a prompt reaches a provider so the
//! CLI, MCP, and window have the same behavior.

use anyhow::Result;
use ginka_protocol::SessionId;
use rusqlite::{Connection, OptionalExtension as _};

/// A command Ginka handles itself rather than sending to a provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command<'a> {
    /// Set or replace a durable objective.
    Goal(&'a str),
    /// Stop carrying the objective into later turns.
    ClearGoal,
    /// Ask a connected question in a separate session.
    Side(&'a str),
    /// Ask a short, read-only question in a separate session.
    Btw(&'a str),
}

/// Parse only complete command names at the beginning of a message.
pub fn parse(text: &str) -> Option<Command<'_>> {
    let text = text.trim();
    for (prefix, kind) in [("/goal", 0), ("/side", 1), ("/btw", 2)] {
        let Some(rest) = text.strip_prefix(prefix) else {
            continue;
        };
        if !rest.is_empty() && !rest.starts_with(char::is_whitespace) {
            continue;
        }
        let argument = rest.trim();
        return Some(match kind {
            0 if argument == "done" || argument == "clear" => Command::ClearGoal,
            0 => Command::Goal(argument),
            1 => Command::Side(argument),
            _ => Command::Btw(argument),
        });
    }
    None
}

/// Save or clear the objective for a session.
pub fn set_goal(conn: &Connection, session: &SessionId, objective: Option<&str>) -> Result<()> {
    conn.execute(
        "INSERT INTO session_goals (session_id, objective) VALUES (?1, ?2)
         ON CONFLICT(session_id) DO UPDATE SET objective = excluded.objective",
        rusqlite::params![session.0, objective],
    )?;
    Ok(())
}

/// Read the objective that must accompany later turns.
pub fn goal(conn: &Connection, session: &SessionId) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT objective FROM session_goals WHERE session_id = ?1",
            [&session.0],
            |row| row.get(0),
        )
        .optional()?
        .flatten())
}

/// Record which main conversation a side session belongs to.
pub fn link_side(
    conn: &Connection,
    child: &SessionId,
    parent: &SessionId,
    kind: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO side_sessions (session_id, parent_session_id, kind) VALUES (?1, ?2, ?3)",
        rusqlite::params![child.0, parent.0, kind],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_need_an_exact_leading_name() {
        assert_eq!(parse("/goals x"), None);
        assert_eq!(parse("hello /side x"), None);
        assert_eq!(parse("/goal  ship it"), Some(Command::Goal("ship it")));
        assert_eq!(parse("/goal done"), Some(Command::ClearGoal));
        assert_eq!(parse("/side why?"), Some(Command::Side("why?")));
        assert_eq!(parse("/btw why?"), Some(Command::Btw("why?")));
    }

    #[test]
    fn goal_survives_a_database_read_and_can_be_cleared() {
        let conn = crate::db::open_in_memory().unwrap();
        let id = SessionId("test".into());
        conn.execute(
            "INSERT INTO sessions (id, workspace_id, provider, created_at, updated_at)
             VALUES (?1, 'test', 'codex', 0, 0)",
            [&id.0],
        )
        .unwrap();
        set_goal(&conn, &id, Some("finish")).unwrap();
        assert_eq!(goal(&conn, &id).unwrap().as_deref(), Some("finish"));
        set_goal(&conn, &id, None).unwrap();
        assert_eq!(goal(&conn, &id).unwrap(), None);
    }
}
